// @vitest-environment jsdom
import "fake-indexeddb/auto";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { makeBackup, restoreBackup } from "../src/lib/full-backup";

// The browser smoke test exercises the WebAssembly build. This test uses the
// equivalent asm.js engine so Node can test IndexedDB failures deterministically.
vi.mock("sql.js", async () => {
  const module = await import("sql.js/dist/sql-asm.js");
  return { default: module.default };
});

let tail: Promise<unknown> = Promise.resolve();
Object.defineProperty(navigator, "locks", { value: {
  request(_name: string, callback: () => Promise<unknown>) {
    const current = tail.then(callback, callback);
    tail = current.catch(() => {});
    return current;
  },
} });

let database: typeof import("../src/lib/browser-sql");
beforeAll(async () => { database = await import("../src/lib/browser-sql"); });

async function getCounter(): Promise<number> {
  return database.withDatabase(async () => {
    const db = await database.lockedDatabase();
    const rows = await db.select<Array<{ value: number }>>(
      "SELECT invoice_next_number AS value FROM settings WHERE id = 1",
    );
    return rows[0].value;
  });
}

describe("browser SQLite persistence", () => {
  it("serializes concurrent read-modify-write operations", async () => {
    await Promise.all(Array.from({ length: 20 }, () => database.withDatabase(async () => {
      const db = await database.lockedDatabase();
      const rows = await db.select<Array<{ value: number }>>(
        "SELECT invoice_next_number AS value FROM settings WHERE id = 1",
      );
      await Promise.resolve();
      await db.execute("UPDATE settings SET invoice_next_number = $1 WHERE id = 1", [rows[0].value + 1]);
    })));
    expect(await getCounter()).toBe(21);
  });

  it("discards a failed operation and an unfinished transaction", async () => {
    const before = await getCounter();
    await expect(database.withDatabase(async () => {
      const db = await database.lockedDatabase();
      await db.execute("UPDATE settings SET invoice_next_number = 999 WHERE id = 1");
      throw new Error("synthetic failure");
    })).rejects.toThrow("synthetic failure");
    await expect(database.withDatabase(async () => {
      const db = await database.lockedDatabase();
      await db.execute("BEGIN");
      await db.execute("UPDATE settings SET invoice_next_number = 999 WHERE id = 1");
    })).rejects.toThrow("Unfinished SQLite transaction");
    expect(await getCounter()).toBe(before);
  });

  it("reports a storage failure and retains the last committed data", async () => {
    const before = await getCounter();
    const failure = vi.spyOn(IDBObjectStore.prototype, "put").mockImplementationOnce(() => {
      throw new DOMException("synthetic quota failure", "QuotaExceededError");
    });
    try {
      await expect(database.withDatabase(async () => {
        const db = await database.lockedDatabase();
        await db.execute("UPDATE settings SET invoice_next_number = 999 WHERE id = 1");
      })).rejects.toThrow("synthetic quota failure");
    } finally {
      failure.mockRestore();
    }
    expect(await getCounter()).toBe(before);
  });

  it("restores invoice and transaction data through the browser adapter", async () => {
    await database.withDatabase(async () => {
      const db = await database.lockedDatabase();
      await db.execute("INSERT INTO contacts (name) VALUES ($1)", ["Synthetic"]);
      await db.execute("INSERT INTO transactions (date, type, amount, account_id) VALUES ($1,$2,$3,$4)",
        ["2026-01-01", "expense", 1200, 1]);
      await db.execute("INSERT INTO invoices (invoice_number, issue_date, contact_id) VALUES ($1,$2,$3)",
        ["TEST-1", "2026-01-01", 1]);
      await db.execute("INSERT INTO invoice_items (invoice_id, description, amount) VALUES ($1,$2,$3)",
        [1, "Synthetic work", 10000]);
    });
    const backup = await database.withDatabase(async () => makeBackup(await database.lockedDatabase()));
    await database.withDatabase(async () => {
      const db = await database.lockedDatabase();
      await db.execute("UPDATE invoice_items SET amount = 1");
      await db.execute("UPDATE transactions SET amount = 1");
    });
    await database.withDatabase(async () => restoreBackup(await database.lockedDatabase(), backup));
    const values = await database.withDatabase(async () => {
      const db = await database.lockedDatabase();
      return {
        items: await db.select<Array<{ amount: number }>>("SELECT amount FROM invoice_items"),
        transactions: await db.select<Array<{ amount: number }>>("SELECT amount FROM transactions"),
      };
    });
    expect(values.items[0].amount).toBe(10000);
    expect(values.transactions[0].amount).toBe(1200);
  });
});
