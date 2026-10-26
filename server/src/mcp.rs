//! MCP (Model Context Protocol) server over stdio.
//!
//! `kenji mcp --url <base> --token <api-key|jwt>` lets AI agents operate the
//! platform through the same REST API used by the Web UI — with the token's
//! role (viewer/operator/admin), workspace sandboxing and audit logging.
//!
//! Protocol: newline-delimited JSON-RPC 2.0 on stdin/stdout, per the MCP stdio
//! transport. All diagnostics go to stderr so stdout stays protocol-clean.

use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::sync::LazyLock;

use crate::files;

// ---------------------------------------------------------------------------
// API client
// ---------------------------------------------------------------------------

pub struct Api {
    base: String,
    token: String,
}

#[derive(Debug)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    fn msg(m: impl Into<String>) -> ToolError {
        ToolError { message: m.into() }
    }
}

enum ToolOutput {
    Json(Value),
    Text(String),
}

impl Api {
    pub fn new(base: String, token: String) -> Api {
        Api { base, token }
    }

    fn q(path: &str, query: &str) -> String {
        if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        }
    }

    fn enc(s: &str) -> String {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        query: &str,
        body: Option<Value>,
    ) -> Result<Value, ToolError> {
        let url = format!("{}{}", self.base, Self::q(path, query));
        let mut req = match method {
            "GET" => ureq::get(&url),
            "POST" => ureq::post(&url),
            "PUT" => ureq::put(&url),
            "DELETE" => ureq::delete(&url),
            _ => return Err(ToolError::msg(format!("unsupported method {method}"))),
        };
        req = req.set("Authorization", &format!("Bearer {}", self.token));
        let resp = if let Some(b) = body {
            req.set("Content-Type", "application/json").send_json(b)
        } else {
            req.call()
        };
        match resp {
            Ok(r) => {
                if r.status() >= 200 && r.status() < 300 {
                    r.into_json().or(Ok(Value::Null))
                } else {
                    Err(ToolError::msg(format!("HTTP {}", r.status())))
                }
            }
            Err(ureq::Error::Status(code, r)) => {
                let msg = r
                    .into_string()
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .and_then(|v| v["error"]["message"].as_str().map(String::from))
                    .unwrap_or_else(|| format!("HTTP {code}"));
                Err(ToolError::msg(format!("HTTP {code}: {msg}")))
            }
            Err(e) => Err(ToolError::msg(format!("connection error: {e}"))),
        }
    }

    fn get(&self, path: &str, query: &str) -> Result<Value, ToolError> {
        self.request("GET", path, query, None)
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, ToolError> {
        self.request("POST", path, "", Some(body))
    }

    fn delete(&self, path: &str, query: &str) -> Result<Value, ToolError> {
        self.request("DELETE", path, query, None)
    }

    /// Download a file as (lossy) UTF-8 text.
    fn download_text(
        &self,
        path: &str,
        max_bytes: usize,
        scope: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut url = format!("{}/api/files/download?path={}", self.base, Self::enc(path));
        if let Some(scope) = scope {
            url.push_str(&format!("&scope={}", Self::enc(scope)));
        }
        let resp = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call()
            .map_err(|e| ToolError::msg(format!("download failed: {e}")))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(|e| ToolError::msg(format!("read failed: {e}")))?;
        let truncated = buf.len() > max_bytes;
        if truncated {
            buf.truncate(max_bytes);
        }
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if truncated {
            text.push_str(&format!("\n...[truncated at {max_bytes} bytes]"));
        }
        Ok(text)
    }

    /// Multipart upload (matches the /api/files/upload contract).
    fn upload(
        &self,
        dir: &str,
        filename: &str,
        bytes: &[u8],
        scope: Option<&str>,
    ) -> Result<Value, ToolError> {
        let boundary = format!("kenji-mcp-{}", uuid::Uuid::new_v4().simple());
        let mut url = format!("{}/api/files/upload?path={}", self.base, Self::enc(dir));
        if let Some(scope) = scope {
            url.push_str(&format!("&scope={}", Self::enc(scope)));
        }
        let mut body: Vec<u8> = Vec::new();
        let head = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{f}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
            b = boundary,
            f = filename
        );
        body.extend_from_slice(head.as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{b}--\r\n", b = boundary).as_bytes());

        let req = ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            );
        match req.send_bytes(&body) {
            Ok(r) => r.into_json().or(Ok(Value::Null)),
            Err(ureq::Error::Status(code, r)) => {
                let msg = r
                    .into_string()
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .and_then(|v| v["error"]["message"].as_str().map(String::from))
                    .unwrap_or_else(|| format!("HTTP {code}"));
                Err(ToolError::msg(format!("HTTP {code}: {msg}")))
            }
            Err(e) => Err(ToolError::msg(format!("upload failed: {e}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool registry
// ---------------------------------------------------------------------------

struct ToolDef {
    name: &'static str,
    description: &'static str,
    schema: Value,
    handler: fn(&Api, &Value) -> Result<ToolOutput, ToolError>,
}

fn tools() -> &'static [ToolDef] {
    static TOOLS: LazyLock<Vec<ToolDef>> = LazyLock::new(|| {
        vec![
            ToolDef {
                name: "system_overview",
                description: "Get a full system overview: host/OS info, CPU, memory, disk and GPU metrics, plus a summary of running jobs. Start here to orient yourself.",
                schema: json!({"type":"object","properties":{},"additionalProperties":false}),
                handler: h_system_overview,
            },
            ToolDef {
                name: "jobs_list",
                description: "List all jobs with their status, restart policy and schedule.",
                schema: json!({"type":"object","properties":{},"additionalProperties":false}),
                handler: h_jobs_list,
            },
            ToolDef {
                name: "job_get",
                description: "Get one job's details: status, restart count, last error, exit code.",
                schema: json!({
                    "type":"object",
                    "properties":{"id":{"type":"string","description":"Job ID (UUID)"}},
                    "required":["id"],"additionalProperties":false
                }),
                handler: h_job_get,
            },
            ToolDef {
                name: "job_run",
                description: "Create a job — it starts immediately (server semantics; scheduled jobs also arm their cron timer). Use job_stop to halt it, job_delete to remove it. For a quick one-off command prefer run_command.",
                schema: json!({
                    "type":"object",
                    "properties": {
                        "name": {"type":"string","description":"Job name","default":"mcp-job"},
                        "command": {"type":"string","description":"Executable to run"},
                        "args": {"type":"array","items":{"type":"string"},"description":"Command arguments"},
                        "cwd": {"type":"string","description":"Working directory. Relative paths use the owner's workspace; absolute paths are allowed."},
                        "env": {"type":"object","additionalProperties":{"type":"string"},"description":"Extra environment variables"},
                        "description": {"type":"string","description":"Human-readable description"},
                        "restart_policy": {"type":"string","enum":["never","on_failure","always"],"default":"never"},
                        "schedule": {"type":"string","description":"Cron schedule (e.g. '0 3 * * *') — first run still fires immediately"}
                    },
                    "required":["command"],"additionalProperties":false
                }),
                handler: h_job_run,
            },
            ToolDef {
                name: "job_start",
                description: "Start a stopped job.",
                schema: json!({
                    "type":"object",
                    "properties":{"id":{"type":"string","description":"Job ID (UUID)"}},
                    "required":["id"],"additionalProperties":false
                }),
                handler: h_job_start,
            },
            ToolDef {
                name: "job_stop",
                description: "Stop a running job (graceful).",
                schema: json!({
                    "type":"object",
                    "properties":{"id":{"type":"string","description":"Job ID (UUID)"}},
                    "required":["id"],"additionalProperties":false
                }),
                handler: h_job_stop,
            },
            ToolDef {
                name: "job_delete",
                description: "Delete a job permanently (stops it first if running).",
                schema: json!({
                    "type":"object",
                    "properties":{"id":{"type":"string","description":"Job ID (UUID)"}},
                    "required":["id"],"additionalProperties":false
                }),
                handler: h_job_delete,
            },
            ToolDef {
                name: "job_logs",
                description: "Fetch a job's recent log output (tail).",
                schema: json!({
                    "type":"object",
                    "properties": {
                        "id": {"type":"string","description":"Job ID (UUID)"},
                        "max_bytes": {"type":"integer","minimum":1,"maximum":1048576,"default":32768}
                    },
                    "required":["id"],"additionalProperties":false
                }),
                handler: h_job_logs,
            },
            ToolDef {
                name: "run_command",
                description: "Run a one-off command on the host and wait for it to finish. By default the command is executed through PowerShell (powershell -Command <command>); set shell=false to run an executable directly with args. Returns exit code and output. Uses a temporary job that is deleted afterwards unless keep_job=true.",
                schema: json!({
                    "type":"object",
                    "properties": {
                        "command": {"type":"string","description":"PowerShell command (or executable path when shell=false)"},
                        "shell": {"type":"boolean","default":true,"description":"Run through PowerShell"},
                        "args": {"type":"array","items":{"type":"string"},"description":"Arguments when shell=false"},
                        "cwd": {"type":"string","description":"Working directory. Relative paths use the owner's workspace; absolute paths (e.g. C:\\Users) are allowed."},
                        "timeout_seconds": {"type":"integer","minimum":1,"maximum":600,"default":120},
                        "max_output_bytes": {"type":"integer","minimum":256,"maximum":262144,"default":32768},
                        "keep_job": {"type":"boolean","default":false,"description":"Keep the job after completion for later inspection via job_logs"}
                    },
                    "required":["command"],"additionalProperties":false
                }),
                handler: h_run_command,
            },
            ToolDef {
                name: "files_list",
                description: "List files and directories. Default scope is the per-user workspace; scope=system (admin only) browses the whole PC.",
                schema: json!({
                    "type":"object",
                    "properties":{
                        "path":{"type":"string","description":"Directory path. Workspace: '/dir'. System: '/' for drives, '/C:/Users'.","default":"/"},
                        "scope":{"type":"string","enum":["workspace","system"],"default":"workspace","description":"workspace = own sandbox; system = whole PC (admin only)"}
                    },
                    "additionalProperties":false
                }),
                handler: h_files_list,
            },
            ToolDef {
                name: "file_read",
                description: "Read a text file (truncated to max_bytes). Default scope is the per-user workspace; scope=system (admin only) reads anywhere on the PC.",
                schema: json!({
                    "type":"object",
                    "properties": {
                        "path": {"type":"string","description":"File path. Workspace: '/dir/file'. System: '/C:/Users/.../file'."},
                        "scope": {"type":"string","enum":["workspace","system"],"default":"workspace","description":"workspace = own sandbox; system = whole PC (admin only)"},
                        "max_bytes": {"type":"integer","minimum":1,"maximum":1048576,"default":65536}
                    },
                    "required":["path"],"additionalProperties":false
                }),
                handler: h_file_read,
            },
            ToolDef {
                name: "file_write",
                description: "Create or overwrite a text file. Default scope is the per-user workspace; scope=system (admin only) writes anywhere on the PC.",
                schema: json!({
                    "type":"object",
                    "properties": {
                        "path": {"type":"string","description":"File path. Workspace: '/dir/file'. System: '/C:/Users/.../file'."},
                        "scope": {"type":"string","enum":["workspace","system"],"default":"workspace","description":"workspace = own sandbox; system = whole PC (admin only)"},
                        "content": {"type":"string","description":"File content (UTF-8)"}
                    },
                    "required":["path","content"],"additionalProperties":false
                }),
                handler: h_file_write,
            },
            ToolDef {
                name: "file_mkdir",
                description: "Create a directory (including intermediate parents). Default scope is the per-user workspace; scope=system (admin only) creates anywhere on the PC.",
                schema: json!({
                    "type":"object",
                    "properties":{
                        "path":{"type":"string","description":"Directory path. Workspace: '/dir'. System: '/C:/Users/...'."},
                        "scope":{"type":"string","enum":["workspace","system"],"default":"workspace","description":"workspace = own sandbox; system = whole PC (admin only)"}
                    },
                    "required":["path"],"additionalProperties":false
                }),
                handler: h_file_mkdir,
            },
            ToolDef {
                name: "file_move",
                description: "Move or rename a file or directory. Default scope is the per-user workspace; scope=system (admin only) moves anywhere on the PC.",
                schema: json!({
                    "type":"object",
                    "properties": {
                        "from": {"type":"string","description":"Source path. Workspace: '/a'. System: '/C:/Users/...'."},
                        "to": {"type":"string","description":"Destination path. Workspace: '/b'. System: '/D:/Backup/...'."},
                        "scope": {"type":"string","enum":["workspace","system"],"default":"workspace","description":"workspace = own sandbox; system = whole PC (admin only)"}
                    },
                    "required":["from","to"],"additionalProperties":false
                }),
                handler: h_file_move,
            },
            ToolDef {
                name: "file_delete",
                description: "Delete a file or directory. Default scope is the per-user workspace; scope=system (admin only) deletes anywhere on the PC (critical roots are protected).",
                schema: json!({
                    "type":"object",
                    "properties":{
                        "path":{"type":"string","description":"Path. Workspace: '/dir/file'. System: '/C:/Users/.../file'."},
                        "scope":{"type":"string","enum":["workspace","system"],"default":"workspace","description":"workspace = own sandbox; system = whole PC (admin only)"}
                    },
                    "required":["path"],"additionalProperties":false
                }),
                handler: h_file_delete,
            },
            ToolDef {
                name: "audit_list",
                description: "List recent audit-log events (who did what, when).",
                schema: json!({
                    "type":"object",
                    "properties":{"limit":{"type":"integer","minimum":1,"maximum":2000,"default":100}},
                    "additionalProperties":false
                }),
                handler: h_audit_list,
            },
        ]
    });
    &TOOLS
}

fn need_str(params: &Value, key: &str) -> Result<String, ToolError> {
    params[key]
        .as_str()
        .map(String::from)
        .ok_or_else(|| ToolError::msg(format!("`{key}` (string) is required")))
}

fn scope_param(params: &Value) -> Option<&str> {
    params["scope"].as_str().filter(|scope| !scope.is_empty())
}

// --- handlers ---

fn h_system_overview(api: &Api, _p: &Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::Json(api.get("/api/system/overview", "")?))
}

fn h_jobs_list(api: &Api, _p: &Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::Json(api.get("/api/jobs", "")?))
}

fn h_job_get(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let id = need_str(p, "id")?;
    Ok(ToolOutput::Json(api.get(&format!("/api/jobs/{id}"), "")?))
}

fn h_job_run(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let mut body = json!({
        "name": p["name"].as_str().unwrap_or("mcp-job"),
        "command": need_str(p, "command")?,
        "args": p["args"].as_array().cloned().unwrap_or_default(),
        "description": p["description"].as_str().unwrap_or("created via MCP"),
        "restart_policy": p["restart_policy"].as_str().unwrap_or("never"),
    });
    if let Some(cwd) = p["cwd"].as_str() {
        body["cwd"] = json!(cwd);
    }
    if let Some(env) = p["env"].as_object() {
        body["env"] = json!(env);
    }
    if let Some(s) = p["schedule"].as_str() {
        body["schedule"] = json!(s);
    }
    // NOTE: the server starts the job immediately on create (spawn_supervisor),
    // so no explicit /start call here — it would 409 with "already running".
    Ok(ToolOutput::Json(api.post("/api/jobs", body)?))
}

fn h_job_start(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let id = need_str(p, "id")?;
    Ok(ToolOutput::Json(
        api.post(&format!("/api/jobs/{id}/start"), json!({}))?,
    ))
}

fn h_job_stop(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let id = need_str(p, "id")?;
    Ok(ToolOutput::Json(
        api.post(&format!("/api/jobs/{id}/stop"), json!({}))?,
    ))
}

fn h_job_delete(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let id = need_str(p, "id")?;
    Ok(ToolOutput::Json(
        api.delete(&format!("/api/jobs/{id}"), "")?,
    ))
}

fn h_job_logs(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let id = need_str(p, "id")?;
    let max = p["max_bytes"]
        .as_u64()
        .unwrap_or(32 * 1024)
        .min(1024 * 1024);
    let v = api.get(&format!("/api/jobs/{id}/logs"), &format!("max_bytes={max}"))?;
    Ok(ToolOutput::Text(
        v["content"].as_str().unwrap_or("").to_string(),
    ))
}

fn h_run_command(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let command = need_str(p, "command")?;
    let shell = p["shell"].as_bool().unwrap_or(true);
    let timeout_secs = p["timeout_seconds"].as_u64().unwrap_or(120).min(600);
    let max_out = p["max_output_bytes"]
        .as_u64()
        .unwrap_or(32 * 1024)
        .min(256 * 1024) as usize;
    let keep = p["keep_job"].as_bool().unwrap_or(false);

    let (cmd, args) = if shell {
        (
            "powershell.exe".to_string(),
            vec!["-NoProfile".to_string(), "-Command".to_string(), command],
        )
    } else {
        let args: Vec<String> = p["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        (command.clone(), args)
    };

    let name = format!(
        "mcp-run-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    );
    let mut body = json!({
        "name": name,
        "command": cmd,
        "args": args,
        "description": "MCP run_command",
        "restart_policy": "never"
    });
    if let Some(cwd) = p["cwd"].as_str().filter(|cwd| !cwd.is_empty()) {
        body["cwd"] = json!(cwd);
    }
    let job = api.post("/api/jobs", body)?;
    let id = job["id"]
        .as_str()
        .ok_or_else(|| ToolError::msg("no job id in response"))?
        .to_string();

    let started = std::time::Instant::now();
    let mut timed_out = false;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let v = api.get(&format!("/api/jobs/{id}"), "")?;
        let job = &v["job"];
        let status = job["status"].as_str().unwrap_or("");
        if status == "stopped" || status == "failed" {
            break;
        }
        if started.elapsed().as_secs() >= timeout_secs {
            timed_out = true;
            let _ = api.post(&format!("/api/jobs/{id}/stop"), json!({}));
            std::thread::sleep(std::time::Duration::from_millis(800));
            break;
        }
    }

    let logs = api.get(
        &format!("/api/jobs/{id}/logs"),
        &format!("max_bytes={max_out}"),
    )?;
    let logs_text = logs["content"].as_str().unwrap_or("").to_string();
    let final_v = api.get(&format!("/api/jobs/{id}"), "").ok();
    let final_job = final_v
        .as_ref()
        .map(|v| v["job"].clone())
        .unwrap_or(Value::Null);
    if !keep {
        let _ = api.delete(&format!("/api/jobs/{id}"), "");
    }

    let mut out = String::new();
    if timed_out {
        out.push_str(&format!(
            "TIMED OUT after {timeout_secs}s — the job was stopped. Partial output follows.\n\n"
        ));
    }
    if let Some(e) = final_job["last_error"].as_str() {
        out.push_str(&format!("error: {e}\n"));
    }
    out.push_str(&format!("exit_code: {}\n", final_job["exit_code"]));
    out.push_str("--- output ---\n");
    out.push_str(&logs_text);
    Ok(ToolOutput::Text(out))
}

fn h_files_list(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let path = p["path"].as_str().unwrap_or("/");
    let mut query = format!("path={}", Api::enc(path));
    if let Some(scope) = scope_param(p) {
        query.push_str(&format!("&scope={}", Api::enc(scope)));
    }
    Ok(ToolOutput::Json(api.get("/api/files/list", &query)?))
}

fn h_file_read(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let path = need_str(p, "path")?;
    let max = p["max_bytes"]
        .as_u64()
        .unwrap_or(64 * 1024)
        .min(1024 * 1024) as usize;
    Ok(ToolOutput::Text(api.download_text(
        &path,
        max,
        scope_param(p),
    )?))
}

fn h_file_write(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let path = need_str(p, "path")?;
    let content = need_str(p, "content")?;
    let (dir, name) = split_path(&path)?;
    let res = api.upload(&dir, &name, content.as_bytes(), scope_param(p))?;
    Ok(ToolOutput::Json(res))
}

fn split_path(path: &str) -> Result<(String, String), ToolError> {
    let normalized = path.trim().replace('\\', "/");
    let p = normalized.trim_start_matches('/');
    if p.is_empty() {
        return Err(ToolError::msg("`path` must name a file"));
    }
    if p.split('/').any(|seg| seg == "..") {
        return Err(ToolError::msg("`path` must not contain `..`"));
    }
    let (dir, name) = match p.rfind('/') {
        Some(i) => {
            let parent = &p[..i];
            let dir = if parent.is_empty() {
                "/".to_string()
            } else {
                format!("/{parent}")
            };
            (dir, &p[i + 1..])
        }
        None => ("/".to_string(), p),
    };
    if name.is_empty() {
        return Err(ToolError::msg("`path` must name a file"));
    }
    files::safe_file_name(name).map_err(|e| ToolError::msg(format!("invalid path: {e}")))?;
    Ok((dir, name.to_string()))
}

fn h_file_mkdir(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let path = need_str(p, "path")?;
    let mut body = json!({ "path": path });
    if let Some(scope) = scope_param(p) {
        body["scope"] = json!(scope);
    }
    Ok(ToolOutput::Json(api.post("/api/files/mkdir", body)?))
}

fn h_file_move(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let from = need_str(p, "from")?;
    let to = need_str(p, "to")?;
    let mut body = json!({ "from": from, "to": to });
    if let Some(scope) = scope_param(p) {
        body["scope"] = json!(scope);
    }
    Ok(ToolOutput::Json(api.post("/api/files/move", body)?))
}

fn h_file_delete(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let path = need_str(p, "path")?;
    let mut query = format!("path={}", Api::enc(&path));
    if let Some(scope) = scope_param(p) {
        query.push_str(&format!("&scope={}", Api::enc(scope)));
    }
    Ok(ToolOutput::Json(api.delete("/api/files/delete", &query)?))
}

fn h_audit_list(api: &Api, p: &Value) -> Result<ToolOutput, ToolError> {
    let limit = p["limit"].as_u64().unwrap_or(100).min(2000);
    Ok(ToolOutput::Json(
        api.get("/api/audit", &format!("limit={limit}"))?,
    ))
}

// ---------------------------------------------------------------------------
// JSON-RPC dispatch
// ---------------------------------------------------------------------------

fn ok_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err_result(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Build the initialize result.
fn initialize_result(client_version: Option<&str>) -> Value {
    json!({
        "protocolVersion": client_version.unwrap_or("2024-11-05"),
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "kenji", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn tool_defs_json() -> Value {
    json!({ "tools": tools().iter().map(|t| json!({
        "name": t.name,
        "description": t.description,
        "inputSchema": t.schema,
    })).collect::<Vec<_>>() })
}

/// Handle one incoming message; None = no reply (notification).
fn dispatch(api: &Api, msg: &Value) -> Option<Value> {
    let method = msg["method"].as_str()?;
    let id = msg.get("id").cloned();

    // notifications (no id) get no response
    let id = id?;

    let result = match method {
        "initialize" => Some(initialize_result(msg["params"]["protocolVersion"].as_str())),
        "ping" => Some(json!({})),
        "tools/list" => Some(tool_defs_json()),
        "tools/call" => Some(call_tool(api, &msg["params"])),
        "resources/list" => Some(json!({ "resources": [] })),
        "prompts/list" => Some(json!({ "prompts": [] })),
        _ => None,
    };

    match result {
        Some(r) => Some(ok_result(id, r)),
        None => Some(err_result(
            id,
            -32601,
            &format!("method not found: {method}"),
        )),
    }
}

fn call_tool(api: &Api, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let args = &params["arguments"];
    let Some(def) = tools().iter().find(|t| t.name == name) else {
        return json!({
            "content": [{ "type": "text", "text": format!("unknown tool: {name}") }],
            "isError": true,
        });
    };
    match (def.handler)(api, args) {
        Ok(ToolOutput::Json(v)) => {
            let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string());
            json!({ "content": [{ "type": "text", "text": text }], "isError": false })
        }
        Ok(ToolOutput::Text(t)) => {
            json!({ "content": [{ "type": "text", "text": t }], "isError": false })
        }
        Err(e) => json!({ "content": [{ "type": "text", "text": e.message }], "isError": true }),
    }
}

/// Main stdio loop. Returns process exit code.
pub fn run(base_url: String, token: String) -> i32 {
    let api = Api::new(base_url, token);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let _ = serde_json::to_writer(
                    &mut out,
                    &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}),
                );
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                continue;
            }
        };
        let responses: Vec<Value> = match &msg {
            Value::Array(items) => items.iter().filter_map(|m| dispatch(&api, m)).collect(),
            _ => dispatch(&api, &msg).into_iter().collect(),
        };
        for r in responses {
            if serde_json::to_writer(&mut out, &r).is_ok() {
                let _ = out.write_all(b"\n");
            }
            let _ = out.flush();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_and_tools_list() {
        let api = Api::new("http://127.0.0.1:1".into(), "unused".into());
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}});
        let resp = dispatch(&api, &init).unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(resp["result"]["serverInfo"]["name"], "kenji");

        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let resp = dispatch(&api, &list).unwrap();
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"run_command"));
        assert!(names.contains(&"file_write"));
        assert!(names.len() >= 12);
    }

    #[test]
    fn notifications_get_no_response_and_unknown_method_errors() {
        let api = Api::new("http://127.0.0.1:1".into(), "unused".into());
        assert!(dispatch(&api, &json!({"method":"notifications/initialized"})).is_none());
        let err = dispatch(&api, &json!({"jsonrpc":"2.0","id":"a","method":"nope"})).unwrap();
        assert_eq!(err["error"]["code"], -32601);
    }

    #[test]
    fn split_path_rules() {
        assert_eq!(
            split_path("/a/b.txt").unwrap(),
            ("/a".to_string(), "b.txt".to_string())
        );
        assert_eq!(
            split_path("/b.txt").unwrap(),
            ("/".to_string(), "b.txt".to_string())
        );
        assert_eq!(
            split_path(r"C:\Users\a.txt").unwrap(),
            ("/C:/Users".to_string(), "a.txt".to_string())
        );
        assert_eq!(
            split_path("/C:/a.txt").unwrap(),
            ("/C:".to_string(), "a.txt".to_string())
        );
        assert!(split_path("/a/../c.txt").is_err());
        assert!(split_path(r"C:\Users\..\secret.txt").is_err());
    }
}
