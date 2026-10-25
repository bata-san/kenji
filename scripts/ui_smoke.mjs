// Headless UI smoke test: renders the built SPA against the live server via jsdom.
// Usage: node ui_smoke.mjs   (from web/; server must run at http://127.0.0.1:8787)
import { JSDOM, VirtualConsole } from 'jsdom';

const BASE = 'http://127.0.0.1:8787';
const USER = process.env.KENJI_USER ?? 'admin';
const PASS = process.env.KENJI_PASS ?? 'test123456';

const vc = new VirtualConsole();
const errors = [];
const logs = [];
vc.on('jsdomError', (e) => errors.push(`jsdomError: ${e.message}`));
vc.on('error', (...a) => errors.push('console.error: ' + a.join(' ')));
vc.on('log', (...a) => logs.push(a.join(' ')));

// 1. login on the real API
const loginRes = await fetch(`${BASE}/api/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS }),
});
if (!loginRes.ok) {
  console.error(`FAIL login (HTTP ${loginRes.status})`);
  process.exit(1);
}
const { token } = await loginRes.json();
console.log('login ok');

// 2. fetch the entry bundle from the real server
const html = await (await fetch(`${BASE}/`)).text();
const srcMatch = html.match(/src="(\/assets\/[^"]+\.js)"/);
if (!srcMatch) {
  console.error('FAIL: entry script not found in index.html');
  process.exit(1);
}
const bundle = await (await fetch(`${BASE}${srcMatch[1]}`)).text();

// 3. jsdom cannot run <script type="module">, so evaluate the single-chunk
//    export-free bundle directly in the page context.
const dom = new JSDOM(
  '<!doctype html><html lang="ja"><head><meta charset="UTF-8"><title>Kenji</title></head><body><div id="root"></div></body></html>',
  {
    url: `${BASE}/`,
    runScripts: 'dangerously',
    pretendToBeVisual: true,
    virtualConsole: vc,
    beforeParse(window) {
      // auth token so the app boots into the dashboard
      window.localStorage.setItem('kenji_token', token);
      // fetch polyfill bound to the real server (jsdom lacks fetch)
      window.fetch = (input, init) => {
        let url = typeof input === 'string' ? input : input.url;
        if (url.startsWith('/')) url = BASE + url;
        return fetch(url, init);
      };
      window.matchMedia =
        window.matchMedia ||
        (() => ({
          matches: false,
          addEventListener() {},
          removeEventListener() {},
          addListener() {},
          removeListener() {},
        }));
      class RO {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
      window.ResizeObserver = window.ResizeObserver || RO;
      window.scrollTo = () => {};
    },
  }
);

dom.window.eval(bundle);

// 4. wait for the app to mount and poll data
await new Promise((r) => setTimeout(r, 4000));

const doc = dom.window.document;
const text = doc.body.textContent || '';
console.log('TEXT:', JSON.stringify(text.slice(0, 400)));
console.log(`root children: ${doc.getElementById('root')?.children.length ?? -1}, body text length: ${text.length}`);
if (logs.length) {
  console.log('--- app console ---');
  for (const l of logs.slice(0, 10)) console.log('  ', l.slice(0, 200));
}

const checks = [
  ['sidebar ダッシュボード', text.includes('ダッシュボード')],
  ['sidebar ジョブ', text.includes('ジョブ')],
  ['sidebar ターミナル', text.includes('ターミナル')],
  ['sidebar ファイル', text.includes('ファイル')],
  ['sidebar 設定', text.includes('設定')],
  ['dashboard CPU', text.includes('CPU')],
  ['dashboard メモリ', text.includes('メモリ')],
  ['dashboard content rendered', text.length > 300],
];

let failed = 0;
for (const [name, ok] of checks) {
  console.log(`  ${ok ? 'PASS' : 'FAIL'} ${name}`);
  if (!ok) failed++;
}
const realErrors = errors.filter((e) => !e.includes('HTMLCanvasElement'));
if (realErrors.length > 0) {
  console.log('  script errors:');
  for (const e of realErrors.slice(0, 5)) console.log('   -', e.slice(0, 200));
  failed++;
}
console.log(failed === 0 ? 'RESULT: UI smoke PASSED' : `RESULT: UI smoke FAILED (${failed})`);
dom.window.close();
process.exit(failed === 0 ? 0 : 1);
