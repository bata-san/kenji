import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validateRequest } from './worker.mjs';
import https from 'node:https';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn, execFileSync } from 'node:child_process';
const valid = { provider: 'kenji-custom', model: 'test', base_url: 'https://example.com/v1', api_key_env: 'KENJI_REPAIR_API_KEY', incident: 'exit code 1' };
test('validates provider and credential references', () => {
  validateRequest(valid);
  for (const patch of [{ base_url: 'http://example.com' }, { base_url: 'https://secret@example.com' }, { api_key_env: '!print key' }, { model: '' }, { incident: 'x'.repeat(4097) }]) {
    assert.throws(() => validateRequest({ ...valid, ...patch }));
  }
});
test('pinned pi SDK exposes required lifecycle without executing tools', async () => {
  const sdk = await import('@earendil-works/pi-coding-agent');
  assert.equal(typeof sdk.createAgentSession, 'function');
  assert.equal(typeof sdk.ModelRuntime.create, 'function');
  assert.equal(typeof sdk.SettingsManager.inMemory, 'function');
});

test('real pi session uses a mock HTTPS provider with no tools or credential leaks', { timeout: 30000 }, async () => {
  const dir = await mkdtemp(join(tmpdir(), 'kenji-pi-test-'));
  let server;
  try {
    const cert = join(dir, 'cert.pem'), key = join(dir, 'key.pem');
    execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', cert, '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1'], { stdio: 'ignore' });
    let request;
    server = https.createServer({ cert: await readFile(cert), key: await readFile(key) }, async (req, res) => {
      let raw = ''; for await (const c of req) raw += c;
      request = { url: req.url, auth: req.headers.authorization, body: JSON.parse(raw) };
      res.writeHead(200, { 'Content-Type': 'text/event-stream' });
      const chunk = (delta, finish_reason = null) => `data: ${JSON.stringify({ id: 'fake-1', object: 'chat.completion.chunk', created: 1, model: 'test', choices: [{ index: 0, delta, finish_reason }] })}\n\n`;
      res.end(chunk({ role: 'assistant', content: 'Mock diagnostic proposal. No files changed.' }) + chunk({}, 'stop') + 'data: [DONE]\n\n');
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    const child = spawn(process.execPath, [fileURLToPath(new URL('./worker.mjs', import.meta.url))], {
      env: { ...process.env, NODE_EXTRA_CA_CERTS: cert, KENJI_PI_TEST_KEY: 'fake-test-credential' }, stdio: ['pipe', 'pipe', 'pipe'],
    });
    let stdout = '', stderr = '';
    child.stdout.on('data', c => stdout += c); child.stderr.on('data', c => stderr += c);
    child.stdin.end(JSON.stringify({ ...valid, base_url: `https://127.0.0.1:${server.address().port}/v1`, api_key_env: 'KENJI_PI_TEST_KEY' }));
    const exit = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
    assert.equal(exit, 0, stderr);
    assert.match(stdout, /Mock diagnostic proposal/);
    assert.equal(request.auth, 'Bearer fake-test-credential');
    assert.ok(!request.url.includes('credential'));
    assert.ok(!request.body.tools?.length);
    assert.ok(!stdout.includes('fake-test-credential'));
  } finally {
    await new Promise(resolve => server ? server.close(resolve) : resolve());
    await rm(dir, { recursive: true, force: true });
  }
});
