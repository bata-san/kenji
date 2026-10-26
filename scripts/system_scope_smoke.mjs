// PC全体ファイルスコープ（管理者専用）の統合スモークテスト。
// 使い捨てデータディレクトリとポート8795でサーバーを起動し、
// ドライブ一覧・読み書き・保護パス・非管理者の拒否・workspace回帰を検証する。
// Usage: node scripts/system_scope_smoke.mjs  （リポジトリルートから、cargo build 済み）
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = process.cwd();
const BIN = [path.join(ROOT, 'target/debug/kenji.exe'), path.join(ROOT, 'target/release/kenji.exe')]
  .find((p) => fs.existsSync(p));
if (!BIN) {
  console.error('FAIL: kenji.exe がありません。先に cargo build を実行してください。');
  process.exit(1);
}
const DATA = path.join(ROOT, '.data-update/system-scope');
const PORT = 8795;
const BASE = `http://127.0.0.1:${PORT}`;
const ADMIN = { username: 'admin', password: 'test123456' };
const OPERATOR = { username: 'sysop', password: 'test123456' };

fs.rmSync(DATA, { recursive: true, force: true });
const setup = spawnSync(
  BIN,
  ['setup', '--data-dir', DATA, '--username', ADMIN.username, '--password', ADMIN.password],
  { stdio: 'inherit' },
);
if (setup.status !== 0) {
  console.error('FAIL: setup');
  process.exit(1);
}

let server;
let token = '';
let checks = 0;
function check(name, condition) {
  assert.ok(condition, name);
  checks += 1;
  console.log(`  PASS ${name}`);
}
function apiPath(native) {
  const resolved = path.resolve(native);
  const drive = resolved.slice(0, 2).toUpperCase();
  return `/${drive}${resolved.slice(2).replace(/\\/g, '/')}`;
}
async function request(url, method = 'GET', body, authToken = token) {
  const headers = { Authorization: `Bearer ${authToken}` };
  if (body !== undefined && !(body instanceof FormData)) headers['Content-Type'] = 'application/json';
  const response = await fetch(BASE + url, {
    method,
    headers,
    body: body === undefined ? undefined : body instanceof FormData ? body : JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    /* binary or empty */
  }
  return { response, body: json, text };
}
async function login(user) {
  const res = await fetch(`${BASE}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(user),
  });
  return res.ok ? (await res.json()).token : null;
}
async function until(action, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    try {
      const value = await action();
      if (value) return value;
    } catch {
      /* retry */
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error('Timed out');
}
function uploadForm(content, name = 'smoke.txt') {
  const form = new FormData();
  form.append('file', new Blob([content], { type: 'text/plain' }), name);
  return form;
}

try {
  server = spawn(BIN, ['serve', '--data-dir', DATA, '--port', String(PORT)], { stdio: 'ignore' });
  await until(async () => (await fetch(`${BASE}/api/health`)).ok);
  token = await login(ADMIN);
  check('admin login', !!token);

  // ---- system scope: drive list and reads ----
  let r = await request('/api/files/list?scope=system&path=/');
  check('system root lists drives', r.response.ok && r.body.scope === 'system');
  const drives = r.body.entries ?? [];
  check('C: drive is listed', drives.some((e) => e.kind === 'drive' && e.name === 'C:'));

  r = await request('/api/files/list?scope=system&path=/C:');
  check('drive root lists entries', r.response.ok && Array.isArray(r.body.entries));
  check('drive root normalized', r.body.path === '/C:');

  r = await request('/api/files/list?scope=system&path=/C:/Windows');
  check('reads system directory', r.response.ok && Array.isArray(r.body.entries));

  r = await request('/api/files/list?scope=system&path=' + encodeURIComponent('C:\\Windows'));
  check('accepts native absolute path', r.response.ok && r.body.path === '/C:/Windows');

  r = await request('/api/files/list?scope=system&path=' + encodeURIComponent('/C:/Windows/../Windows'));
  check('rejects traversal', r.response.status === 400);
  r = await request('/api/files/list?scope=system&path=/C:/CON');
  check('rejects reserved device name', r.response.status === 400);
  r = await request('/api/files/list?scope=system&path=/C:/report%20');
  check('rejects trailing space', r.response.status === 400);
  r = await request('/api/files/list?path=/&scope=bogus');
  check('rejects unknown scope', r.response.status === 400);

  // ---- system scope: mkdir / upload / download / move / delete ----
  const sysDir = path.join(DATA, 'sys-test');
  const sysDirApi = apiPath(sysDir);
  r = await request('/api/files/mkdir', 'POST', { path: `${sysDirApi}/sub`, scope: 'system' });
  check('system mkdir', r.response.ok && fs.existsSync(path.join(sysDir, 'sub')));

  const filePath = path.join(sysDir, 'smoke.txt');
  const fileApi = apiPath(filePath);
  r = await request(
    `/api/files/upload?path=${encodeURIComponent(apiPath(sysDir))}&scope=system`,
    'POST',
    uploadForm('system-scope-smoke'),
  );
  check('system upload', r.response.ok && fs.readFileSync(filePath, 'utf8') === 'system-scope-smoke');

  r = await request(
    `/api/files/download?path=${encodeURIComponent(fileApi)}&scope=system&token=${encodeURIComponent(token)}`,
    'GET',
    undefined,
    '',
  );
  check('system download', r.response.ok && r.text === 'system-scope-smoke');

  const renamedApi = apiPath(path.join(sysDir, 'renamed.txt'));
  r = await request('/api/files/move', 'POST', {
    from: fileApi,
    to: renamedApi,
    scope: 'system',
  });
  check('system move', r.response.ok && fs.existsSync(path.join(sysDir, 'renamed.txt')));

  r = await request('/api/files/delete?path=' + encodeURIComponent(renamedApi) + '&scope=system', 'DELETE');
  check('system delete file', r.response.ok && !fs.existsSync(path.join(sysDir, 'renamed.txt')));
  r = await request('/api/files/delete?path=' + encodeURIComponent(sysDirApi) + '&scope=system', 'DELETE');
  check('system delete directory', r.response.ok && !fs.existsSync(sysDir));

  // ---- destructive guards ----
  for (const [label, target] of [
    ['drive root', '/C:'],
    ['Windows', '/C:/Windows'],
    ['Program Files', '/C:/Program Files'],
    ['user profiles', '/C:/Users'],
  ]) {
    r = await request('/api/files/delete?path=' + encodeURIComponent(target) + '&scope=system', 'DELETE');
    check(`protected ${label} delete rejected`, r.response.status === 400);
  }
  r = await request('/api/files/delete?path=' + encodeURIComponent(apiPath(DATA)) + '&scope=system', 'DELETE');
  check('Kenji data directory delete rejected', r.response.status === 400);
  r = await request(
    '/api/files/delete?path=' +
      encodeURIComponent(apiPath(path.join(DATA, 'auth.json'))) +
      '&scope=system',
    'DELETE',
  );
  check('Kenji auth.json delete rejected', r.response.status === 400);
  r = await request(
    `/api/files/upload?path=${encodeURIComponent(apiPath(DATA))}&scope=system`,
    'POST',
    uploadForm('overwrite', 'auth.json'),
  );
  check('Kenji auth.json overwrite rejected', r.response.status === 400);

  // ---- non-admin isolation ----
  r = await request('/api/users', 'POST', {
    username: OPERATOR.username,
    password: OPERATOR.password,
    role: 'operator',
  });
  check('create operator', r.response.ok);
  const opToken = await login(OPERATOR);
  check('operator login', !!opToken);

  r = await request('/api/files/list?scope=system&path=/', 'GET', undefined, opToken);
  check('operator system list forbidden', r.response.status === 403);
  r = await request('/api/files/delete?path=/C:/Windows&scope=system', 'DELETE', undefined, opToken);
  check('operator system delete forbidden', r.response.status === 403);
  r = await request(
    `/api/files/upload?path=${encodeURIComponent(apiPath(DATA))}&scope=system`,
    'POST',
    uploadForm('nope', 'nope.txt'),
    opToken,
  );
  check('operator system upload forbidden', r.response.status === 403);
  r = await request('/api/files/mkdir', 'POST', { path: '/x', scope: 'system' }, opToken);
  check('operator system mkdir forbidden', r.response.status === 403);

  // ---- workspace regression (both roles) ----
  r = await request('/api/files/list?path=/', 'GET', undefined, opToken);
  check('operator workspace list', r.response.ok && r.body.scope === 'workspace');
  r = await request('/api/files/mkdir', 'POST', { path: '/op-smoke' }, opToken);
  check('operator workspace mkdir', r.response.ok);
  r = await request(
    '/api/files/upload?path=%2Fop-smoke',
    'POST',
    uploadForm('operator-workspace'),
    opToken,
  );
  check('operator workspace upload', r.response.ok);
  r = await request(
    `/api/files/download?path=%2Fop-smoke%2Fsmoke.txt&token=${encodeURIComponent(opToken)}`,
    'GET',
    undefined,
    '',
  );
  check('operator workspace download', r.response.ok && r.text === 'operator-workspace');
  r = await request('/api/files/delete?path=%2Fop-smoke', 'DELETE', undefined, opToken);
  check('operator workspace delete', r.response.ok);

  r = await request('/api/files/list?path=/');
  check('admin workspace list', r.response.ok && r.body.scope === 'workspace');
  r = await request('/api/files/mkdir', 'POST', { path: '/admin-smoke' });
  check('admin workspace mkdir', r.response.ok);
  r = await request('/api/files/delete?path=%2Fadmin-smoke', 'DELETE');
  check('admin workspace delete', r.response.ok);

  console.log(`RESULT: ${checks} system-scope checks passed`);
} finally {
  if (server && server.exitCode === null) {
    server.kill();
    await new Promise((resolve) => {
      const timer = setTimeout(resolve, 5000);
      server.once('exit', () => {
        clearTimeout(timer);
        resolve();
      });
    });
  }
  fs.rmSync(DATA, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
}
