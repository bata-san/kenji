// Kenji MCP stdio integration smoke test (Node — reliable process stdio, unlike PS 5.1 pipes)
// Usage: node scripts/mcp_smoke.mjs
// Assumes: target/{debug,release}/kenji.exe built. Starts a throwaway server on
// 127.0.0.1:8787 (data dir .data-mcp-smoke), then drives `kenji mcp` over stdio.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const BIN = [path.join(ROOT, 'target/debug/kenji.exe'), path.join(ROOT, 'target/release/kenji.exe')]
  .find(existsSync);
if (!BIN) {
  console.error('kenji.exe not found. Build first.');
  process.exit(1);
}
const DATA = path.join(ROOT, '.data-mcp-smoke');
const BASE = 'http://127.0.0.1:8787';
const USER = 'admin';
const PASS = 'test123456';

let pass = 0, fail = 0;
const check = (name, cond) => {
  if (cond) { pass++; console.log(`  PASS ${name}`); }
  else { fail++; console.log(`  FAIL ${name}`); }
};

rmSync(DATA, { recursive: true, force: true });

console.log('== setup ==');
const setup = spawnSync(BIN, ['setup', '--data-dir', DATA, '--username', USER, '--password', PASS],
  { stdio: 'inherit' });
if (setup.status !== 0) { console.error('SETUP FAILED'); process.exit(1); }

console.log('== serve (background) ==');
const server = spawn(BIN, ['serve', '--data-dir', DATA], { stdio: ['ignore', 'pipe', 'pipe'] });

// --- helpers -------------------------------------------------------------
async function waitHealthy() {
  for (let i = 0; i < 40; i++) {
    try {
      const r = await fetch(`${BASE}/api/health`);
      if (r.ok) return true;
    } catch { /* not up yet */ }
    await new Promise((r) => setTimeout(r, 250));
  }
  return false;
}

/** Send a batch of JSON-RPC lines to `kenji mcp`, return responses keyed by id. */
async function mcpBatch(lines, token) {
  const child = spawn(BIN, ['mcp', '--url', BASE, '--token', token]);
  let out = '', err = '';
  child.stdout.on('data', (d) => { out += d; });
  child.stderr.on('data', (d) => { err += d; });
  const done = new Promise((resolve) => child.on('close', resolve));
  for (const l of lines) child.stdin.write(l + '\n');
  child.stdin.end();
  const code = await done;
  if (code !== 0) throw new Error(`kenji mcp exited with ${code}; stderr: ${err}`);
  const resp = new Map();
  for (const l of out.split('\n')) {
    const t = l.trim();
    if (!t) continue;
    const o = JSON.parse(t);
    if (o.id !== undefined && o.id !== null) resp.set(String(o.id), o);
    else resp.set('parse-error', o);
  }
  return resp;
}
const rpc = (id, method, params) => JSON.stringify({ jsonrpc: '2.0', id, method, ...(params ? { params } : {}) });
const toolCall = (id, name, args) => rpc(id, 'tools/call', { name, arguments: args });
const toolText = (r) => r.result?.content?.[0]?.text ?? '';

let token = '';
const jobCommand = '1..5 | ForEach-Object { Write-Output "mcp=$_"; Start-Sleep -Milliseconds 500 }';

try {
  if (!(await waitHealthy())) throw new Error('server did not become healthy');
  const loginRes = await fetch(`${BASE}/api/auth/login`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: USER, password: PASS }),
  });
  const login = await loginRes.json();
  token = login.token;
  check('login', !!token);

  // 1: initialize / notification / tools/list in one batch
  let r = await mcpBatch([
    rpc(1, 'initialize', { protocolVersion: '2025-06-18' }),
    JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' }),
    rpc(2, 'tools/list'),
  ], token);
  check('initialize', r.get('1')?.result?.serverInfo?.name === 'kenji'
    && r.get('1')?.result?.protocolVersion === '2025-06-18');
  check('notification-no-response', r.size === 2);
  const names = (r.get('2')?.result?.tools ?? []).map((t) => t.name);
  check('tools-list-16', names.length >= 16);
  check('tools-list-run-command', names.includes('run_command'));

  // 2: system_overview
  r = await mcpBatch([toolCall(1, 'system_overview', {})], token);
  const ov = JSON.parse(toolText(r.get('1')));
  check('system_overview', r.get('1')?.result?.isError !== true && !!ov.cpu);

  // 3: job_run (create starts the job server-side; status may need a moment)
  r = await mcpBatch([toolCall(1, 'job_run', {
    name: 'mcp-smoke-counter', command: 'powershell.exe',
    args: ['-NoProfile', '-Command', jobCommand],
  })], token);
  const job = JSON.parse(toolText(r.get('1')));
  const jobId = job.id;
  check('job_run-created', !!jobId);

  let running = false;
  for (let i = 0; i < 10 && !running; i++) {
    const g = await mcpBatch([toolCall(1, 'job_get', { id: jobId })], token);
    running = JSON.parse(toolText(g.get('1'))).job?.status === 'running';
    if (!running) await new Promise((res) => setTimeout(res, 300));
  }
  check('job_run-started', running);

  await new Promise((res) => setTimeout(res, 2000));
  r = await mcpBatch([toolCall(1, 'job_logs', { id: jobId })], token);
  check('job_logs', toolText(r.get('1')).includes('mcp='));

  r = await mcpBatch([toolCall(1, 'job_get', { id: jobId })], token);
  const jg = JSON.parse(toolText(r.get('1')));
  check('job_get', jg.job?.id === jobId);

  // 4: run_command (one-off; temp job removed afterwards)
  r = await mcpBatch([toolCall(1, 'run_command', { command: 'Write-Output hello-from-mcp', timeout_seconds: 60 })], token);
  const rc = toolText(r.get('1'));
  check('run_command', rc.includes('hello-from-mcp') && rc.includes('exit_code: 0'));
  r = await mcpBatch([toolCall(1, 'jobs_list', {})], token);
  const jobsAfter = JSON.parse(toolText(r.get('1')));
  check('run_command-cleanup', !jobsAfter.jobs?.some((j) => j.name?.startsWith('mcp-run-')));

  // 5: files
  r = await mcpBatch([toolCall(1, 'file_mkdir', { path: '/mcp-test' })], token);
  check('file_mkdir', r.get('1')?.result?.isError !== true);
  r = await mcpBatch([toolCall(1, 'file_write', { path: '/mcp-test/hello.txt', content: 'hello from mcp' })], token);
  check('file_write', r.get('1')?.result?.isError !== true);
  r = await mcpBatch([toolCall(1, 'file_read', { path: '/mcp-test/hello.txt' })], token);
  check('file_read', toolText(r.get('1')).includes('hello from mcp'));
  r = await mcpBatch([toolCall(1, 'files_list', { path: '/mcp-test' })], token);
  const ls = JSON.parse(toolText(r.get('1')));
  check('files_list', ls.entries?.some((e) => e.name === 'hello.txt'));
  r = await mcpBatch([toolCall(1, 'file_move', { from: '/mcp-test/hello.txt', to: '/mcp-test/renamed.txt' })], token);
  check('file_move', r.get('1')?.result?.isError !== true);
  r = await mcpBatch([toolCall(1, 'file_delete', { path: '/mcp-test/renamed.txt' })], token);
  check('file_delete', r.get('1')?.result?.isError !== true);

  // 6: audit_list
  r = await mcpBatch([toolCall(1, 'audit_list', { limit: 50 })], token);
  const audit = JSON.parse(toolText(r.get('1')));
  check('audit_list', (audit.entries?.length ?? 0) > 0);

  // 7: stop + delete job
  r = await mcpBatch([toolCall(1, 'job_stop', { id: jobId })], token);
  check('job_stop', r.get('1')?.result?.isError !== true);
  r = await mcpBatch([toolCall(1, 'job_delete', { id: jobId })], token);
  check('job_delete', r.get('1')?.result?.isError !== true);

  // 8: error paths
  r = await mcpBatch([toolCall(1, 'no_such_tool', {})], token);
  check('unknown-tool-isError', r.get('1')?.result?.isError === true);
  r = await mcpBatch([rpc('e', 'bogus/method')], token);
  check('unknown-method-32601', r.get('e')?.error?.code === -32601);
  r = await mcpBatch(['not json at all'], token);
  check('parse-error-32700', r.get('parse-error')?.error?.code === -32700);
} catch (e) {
  console.error(`EXCEPTION: ${e.message}`);
  fail++;
} finally {
  if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(server.pid), '/T', '/F'], { stdio: 'ignore' });
  else server.kill('SIGKILL');
}

console.log(`\nRESULT: ${pass} pass / ${fail} fail`);
process.exit(fail > 0 ? 1 : 0);
