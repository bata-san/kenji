// 自動アップデート統合テスト: フェイクGitHub APIサーバーを立て、新規インスタンスで
// check → install → バイナリ入れ替え → update-finalizeヘルパーによる自動再起動までを検証する。
// Usage: node scripts/update_smoke.mjs  （リポジトリルートから。cargo build 済みであること）
import http from 'node:http';
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { spawn, execSync } from 'node:child_process';

const ROOT = process.cwd();
// リリースバイナリを優先（デバッグバイナリは128MB上限を超えうる）
const candidates = ['target/release/kenji.exe', 'target/debug/kenji.exe'];
const EXE = candidates.find((p) => fs.existsSync(path.join(ROOT, p)));
const DATA = path.join(ROOT, '.data-update');
const PORT = 8790;
const FAKE_PORT = 8791;
const BASE = `http://127.0.0.1:${PORT}`;
const MAX_EXE = 128 * 1024 * 1024;

if (!EXE) {
  console.error('FAIL: kenji.exe がありません。先に cargo build を実行してください。');
  process.exit(1);
}
const exePath = path.join(ROOT, EXE);
const exeBytes = fs.readFileSync(exePath);
console.log(`binary: ${EXE} (${(exeBytes.length / 1024 / 1024).toFixed(1)} MB)`);
if (exeBytes.length > MAX_EXE) {
  console.error('FAIL: デバッグバイナリが128MB上限を超えています。cargo build --release してください。');
  process.exit(1);
}
const exeSha = crypto.createHash('sha256').update(exeBytes).digest('hex');

let failed = 0;
const check = (name, ok) => {
  console.log(`  ${ok ? 'PASS' : 'FAIL'} ${name}`);
  if (!ok) failed++;
};

// --- フェイクGitHub API（releases/latest + バイナリ + sha256） ---
const fake = http.createServer((req, res) => {
  const url = req.url ?? '';
  if (url.endsWith('/releases/latest')) {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({
      tag_name: 'v0.9.9',
      html_url: `http://127.0.0.1:${FAKE_PORT}/bata-san/kenji/releases/tag/v0.9.9`,
      assets: [
        {
          name: 'kenji-windows-amd64.exe',
          browser_download_url: `http://127.0.0.1:${FAKE_PORT}/dl/kenji-windows-amd64.exe`,
        },
        {
          name: 'kenji-windows-amd64.exe.sha256',
          browser_download_url: `http://127.0.0.1:${FAKE_PORT}/dl/kenji-windows-amd64.exe.sha256`,
        },
      ],
    }));
  } else if (url.endsWith('/dl/kenji-windows-amd64.exe')) {
    res.writeHead(200, { 'Content-Type': 'application/octet-stream' });
    res.end(exeBytes);
  } else if (url.endsWith('/dl/kenji-windows-amd64.exe.sha256')) {
    res.writeHead(200, { 'Content-Type': 'text/plain' });
    res.end(`${exeSha}  kenji-windows-amd64.exe\n`);
  } else {
    res.writeHead(404, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ message: 'Not Found' }));
  }
});
await new Promise((r) => fake.listen(FAKE_PORT, '127.0.0.1', r));
console.log(`fake GitHub API on http://127.0.0.1:${FAKE_PORT}`);

async function health(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${BASE}/api/health`);
      if (r.ok) return true;
    } catch {
      // 未接続 → リトライ
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  return false;
}

async function login() {
  const r = await fetch(`${BASE}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'test123456' }),
  });
  const b = await r.json();
  return b.token;
}

let child = null;
try {
  // --- 新規インスタンス ---
  fs.rmSync(DATA, { recursive: true, force: true });
  execSync(`"${exePath}" setup --data-dir "${DATA}" --username admin --password test123456`, { stdio: 'pipe' });
  child = spawn(exePath, ['serve', '--data-dir', DATA, '--port', String(PORT)], {
    env: { ...process.env, KENJI_UPDATE_API: `http://127.0.0.1:${FAKE_PORT}` },
    stdio: 'ignore',
  });

  if (!(await health(15000))) throw new Error('サーバーが起動しません');
  // ポート衝突などで子プロセスが即死していないか（古いサーバーに繋って偽陽性化するのを防ぐ）
  if (child.exitCode !== null) throw new Error(`サーバーが異常終了しました (exit=${child.exitCode})。ポート ${PORT} が使用中でないか確認してください。`);
  console.log('server up');
  const token = await login();
  const auth = { Authorization: `Bearer ${token}` };

  // --- status ---
  const st = await fetch(`${BASE}/api/update/status`, { headers: auth });
  if (!st.ok) {
    throw new Error(`/api/update/status → HTTP ${st.status}。バイナリが古い（アップデート機能未実装）可能性があります。cargo build してください。`);
  }
  let s = await st.json();
  check('status current_version=0.1.0', s.current_version === '0.1.0');
  check('status idle', s.status === 'idle');
  check('status repo=bata-san/kenji', s.settings?.repo === 'bata-san/kenji');

  // --- check ---
  const ck = await fetch(`${BASE}/api/update/check`, { method: 'POST', headers: auth });
  if (!ck.ok) throw new Error(`/api/update/check → HTTP ${ck.status}`);
  s = await ck.json();
  check('check latest=v0.9.9', s.latest?.version === '0.9.9');
  check('check available=true', s.available === true);
  check('check last_check_at set', !!s.last_check_at);
  check('check no error', s.error == null);

  // --- install → graceful shutdown → finalize → 再起動 ---
  const t0 = Date.now();
  const r = await fetch(`${BASE}/api/update/install`, { method: 'POST', headers: auth });
  check('install 200', r.status === 200);
  const ib = await r.json().catch(() => ({}));
  check('install body restarting', ib.restarting === true);

  const back = await health(60000);
  check('restarted & health ok', back);
  console.log(`  (restart took ${((Date.now() - t0) / 1000).toFixed(1)}s)`);

  const token2 = await login();
  s = await (await fetch(`${BASE}/api/update/status`, { headers: { Authorization: `Bearer ${token2}` } })).json();
  check('new process serves status', s.current_version === '0.1.0');
  check('new process no stale error', s.error == null);

  const logPath = path.join(DATA, 'updates', 'finalize.log');
  const log = fs.existsSync(logPath) ? fs.readFileSync(logPath, 'utf8') : '';
  check('finalize.log: 再起動成功', log.includes('新バイナリを再起動しました'));
} catch (e) {
  console.error('FAIL:', e.message);
  failed++;
} finally {
  fake.close();
  if (child && !child.killed) {
    try { child.kill(); } catch { /* すでに停止済み */ }
  }
  // 再起動後のプロセス（detachedで親を持たない）をポートから特定して停止
  try {
    const out = execSync(`netstat -ano | findstr :${PORT}`).toString();
    const pids = new Set([...out.matchAll(/LISTENING\s+(\d+)/g)].map((m) => m[1]));
    for (const pid of pids) {
      try { execSync(`taskkill /PID ${pid} /F`, { stdio: 'pipe' }); } catch { /* 既に停止 */ }
    }
  } catch { /* 接続なし */ }
  console.log(failed === 0 ? 'RESULT: update smoke PASSED' : `RESULT: update smoke FAILED (${failed})`);
  process.exit(failed === 0 ? 0 : 1);
}
