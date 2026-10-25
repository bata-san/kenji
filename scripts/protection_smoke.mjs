// Self-contained API/resource/crash observer smoke. Uses .data-update/protection.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
const root = process.cwd();
const bin = path.join(root, 'target/debug/kenji.exe');
const data = path.join(root, '.data-update/protection');
const base = 'http://127.0.0.1:8794';
fs.rmSync(data, { recursive: true, force: true });
execFileSync(bin, ['setup', '--data-dir', data, '--username', 'admin', '--password', 'test123456'], { stdio: 'ignore' });
let server;
let token;
let checks = 0;
function check(name, condition) { assert.ok(condition, name); checks++; console.log(`PASS ${name}`); }
async function request(url, method = 'GET', body) {
  const response = await fetch(base + url, { method, headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
  return { response, body: await response.json() };
}
async function until(action, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { try { const value = await action(); if (value) return value; } catch {} await new Promise(r => setTimeout(r, 100)); }
  throw new Error('Timed out');
}
try {
  server = spawn(bin, ['serve', '--data-dir', data, '--port', '8794'], { stdio: 'ignore' });
  await until(async () => (await fetch(base + '/api/health')).ok);
  token = (await request('/api/auth/login', 'POST', { username: 'admin', password: 'test123456' })).body.token;
  check('login', !!token);
  let settings = (await request('/api/settings')).body;
  check('protection disabled by default', settings.resources.enabled === false);
  const toggle = await request('/api/settings', 'PUT', { desktop_priority: true });
  check('enable protection', toggle.response.ok && toggle.body.resources.enabled);
  const job = await request('/api/jobs', 'POST', { name: 'protection-smoke', command: 'powershell.exe', args: ['-NoProfile', '-Command', 'Write-Output protected; Start-Sleep -Seconds 30'] });
  const id = job.body.id;
  check('create protected job', !!id);
  const running = await until(async () => { const j = (await request(`/api/jobs/${id}`)).body.job; return j.status === 'running' && j; });
  check('suspended job resumes', !!running.pid);
  const logs = await until(async () => { const value = (await request(`/api/jobs/${id}/logs`)).body.content; return value?.includes('protected') && value; });
  check('job output', !!logs);
  const disable = await request('/api/settings', 'PUT', { desktop_priority: false });
  check('disable while running', disable.response.ok && !disable.body.resources.enabled);
  check('job remains running', (await request(`/api/jobs/${id}`)).body.job.status === 'running');
  const invalid = await request('/api/settings', 'PUT', { desktop_priority: true, update: { repo: 'invalid' } });
  check('invalid combined settings rejected', !invalid.response.ok);
  check('invalid request has no partial resource side effect', !(await request('/api/settings')).body.resources.enabled);
  const key = (await request('/api/auth/keys', 'POST', { name: 'viewer-test', role: 'viewer' })).body;
  const adminToken = token;
  token = key.key;
  check('viewer cannot change resource limits', (await request('/api/settings', 'PUT', { desktop_priority: true })).response.status === 403);
  check('viewer cannot launch diagnostic worker', (await request('/api/repair/diagnose', 'POST')).response.status === 403);
  token = adminToken;
  await request(`/api/auth/keys/${key.id}`, 'DELETE');
  for (const p of ['/', '/.', '/sub/..']) {
    const result = await request('/api/files/delete?path=' + encodeURIComponent(p), 'DELETE');
    check('reject root deletion ' + p, !result.response.ok);
  }
  check('reject moving root', !(await request('/api/files/move', 'POST', { from: '/', to: '/renamed' })).response.ok);
  check('repair disabled by default', !(await request('/api/repair/diagnose', 'POST')).response.ok);
  check('reject insecure provider', !(await request('/api/settings', 'PUT', { repair: { ...settings.repair, base_url: 'http://example.com' } })).response.ok);
  await request(`/api/jobs/${id}/stop`, 'POST');
  await request(`/api/jobs/${id}`, 'DELETE');
  // Enable an actual independent crash observer with a fake worker only. This
  // validates orchestration, not real-provider quality or paid model inference.
  const worker = path.join(data, 'fake-worker.mjs');
  fs.writeFileSync(worker, `let raw='';for await(const c of process.stdin)raw+=c;const r=JSON.parse(raw);if(!r.incident.includes('abnormally'))process.exit(2);console.log('fake diagnosis: no deployment');`);
  const configPath = path.join(data, 'config.json');
  const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
  config.repair = { ...config.repair, enabled: true, model: 'fake', harness_path: worker, api_key_env: 'KENJI_TEST_REPAIR_KEY' };
  fs.writeFileSync(configPath, JSON.stringify(config));
  // Current observer inherited server's environment, so this first crash records
  // missing key rather than calling any external service.
  server.kill();
  await until(() => fs.existsSync(path.join(data, 'repair/status.json')));
  const failed = JSON.parse(fs.readFileSync(path.join(data, 'repair/status.json')));
  check('observer survives server crash', failed.state === 'failed');
  check('missing credential fails closed', failed.error?.includes('APIキー'));
  check('single-flight lock cleaned after failure', !fs.existsSync(path.join(data, 'repair/active.lock')));
  fs.rmSync(path.join(data, 'repair/status.json'));
  // Test-only reset of the production five-minute crash-loop cooldown.
  fs.rmSync(path.join(data, 'repair/last-attempt'));
  server = spawn(bin, ['serve', '--data-dir', data, '--port', '8794'], { stdio: 'ignore', env: { ...process.env, KENJI_TEST_REPAIR_KEY: 'fake-not-a-secret' } });
  await until(async () => (await fetch(base + '/api/health')).ok);
  server.kill();
  const result = await until(() => {
    try { const s = JSON.parse(fs.readFileSync(path.join(data, 'repair/status.json'))); return ['completed', 'failed'].includes(s.state) && s; } catch { return false; }
  });
  check('crash launches bounded diagnostic worker', result.state === 'completed');
  check('fake worker result saved', fs.readFileSync(result.output_file, 'utf8').includes('fake diagnosis'));
  check('deployment remains disabled', result.automatic_deployment === false);
  check('single-flight lock cleaned after success', !fs.existsSync(path.join(data, 'repair/active.lock')));
  console.log(`RESULT: ${checks} protection checks passed (worker is fake; no paid provider call)`);
} finally {
  server?.kill();
  // Restore a safe disabled setting for this disposable test data.
  const cfg = JSON.parse(fs.readFileSync(path.join(data, 'config.json')));
  cfg.repair.enabled = false;
  fs.writeFileSync(path.join(data, 'config.json'), JSON.stringify(cfg));
}
