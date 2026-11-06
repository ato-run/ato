// Run: node --test samples/discover-oss/puter-compat/ato-puter-shim.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const j = (x) => JSON.parse(JSON.stringify(x));

function load() {
  const data = new Map();
  // Same key rule as ato-api claude_storage.ts validKey().
  const checkKey = (k) => assert.ok(k.length > 0 && !/[\s\\/'"]/.test(k), `invalid storage key: ${k}`);
  const storage = {
    async get(key, shared) {
      checkKey(key);
      assert.equal(shared, true);
      if (!data.has(key)) throw Object.assign(new Error('not found'), { code: 'not_found' });
      return { key, value: data.get(key), shared };
    },
    async set(key, value, shared) {
      checkKey(key);
      assert.equal(shared, true);
      assert.equal(typeof value, 'string');
      data.set(key, value);
      return { key, value, shared };
    },
    async delete(key, shared) {
      assert.equal(shared, true);
      data.delete(key);
      return { key, deleted: true, shared };
    },
    async list(prefix, shared) {
      assert.equal(shared, true);
      return { keys: [...data.keys()].filter((k) => k.startsWith(prefix || '')), prefix, shared };
    },
  };
  const ctx = { window: { storage }, location: { hostname: 'cinst-test.stg-app.ato.run' }, console, Blob, TextEncoder, btoa, atob, fetch: async () => null };
  ctx.globalThis = ctx;
  vm.createContext(ctx);
  vm.runInContext(readFileSync(new URL('./ato-puter-shim.js', import.meta.url), 'utf8'), ctx);
  return { puter: ctx.puter, data };
}

test('auth is signed in as the Instance', async () => {
  const { puter } = load();
  assert.equal(puter.auth.isSignedIn(), true);
  assert.equal((await puter.auth.getUser()).uuid, 'ato:cinst-test');
});

test('kv get/set/del/list follow the property-management backend', async () => {
  const { puter } = load();
  assert.equal(await puter.kv.get('pm1:settings'), null);
  await puter.kv.set('pm1:settings', { name: 'Acme' });
  assert.deepEqual(j(await puter.kv.get('pm1:settings')), { name: 'Acme' });
  await puter.kv.set([
    { key: 'pm1:r:leases:a', value: { id: 'a' } },
    { key: 'pm1:r:leases:b', value: { id: 'b' } },
    { key: 'pm1:r:tenants:c', value: { id: 'c' } },
  ]);
  const page = await puter.kv.list({ pattern: 'pm1:r:leases:', returnValues: true, limit: 1000 });
  assert.deepEqual(j(page.map((i) => i.value.id)), ['a', 'b']);
  assert.deepEqual(j(await puter.kv.list('pm1:r:*')), ['pm1:r:leases:a', 'pm1:r:leases:b', 'pm1:r:tenants:c']);
  await puter.kv.del('pm1:r:leases:a');
  assert.deepEqual(j((await puter.kv.list({ pattern: 'pm1:r:leases:' }))), ['pm1:r:leases:b']);
});

test('kv incr handles numbers, plain paths and quoted dotted names', async () => {
  const { puter } = load();
  assert.equal(await puter.kv.incr('n'), 1);
  assert.equal(await puter.kv.incr('n', 4), 5);
  assert.deepEqual(j(await puter.kv.incr('pm1:meta', { leases: 1, '["tx.2026-10"]': 1 })), { leases: 1, 'tx.2026-10': 1 });
  assert.deepEqual(j(await puter.kv.incr('pm1:meta', { leases: 1 })), { leases: 2, 'tx.2026-10': 1 });
  assert.deepEqual(j(await puter.kv.incr('pm1:seq', { lease: 1 })), { lease: 1 });
});

test('fs write/read/stat/readdir/delete round-trip bytes', async () => {
  const { puter } = load();
  const info = await puter.fs.write('Property Management/att/1/a.txt', new Blob(['hello'], { type: 'text/plain' }), { createMissingParents: true });
  assert.equal(info.size, 5);
  const blob = await puter.fs.read('/Property Management/att/1/a.txt');
  assert.equal(await blob.text(), 'hello');
  assert.equal(blob.type, 'text/plain');
  assert.deepEqual(j((await puter.fs.readdir('Property Management/att')).map((e) => e.name)), ['1']);
  await puter.fs.delete('Property Management/att');
  await assert.rejects(puter.fs.read('Property Management/att/1/a.txt'), /not found/);
});

test('keys with spaces, slashes and quotes are stored under valid keys', async () => {
  const { puter, data } = load();
  await puter.kv.set('pm1:settings', 1);
  assert.ok(data.has('puter1:kv:pm1:settings'), 'already-valid keys are unchanged');
  await puter.fs.write("contract-manager/docs/a b/it's \"x\".pdf", 'hi');
  assert.equal(await (await puter.fs.read("contract-manager/docs/a b/it's \"x\".pdf")).text(), 'hi');
  assert.deepEqual(j((await puter.fs.readdir('contract-manager/docs')).map((e) => e.name)), ['a b']);
  assert.deepEqual(j(await puter.kv.list('pm1:')), ['pm1:settings']);
});

test('fs refuses files over the limit and workers report unsupported', async () => {
  const { puter } = load();
  await assert.rejects(puter.fs.write('big.bin', new Uint8Array(4 * 1024 * 1024 + 1)), /larger than 4 MB/);
  await assert.rejects(puter.workers.exec('x'), /not available on ato.run/);
});
