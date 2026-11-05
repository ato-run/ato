'use strict';

// Synthetic, isolated integration probe. Do not point this at a real instance.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const http = require('node:http');
// Node's global WebSocket cannot set Origin, and socketcluster-client ignores
// wsOptions when that global exists. Use its bundled ws implementation so the
// synthetic origin matches what a browser sends.
delete globalThis.WebSocket;
const socketClusterClient = require('/opt/fleetbase-socket/node_modules/socketcluster-client');

const origin = 'http://localhost:4200';
const base = 'http://console:4200';
const timeout = (promise, label, ms = 10000) => {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`Timeout: ${label}`)), ms); }),
  ]).finally(() => clearTimeout(timer));
};

async function post(path, body, bearer) {
  const response = await fetch(base + path, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Accept: 'application/json',
      Host: 'localhost:4200',
      'X-Forwarded-Proto': 'http',
      ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}),
    },
    body: JSON.stringify(body),
  });
  const json = await response.json();
  if (!response.ok) {
    throw new Error(`${path}: HTTP ${response.status}; validation fields: ${Object.keys(json.errors || {}).join(',')}`);
  }
  return json;
}

function memoryAuthEngine(initial) {
  let token = initial;
  return {
    async saveToken(_name, value) { token = value; return value; },
    async removeToken() { const old = token; token = null; return old; },
    async loadToken() { return token; },
  };
}

function socket(token, socketOrigin = origin) {
  return socketClusterClient.create({
    hostname: 'console', port: 4200, path: '/socketcluster/', autoReconnect: false,
    ackTimeout: 5000, authEngine: memoryAuthEngine(token),
    wsOptions: { headers: { Origin: socketOrigin } },
  });
}

async function subscribe(client, name) {
  const channel = client.subscribe(name);
  const outcome = await timeout(Promise.race([
    channel.listener('subscribe').once().then(() => ({ ok: true })),
    channel.listener('subscribeFail').once().then(({ error }) => ({ ok: false, reason: error.reason })),
  ]), 'subscription');
  return { channel, ...outcome };
}

function handshakeStatus(value) {
  return timeout(new Promise((resolve, reject) => {
    const req = http.request({
      host: 'console', port: 4200, path: '/socketcluster/',
      headers: { Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Version': '13',
        'Sec-WebSocket-Key': crypto.randomBytes(16).toString('base64'), Origin: value },
    });
    req.on('upgrade', (response, connection) => { connection.destroy(); resolve(response.statusCode); });
    req.on('response', response => { response.resume(); resolve(response.statusCode); });
    req.on('error', reject);
    req.end();
  }), 'handshake');
}

async function main() {
  const clients = [];
  try {
    const deadline = Date.now() + 300000;
    let ready = false;
    while (Date.now() < deadline) {
      try {
        const response = await fetch(`${base}/int/v1/onboard/should-onboard`, {
          headers: { Host: 'localhost:4200', 'X-Forwarded-Proto': 'http' },
          signal: AbortSignal.timeout(3000),
        });
        if (response.ok) { ready = true; break; }
      } catch (_) { /* Initialization is still in progress. */ }
      await new Promise(resolve => setTimeout(resolve, 1000));
    }
    assert.equal(ready, true, 'isolated application did not become ready');
    const password = `Proof!9aA${crypto.randomBytes(18).toString('hex')}`;
    const account = await post('/int/v1/onboard/create-account', {
      name: 'Alice Sample', email: 'alice@synthetic.invalid', phone: '+12025550123',
      organization_name: 'Synthetic Logistics', password, password_confirmation: password,
      hosted_routing: false,
    });
    assert.equal(account.status, 'success');
    assert.equal(account.skipVerification, true);
    assert.ok(typeof account.token === 'string');
    const userId = Buffer.from(account.session, 'base64').toString('utf8');
    assert.match(userId, /^[a-zA-Z0-9_-]+$/);
    const otherAccount = await post('/int/v1/onboard/create-account', {
      name: 'Bob Sample', email: 'bob@synthetic.invalid', phone: '+12025550124',
      organization_name: 'Other Logistics', password, password_confirmation: password,
      hosted_routing: false,
    });
    assert.equal(otherAccount.status, 'success');
    const otherId = Buffer.from(otherAccount.session, 'base64').toString('utf8');
    assert.match(otherId, /^[a-zA-Z0-9_-]+$/);
    assert.notEqual(userId, otherId);

    const minted = await post('/int/v1/socket/token', {}, account.token);
    assert.ok(typeof minted.token === 'string' && minted.expires_in > 0);
    assert.equal(await handshakeStatus(origin), 101);
    assert.equal(await handshakeStatus('http://attacker.invalid:4200'), 403);

    const anonymous = socket(null);
    clients.push(anonymous);
    const anonymousStatus = await timeout(anonymous.listener('connect').once(), 'anonymous connect');
    assert.equal(anonymousStatus.isAuthenticated, false);
    const deniedAnonymous = await subscribe(anonymous, `user.${userId}`);
    assert.equal(deniedAnonymous.ok, false);
    assert.equal(deniedAnonymous.reason, 'no_token');

    const authenticated = socket(minted.token);
    clients.push(authenticated);
    const status = await timeout(authenticated.listener('connect').once(), 'authenticated connect');
    assert.equal(status.isAuthenticated, true);
    const own = await subscribe(authenticated, `user.${userId}`);
    assert.equal(own.ok, true);
    const unrelated = await subscribe(authenticated, `user.${otherId}`);
    assert.equal(unrelated.ok, false);

    // A test channel requires the actual API authorizer; then the API's signed
    // publisher must deliver through nginx to this same authenticated socket.
    const testChannel = await subscribe(authenticated, `test.${userId}`);
    assert.equal(testChannel.ok, true);
    const received = timeout(testChannel.channel.createConsumer().next(), 'published message');
    const sent = await post('/int/v1/settings/test-socket', { channel: 'test.someone-else' }, account.token);
    assert.equal(sent.status, 'success');
    assert.equal(sent.channel, `test.${userId}`);
    const message = await received;
    assert.equal(message.value.message, 'Hello World');
    assert.equal(message.value.sender, 'Fleetbase');
    console.log('PASS: accounts, socket token, origin restriction, anonymous and cross-organization denial, authorized API publish and websocket delivery');
  } finally {
    for (const client of clients) {
      client.disconnect();
      client.killAllListeners();
      client.killAllChannels();
    }
  }
}

main().catch(error => { console.error(error.message); process.exitCode = 1; });
