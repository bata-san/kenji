// Headless UI smoke for the Files page: verifies the workspace/system scope
// switch and the admin drive listing against the live server.
// Usage: KENJI_PASS=... node files_ui_smoke.mjs   (from web/, server on 8787)
import { JSDOM, VirtualConsole } from 'jsdom';

const BASE = 'http://127.0.0.1:8787';
const USER = process.env.KENJI_USER ?? 'admin';
const PASS = process.env.KENJI_PASS ?? 'test123456';

const vc = new VirtualConsole();
const errors = [];
vc.on('jsdomError', (e) => errors.push(`jsdomError: ${e.message}`));
vc.on('error', (...a) => errors.push('console.error: ' + a.join(' ')));

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

const html = await (await fetch(`${BASE}/`)).text();
const srcMatch = html.match(/src="(\/assets\/[^"]+\.js)"/);
if (!srcMatch) {
  console.error('FAIL: entry script not found in index.html');
  process.exit(1);
}
const bundle = await (await fetch(`${BASE}${srcMatch[1]}`)).text();

const dom = new JSDOM(
  '<!doctype html><html lang="ja"><head><meta charset="UTF-8"><title>Kenji</title></head><body><div id="root"></div></body></html>',
  {
    url: `${BASE}/files`,
    runScripts: 'dangerously',
    pretendToBeVisual: true,
    virtualConsole: vc,
    beforeParse(window) {
      window.localStorage.setItem('kenji_token', token);
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
  },
);

dom.window.eval(bundle);
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
await wait(3000);

const doc = dom.window.document;
const text = () => doc.body.textContent || '';
const findButton = (label) =>
  [...doc.querySelectorAll('button')].find((b) => (b.textContent || '').includes(label));

const checks = [];
const check = (name, ok) => {
  checks.push([name, ok]);
};

check('files page rendered', text().includes('マイワークスペース'));
check('PC全体 switch visible for admin', !!findButton('PC全体'));

const systemButton = findButton('PC全体');
systemButton?.dispatchEvent(new dom.window.MouseEvent('click', { bubbles: true }));
await wait(2500);

check('system warning banner', text().includes('PC全体モード（管理者）'));
check('drive list rendered', text().includes('C:'));
check('system path breadcrumb', text().includes('PC全体'));

const workspaceButton = findButton('マイワークスペース');
workspaceButton?.dispatchEvent(new dom.window.MouseEvent('click', { bubbles: true }));
await wait(2000);
check('back to workspace', !text().includes('PC全体モード（管理者）'));

const realErrors = errors.filter((e) => !e.includes('HTMLCanvasElement'));
if (realErrors.length > 0) {
  console.log('  script errors:');
  for (const e of realErrors.slice(0, 5)) console.log('   -', e.slice(0, 300));
  check('no script errors', false);
} else {
  check('no script errors', true);
}

let failed = 0;
for (const [name, ok] of checks) {
  console.log(`  ${ok ? 'PASS' : 'FAIL'} ${name}`);
  if (!ok) failed += 1;
}
console.log(failed === 0 ? 'RESULT: Files UI smoke PASSED' : `RESULT: Files UI smoke FAILED (${failed})`);
dom.window.close();
process.exit(failed === 0 ? 0 : 1);
