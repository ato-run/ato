import initSqlJs from 'sql.js';
import wasmUrl from 'sql.js/dist/sql-wasm.wasm?url';
import schema from '../server/schema.sql?raw';
import { Hono } from 'hono';
import routes from '../server/routes';
import { ensureSeed } from '../server/domain/seed';
import { setLocalDatabase } from '../server/db';

const app = new Hono();
app.route('/', routes);

const database = new Promise<IDBDatabase>((resolve, reject) => {
  const req = indexedDB.open('openkitchen-local-v1', 1);
  req.onupgradeneeded = () => req.result.createObjectStore('sqlite');
  req.onsuccess = () => resolve(req.result);
  req.onerror = () => reject(req.error);
  req.onblocked = () => reject(new Error('OpenKitchen storage is blocked by another tab'));
});
const sql = initSqlJs({ locateFile: () => wasmUrl });
let poisoned = false;
let initializedOnce = false;

async function loadSnapshot(): Promise<Uint8Array | undefined> {
  const db = await database;
  return await new Promise((resolve, reject) => {
    const tx = db.transaction('sqlite', 'readonly');
    const req = tx.objectStore('sqlite').get('database');
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
    tx.onabort = () => reject(tx.error);
  });
}
async function saveSnapshot(bytes: Uint8Array): Promise<void> {
  const db = await database;
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction('sqlite', 'readwrite');
    tx.objectStore('sqlite').put(bytes, 'database');
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  });
}

export async function localApi(path: string, init?: RequestInit): Promise<Response> {
  if (!navigator.locks) throw new Error('OpenKitchen requires browser Web Locks for safe local storage');
  return navigator.locks.request('openkitchen-local-v1', async () => {
    if (poisoned) throw new Error('Storage failed; reload before continuing');
    const SQL = await sql;
    const bytes = await loadSnapshot();
    if (!bytes && initializedOnce) { poisoned = true; throw new Error('Saved database disappeared. Reload before continuing.'); }
    const db = new SQL.Database(bytes);
    try {
      db.run('PRAGMA foreign_keys = ON');
      if (!bytes) db.run(schema);
      setLocalDatabase(db);
      db.run('BEGIN IMMEDIATE');
      try { await ensureSeed(); db.run('COMMIT'); }
      catch (error) { db.run('ROLLBACK'); throw error; }
      const req = new Request(new URL(path, window.location.origin), init);
      db.run('BEGIN IMMEDIATE');
      let response: Response;
      try {
        response = await app.fetch(req);
        db.run(response.ok ? 'COMMIT' : 'ROLLBACK');
      } catch (error) { db.run('ROLLBACK'); throw error; }
      try { await saveSnapshot(db.export()); initializedOnce = true; }
      catch (error) { poisoned = true; throw error; }
      return response;
    } catch (error) { poisoned = true; throw error; }
    finally { setLocalDatabase(undefined); db.close(); }
  });
}
