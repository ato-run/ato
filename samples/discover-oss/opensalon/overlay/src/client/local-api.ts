import initSqlJs from "sql.js";
import wasmUrl from "sql.js/dist/sql-wasm.wasm?url";
import schema from "../server/schema.sql?raw";
import app from "../server/index";
import { initDB } from "../server/db";
import { ensureSeeded } from "../server/seed";

const database = new Promise<IDBDatabase>((resolve, reject) => {
  const req = indexedDB.open("opensalon-local-v1", 1);
  req.onupgradeneeded = () => req.result.createObjectStore("sqlite");
  req.onsuccess = () => resolve(req.result);
  req.onerror = () => reject(req.error);
  req.onblocked = () => reject(new Error("OpenSalon storage is blocked by another tab"));
});
const sql = initSqlJs({ locateFile: () => wasmUrl });
let poisoned = false;
let initializedOnce = false;
async function loadSnapshot(): Promise<Uint8Array | undefined> {
  const db = await database;
  return await new Promise((resolve, reject) => {
    const tx = db.transaction("sqlite", "readonly");
    const req = tx.objectStore("sqlite").get("database");
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
    tx.onabort = () => reject(tx.error);
  });
}
async function saveSnapshot(bytes: Uint8Array): Promise<void> {
  const db = await database;
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction("sqlite", "readwrite");
    tx.objectStore("sqlite").put(bytes, "database");
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  });
}
export async function localApi(path: string, init?: RequestInit): Promise<Response> {
  if (!navigator.locks) throw new Error("OpenSalon requires Web Locks for safe local storage");
  return navigator.locks.request("opensalon-local-v1", async () => {
    if (poisoned) throw new Error("Storage failed; reload before continuing");
    const SQL = await sql;
    const bytes = await loadSnapshot();
    if (!bytes && initializedOnce) { poisoned = true; throw new Error("Saved database disappeared. Reload before continuing."); }
    const db = new SQL.Database(bytes);
    try {
      db.run("PRAGMA foreign_keys = ON");
      if (!bytes) db.run(schema);
      const storage = { async query(statement: string, params: unknown[] = []) {
        const stmt = db.prepare(statement);
        try {
          stmt.bind(params.map(value => typeof value === "boolean" ? Number(value) : value) as any[]);
          const rows: Record<string, unknown>[] = [];
          while (stmt.step()) rows.push(stmt.getAsObject());
          const changes = db.getRowsModified();
          const rowid = db.exec("SELECT last_insert_rowid() AS id")[0]?.values[0]?.[0] ?? 0;
          return { rows, meta: { changes, last_row_id: Number(rowid) } };
        } finally { stmt.free(); }
      }};
      initDB(storage);
      db.run("BEGIN IMMEDIATE");
      try {
        await ensureSeeded();
        const meta = await storage.query("SELECT count(*) AS n FROM _meta WHERE key IN ('appointment_counter', 'appointment_prefix')");
        if (meta.rows[0]?.n !== 2) throw new Error("Could not initialize sample data");
        db.run("COMMIT");
      } catch (error) { db.run("ROLLBACK"); throw error; }
      const req = new Request(new URL(path, window.location.origin), init);
      db.run("BEGIN IMMEDIATE");
      let response: Response;
      try { response = await app.fetch(req, { STORAGE: storage } as any); db.run(response.ok ? "COMMIT" : "ROLLBACK"); }
      catch (error) { db.run("ROLLBACK"); throw error; }
      try { await saveSnapshot(db.export()); initializedOnce = true; }
      catch (error) { poisoned = true; throw error; }
      return response;
    } catch (error) { poisoned = true; throw error; }
    finally { initDB(undefined); db.close(); }
  });
}
