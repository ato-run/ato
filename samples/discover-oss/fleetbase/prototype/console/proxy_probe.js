'use strict';

// An isolated echo backend and client for the console's proxy boundary.
const assert = require('node:assert/strict');
const http = require('node:http');

if (process.argv[2] === 'server') {
  http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(req.headers));
  }).listen(8000, '0.0.0.0');
} else {
  const forbidden = ['x-forwarded-for', 'x-forwarded-prefix', 'x-forwarded-server',
    'x-forwarded-scheme', 'x-forwarded-ssl', 'x-real-ip', 'x-original-host',
    'x-original-url', 'x-rewrite-url', 'forwarded'];
  async function request(name, host, headers = {}, path = '/int/v1/probe') {
    return new Promise((resolve, reject) => {
      const req = http.request({ hostname: name, port: 4200, path,
        headers: { Host: host, ...headers }, timeout: 3000 }, response => {
        const chunks = [];
        response.on('data', chunk => chunks.push(chunk));
        response.on('end', () => resolve({ status: response.statusCode,
          json: async () => JSON.parse(Buffer.concat(chunks).toString('utf8')) }));
      });
      req.on('timeout', () => req.destroy(new Error('request timeout')));
      req.on('error', reject);
      req.end();
    });
  }
  async function verify(name, host, scheme, port) {
    let response;
    for (let i = 0; i < 30; i++) {
      try { response = await request(name, host); break; }
      catch (_) { await new Promise(resolve => setTimeout(resolve, 200)); }
    }
    assert.ok(response, 'proxy did not start');
    const injected = Object.fromEntries(forbidden.map(key => [key, 'attacker.invalid']));
    const result = await request(name, host, { ...injected, 'X-Forwarded-Host': 'attacker.invalid',
      'X-Forwarded-Proto': 'ftp', 'X-Forwarded-Port': '9' });
    assert.equal(result.status, 200);
    const headers = await result.json();
    assert.equal(headers.host, host);
    assert.equal(headers['x-forwarded-host'], host);
    assert.equal(headers['x-forwarded-proto'], scheme);
    assert.equal(headers['x-forwarded-port'], port);
    for (const key of forbidden) assert.equal(headers[key], undefined, key);
    // Ato's readiness and Contract observers use an internal Host. Such a
    // request is accepted, but cannot choose what the upstream application sees.
    const otherHost = await request(name, 'attacker.invalid');
    assert.equal(otherHost.status, 200);
    const otherHeaders = await otherHost.json();
    assert.equal(otherHeaders.host, host);
    assert.equal(otherHeaders['x-forwarded-host'], host);
    assert.equal(otherHeaders['x-forwarded-proto'], scheme);
    assert.equal((await request(name, '127.0.0.1:12345', {}, '/')).status, 200);
  }
  (async () => {
    await verify('console-http', 'localhost:4200', 'http', '4200');
    await verify('console-https', 'tenant.synthetic.invalid:8443', 'https', '8443');
    await verify('console-https-default', 'tenant.synthetic.invalid', 'https', '443');
    console.log('PASS: upstream host and scheme fixed; forged forwarding headers removed; internal Host accepted');
  })().catch(error => { console.error(error.message); process.exitCode = 1; });
}
