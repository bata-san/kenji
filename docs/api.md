# Kenji REST/WebSocket API 契約 (v0.1)

フロントエンド・クライアント実装はこの文書に正確に従うこと。

- ベース: `http://127.0.0.1:8787`（本番は同オリジン。開発時は Vite プロキシ `http://127.0.0.1:8787`）
- 全て `/api` 配下。JSON は UTF-8。日時は ISO 8601 (UTC)。バイト数は整数。
- 認証: `Authorization: Bearer <token>`。token = ログインJWT または APIキー（`kjk_` 始まり）。
- ロール: `viewer` < `operator` < `admin`。各エンドポイントに必要ロールを記す（JWTでもAPIキーでも同じロール体系）。
- エラー形式: HTTPエラー時 `{"error":{"code":"...","message":"..."}}`。主要コード: `unauthorized`(401), `forbidden`(403), `not_found`(404), `conflict`(409), `bad_request`(400), `internal`(500)
- WebSocket はヘッダーを渡せないため `?token=<token>` クエリで認証。ロール不足は upgrade 前に 401/403 を返す。

---

## Auth

### POST /api/auth/login  (認証不要)
```json
リクエスト: {"username":"admin","password":"..."}
成功 200: {"token":"<jwt>","username":"admin","role":"admin","expires_at":"2026-10-26T06:00:00Z"}
失敗 401: {"error":{"code":"unauthorized","message":"invalid credentials"}}
```

### GET /api/auth/me  (viewer+)
→ `{"username":"admin","role":"admin"}`

### GET /api/auth/keys  (admin)
→ `{"keys":[{"id":"...","name":"ci","role":"operator","created_at":"...","last_used_at":"...|null"}]}`

### POST /api/auth/keys  (admin)
```json
リクエスト: {"name":"ci","role":"operator"}
→ 200: {"id":"...","name":"ci","role":"operator","key":"kjk_XXXX..."}   // keyはこの時だけ一度だけ返る
```

### DELETE /api/auth/keys/{id}  (admin)
→ 200 `{"ok":true}`

---

## System

### GET /api/health  (認証不要)
→ `{"ok":true,"version":"0.1.0","setup_done":true}`

### GET /api/system/overview  (viewer+)
```json
{
  "hostname":"DESKTOP-XYZ","os":"Windows 11 Pro","version":"0.1.0","uptime_secs":12345,
  "cpu":{"cores":16,"usage_percent":12.5},
  "memory":{"total_bytes":34359738368,"used_bytes":15032385536},
  "disks":[{"label":"C:","mount_point":"C:\\","total_bytes":512110190592,"available_bytes":231045020672}],
  "gpus":[{"name":"NVIDIA GeForce RTX 4090","utilization_percent":5,"memory_total_bytes":25757220864,"memory_used_bytes":1073741824}],
  "jobs_summary":{"running":2,"total":7,"failed":1},
  "tunnel":{"available":true,"enabled":true,"url":"https://xxx.trycloudflare.com","status":"running"}
}
```
（GPU非対応環境は `gpus: []`）

---

## Jobs

Job オブジェクト:
```json
{
  "id":"0a1b...","name":"train","description":"...",
  "command":"python","args":["train.py","--epochs","10"],
  "cwd":"C:\\work\\proj",           // 省略可。相対パスはワークスペースルート基準
  "env":{"CUDA_VISIBLE_DEVICES":"0"}, // 省略可
  "restart_policy":"never",        // "never" | "on_failure" | "always"
  "schedule":"*/5 * * * *",        // 省略可。標準5フィールドcron(分 時 日 月 曜)
  "tags":["ml"],                   // 省略可
  "status":"running",              // "running" | "stopped" | "failed" | "stopping"
  "pid":1234,"exit_code":null,"last_error":null,
  "started_at":"2026-10-25T10:00:00Z","stopped_at":null,"created_at":"2026-10-24T09:00:00Z",
  "schedule_next_at":"2026-10-25T10:05:00Z"  // cron指定時のみ
}
```

### POST /api/jobs  (operator+)  — ジョブ定義を作成し即時開始
```json
リクエスト: {"name":"train","command":"python","args":["train.py"],"cwd":"proj","env":{},"restart_policy":"never","schedule":null,"tags":[],"description":""}
→ 200: Job（status:"running" で返る）
```

### GET /api/jobs  (viewer+)
→ `{"jobs":[Job,...]}`

### GET /api/jobs/{id}  (viewer+)
→ `{"job":Job}`

### POST /api/jobs/{id}/start  (operator+)  — 停止中のジョブを再開/再実行
→ 200 `{"job":Job}`　実行中なら 409 `conflict`

### POST /api/jobs/{id}/stop  (operator+)  — プロセスツリーごと強制停止（手動停止のため自動再開しない）
→ 200 `{"job":Job}`

### DELETE /api/jobs/{id}  (operator+)  — 実行中なら停止して定義を削除
→ 200 `{"ok":true}`

### GET /api/jobs/{id}/logs?max_bytes=65536  (viewer+)  — 末尾のログ取得
→ 200 `{"content":"...テキスト..."}`

### WS /api/jobs/{id}/logs/ws?token=...  (viewer+)  — ライブログ
- 接続直後に過去ログ（最大64KB）を `{"type":"log","data":"..."}` フレームで送信し、以降は新規出力を随時送信。
- 状態変化時に `{"type":"status","job":{...Job}}` を送信。
- クライアントは `{"type":"ping"}` を送ってよい（サーバーは `{"type":"pong"}` を返す）。

---

## Terminal

### WS /api/terminal/ws?token=...  (operator+)  — PowerShell (ConPTY)
- **バイナリフレーム**: クライアント→サーバーはキー入力（UTF-8バイト列）。サーバー→クライアントはPTY出力（UTF-8バイト列）。
- **テキストフレーム（JSON）**: 制御用。クライアント→`{"type":"resize","cols":80,"rows":24}`。サーバー→`{"type":"exit"}`（PTY終了時、その後ソケットを閉じる）。

---

## Files（ワークスペースルート配下のみ。`..` は拒否）

パス表現: ルートが `/`。例 `/src/main.rs`、`/docs`。クエリ/JSONとも文字列。

### GET /api/files/list?path=/  (viewer+)
→ `{"path":"/","entries":[{"name":"main.rs","kind":"file","size":123,"modified":"2026-10-25T09:00:00Z"},{"name":"docs","kind":"dir","size":null,"modified":"..."}]}`

### POST /api/files/mkdir  (operator+)  `{"path":"/newdir"}`
→ 200 `{"ok":true}`

### POST /api/files/move  (operator+)  `{"from":"/a.txt","to":"/b/a.txt"}`
→ 200 `{"ok":true}`

### DELETE /api/files/delete?path=/dir  (operator+)  — ディレクトリは再帰削除
→ 200 `{"ok":true}`

### GET /api/files/download?path=/a.bin  (viewer+)  — 認証はクエリ `token` でも可
→ バイナリ本文、`Content-Disposition: attachment`

### POST /api/files/upload?path=/dir  (operator+)  — multipart/form-data、フィールド名 `file`（複数ファイル可、上限2GiB）
→ 200 `{"ok":true,"saved":[{"name":"a.bin","size":123}]}`

---

## Admin

### GET /api/settings  (admin)
```json
{"data_dir":"C:\\ProgramData\\kenji","workspace_root":"C:\\ProgramData\\kenji\\workspace",
 "bind":"127.0.0.1","port":8787,"version":"0.1.0",
 "tunnel":{"available":true,"enabled":false,"url":null,"status":"disabled"}}
```
tunnel.status: `disabled` | `starting` | `running` | `stopped` | `unavailable` | `error`

### PUT /api/settings  (admin)  — 部分更新
```json
リクエスト: {"tunnel":{"enabled":true}}
→ 200: GET /api/settings と同じ形式
```

### GET /api/audit?limit=200  (admin)
→ `{"entries":[{"ts":"...","user":"admin","role":"admin","action":"job_start","detail":"train"}]}`
（新しい順。action: login / login_failed / job_create / job_start / job_stop / job_delete / file_mkdir / file_move / file_delete / file_upload / file_download / key_create / key_delete / settings_update / terminal_open）

---

## MCP サーバー（stdio）

`kenji mcp` は MCP (Model Context Protocol) サーバーを stdin/stdout 上の JSON-RPC 2.0（改行区切り）で起動します。AIエージェント（Claude Desktop等のMCPクライアント）に、REST APIと同じ操作・権限・監査を提供します。

```
kenji mcp --url http://127.0.0.1:8787 --token <APIキーまたはJWT>
```

- 認証・RBAC（viewer/operator/admin）・監査ログ・ワークスペースサンドボックスはREST APIと同一。トークンにoperatorを渡すのが典型
- `initialize` / `ping` / `tools/list` / `tools/call` をサポート（protocolVersionはクライアントに追従）

### tools（16種）

| ツール | 説明 |
|---|---|
| `system_overview` | ホスト・CPU/メモリ/GPU・稼働ジョブ概要 |
| `jobs_list` / `job_get` | ジョブ一覧・詳細 |
| `job_run` | ジョブ作成（サーバー仕様により作成と同時に起動。停止は `job_stop`。cron/再起動ポリシー指定可） |
| `job_start` / `job_stop` / `job_delete` | ジョブの起動・停止・削除 |
| `job_logs` | ジョブログの末尾取得（max_bytes上限1MiB） |
| `run_command` | one-shotコマンド実行（デフォルトでPowerShell経由、最大600秒待機、一時ジョブは自動削除） |
| `files_list` / `file_read` / `file_write` | サンドボックス内のファイル閲覧・書き込み |
| `file_mkdir` / `file_move` / `file_delete` | サンドボックス内のディレクトリ作成・移動・削除 |
| `audit_list` | 監査ログ参照（上限2000件） |

ツールの引数スキーマは `tools/list` が返す `inputSchema` を参照してください。

### Claude Desktop / MCPクライアント設定例

```json
{
  "mcpServers": {
    "kenji": {
      "command": "C:\\Program Files\\Kenji\\kenji.exe",
      "args": ["mcp", "--url", "http://127.0.0.1:8787", "--token", "kjk_＜operator権限のAPIキー＞"]
    }
  }
}
```

AIエージェントには**operatorロールのAPIキー**を渡すのが典型です（viewerは読み取り専用・adminは過剰）。リモートマシンで稼働するKenjiに接続する場合は、`--url` をCloudflare TunnelのURLにしてください。

---

## 開発用メモ（フロントエンド）
- Vite 開発サーバー: `/api` を `http://127.0.0.1:8787` にプロキシ（`ws: true` 必須）。
- トークンは localStorage 保存。401 ならログインページへ遷移。
- ログインロックアウト: 5回失敗で60秒。
