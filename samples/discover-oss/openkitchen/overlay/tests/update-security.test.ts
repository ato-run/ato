import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { Hono } from 'hono';
import initSqlJs from 'sql.js';
import routes from '../src/server/routes';
import { setLocalDatabase } from '../src/server/db';

test('updates accept only declared fields and cannot alter unrelated rows', async () => {
  const wasm = process.env.OPENKITCHEN_SQLJS_WASM;
  if (!wasm) throw new Error('OPENKITCHEN_SQLJS_WASM is required');
  const SQL = await initSqlJs({ locateFile: () => wasm });
  const db = new SQL.Database();
  db.run(await readFile('src/server/schema.sql', 'utf8'));
  db.run("INSERT INTO ingredients (id, name) VALUES ('one', 'carrot'), ('two', 'onion')");
  setLocalDatabase(db);
  const app = new Hono();
  app.route('/', routes);
  const patch = (id: string, body: unknown) => app.request(`/api/ingredients/${id}`, {
    method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body),
  });
  try {
    const attack = await patch('%25', { 'name = ? WHERE id LIKE ? --': 'changed' });
    assert.equal(attack.status, 400);
    const rows = () => db.exec('SELECT id, name FROM ingredients ORDER BY id')[0].values;
    assert.deepEqual(rows(), [['one', 'carrot'], ['two', 'onion']]);
    assert.equal((await patch('one', { name: 'tomato', cost_cents: 120 })).status, 200);
    assert.deepEqual(rows(), [['one', 'tomato'], ['two', 'onion']]);
    for (const body of [{ id: 'other' }, { updated_at: 'forged' }, [], null]) {
      assert.equal((await patch('one', body)).status, 400);
    }
    assert.deepEqual(rows(), [['one', 'tomato'], ['two', 'onion']]);
  } finally {
    setLocalDatabase(undefined);
    db.close();
  }
});

test('all PATCH routes retain their intended fields and reject identifier changes', async () => {
  const wasm = process.env.OPENKITCHEN_SQLJS_WASM;
  if (!wasm) throw new Error('OPENKITCHEN_SQLJS_WASM is required');
  const SQL = await initSqlJs({ locateFile: () => wasm });
  const db = new SQL.Database();
  db.run(await readFile('src/server/schema.sql', 'utf8'));
  db.run("INSERT INTO suppliers (id, name) VALUES ('s', 'supplier')");
  db.run("INSERT INTO ingredients (id, name) VALUES ('i', 'ingredient')");
  db.run("INSERT INTO recipes (id, name) VALUES ('r', 'recipe')");
  db.run("INSERT INTO invoices (id) VALUES ('v')");
  db.run("INSERT INTO documents (id, name) VALUES ('d', 'document')");
  setLocalDatabase(db);
  const app = new Hono();
  app.route('/', routes);
  const cases = [
    ['suppliers', 's', { notes: 'supplier note' }],
    ['ingredients', 'i', { cost_cents: 135 }],
    ['recipes', 'r', { menu_price_cents: 600 }],
    ['invoices', 'v', { status: 'paid' }],
    ['documents', 'd', { reminded_at: '2026-01-01' }],
  ] as const;
  try {
    for (const [resource, id, body] of cases) {
      const patch = (value: unknown) => app.request(`/api/${resource}/${id}`, {
        method: 'PATCH', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(value),
      });
      assert.equal((await patch(body)).status, 200, resource);
      assert.equal((await patch({ ...body, id: 'other' })).status, 400, resource);
      assert.equal((await patch({ created_at: 'forged' })).status, 400, resource);
    }
    assert.equal(db.exec("SELECT cost_cents FROM ingredients WHERE id='i'")[0].values[0][0], 135);
    assert.equal(db.exec("SELECT status FROM invoices WHERE id='v'")[0].values[0][0], 'paid');
  } finally {
    setLocalDatabase(undefined);
    db.close();
  }
});
