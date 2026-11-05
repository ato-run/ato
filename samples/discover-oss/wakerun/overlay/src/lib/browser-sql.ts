/// <reference types="vite/client" />
import initSqlJs, { type Database as SqlDatabase, type SqlJsStatic } from "sql.js";
import wasmUrl from "sql.js/dist/sql-wasm.wasm?url";
import migration1 from "../../src-tauri/migrations/001_init.sql?raw";
import migration2 from "../../src-tauri/migrations/002_seed_categories.sql?raw";
import migration3 from "../../src-tauri/migrations/003_transactions_to_account.sql?raw";
import migration4 from "../../src-tauri/migrations/004_add_profile_fields.sql?raw";
import migration5 from "../../src-tauri/migrations/005_recurring_and_invoices.sql?raw";

type BindValue = string | number | Uint8Array | null;
type Bindings = Record<string, BindValue>;
const migrations = [migration1, migration2, migration3, migration4, migration5];
const sqlite = initSqlJs({ locateFile: () => wasmUrl });
const idb = new Promise<IDBDatabase>((resolve, reject) => {
  const request = indexedDB.open("wakerun-browser-v1", 1);
  request.onupgradeneeded = () => request.result.createObjectStore("data");
  request.onsuccess = () => resolve(request.result);
  request.onerror = () => reject(request.error);
  request.onblocked = () => reject(new Error("Database open blocked by another tab"));
});

async function readBytes(): Promise<Uint8Array | undefined> {
  const database = await idb;
  return new Promise((resolve, reject) => {
    const transaction = database.transaction("data", "readonly");
    const request = transaction.objectStore("data").get("sqlite");
    request.onsuccess = () => resolve(request.result as Uint8Array | undefined);
    request.onerror = () => reject(request.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

async function writeBytes(bytes: Uint8Array): Promise<void> {
  const database = await idb;
  return new Promise((resolve, reject) => {
    const transaction = database.transaction("data", "readwrite");
    transaction.objectStore("data").put(bytes, "sqlite");
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

function bindings(values: unknown[]): Bindings {
  return Object.fromEntries(values.map((value, index) => {
    if (!(value === null || typeof value === "string" ||
        (typeof value === "number" && Number.isFinite(value) &&
          (!Number.isInteger(value) || Number.isSafeInteger(value))) || value instanceof Uint8Array)) {
      throw new Error(`Unsupported SQLite parameter ${index + 1}`);
    }
    return [`$${index + 1}`, value];
  }));
}

class Session {
  dirty: boolean;
  inTransaction = false;
  constructor(readonly db: SqlDatabase, fresh: boolean) { this.dirty = fresh; }

  async select<T>(query: string, values: unknown[] = []): Promise<T> {
    const statement = this.db.prepare(query, bindings(values));
    const rows: Record<string, unknown>[] = [];
    try {
      while (statement.step()) rows.push(statement.getAsObject());
    } finally {
      statement.free();
    }
    return rows as T;
  }

  async execute(query: string, values: unknown[] = []): Promise<{ rowsAffected: number; lastInsertId: number }> {
    const command = query.trim().toUpperCase().replace(/;$/, "");
    this.db.run(query, bindings(values));
    if (command === "BEGIN" || command.startsWith("BEGIN ")) this.inTransaction = true;
    else if (command === "COMMIT" || command === "ROLLBACK") this.inTransaction = false;
    else this.dirty = true;
    const result = this.db.exec("SELECT last_insert_rowid() AS id");
    return { rowsAffected: this.db.getRowsModified(), lastInsertId: Number(result[0]?.values[0]?.[0] ?? 0) };
  }
}

let active: Session | undefined;

/** Serialize a complete business operation across tabs, persisting atomically. */
export async function withDatabase<T>(operation: () => Promise<T>): Promise<T> {
  if (!navigator.locks) throw new Error("This browser cannot safely coordinate the database across tabs");
  return navigator.locks.request("wakerun-browser-v1", async () => {
    const engine: SqlJsStatic = await sqlite;
    const bytes = await readBytes();
    const db = bytes ? new engine.Database(bytes) : new engine.Database();
    try {
      db.run("PRAGMA foreign_keys = ON");
      if (!bytes) {
        db.run("BEGIN");
        for (const migration of migrations) db.run(migration);
        db.run("COMMIT");
      }
      const session = new Session(db, !bytes);
      active = session;
      const result = await operation();
      if (session.inTransaction) throw new Error("Unfinished SQLite transaction");
      if (session.dirty) await writeBytes(db.export());
      return result;
    } finally {
      active = undefined;
      db.close();
    }
  });
}

/** Only use this while holding withDatabase's exclusive lock. */
export async function lockedDatabase(): Promise<Session> {
  if (!active) throw new Error("Database operation requires an exclusive lock");
  return active;
}

/** Public individual-statement adapter for upstream modules outside db.ts. */
export class BrowserDatabase {
  async select<T>(query: string, values: unknown[] = []): Promise<T> {
    return withDatabase(async () => (await lockedDatabase()).select<T>(query, values));
  }
  async execute(query: string, values: unknown[] = []) {
    return withDatabase(async () => (await lockedDatabase()).execute(query, values));
  }
}

export function serialized<F extends (...args: never[]) => Promise<unknown>>(fn: F): F {
  return ((...args: never[]) => withDatabase(() => fn(...args))) as F;
}
