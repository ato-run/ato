/** Full database backup for the pinned Wakerun schema.
 *
 * Callers must hold the adapter's exclusive database lock for the entire
 * operation and use a single SQLite connection. Backups contain sensitive
 * data, including any AI API key. They must be stored and disclosed as such.
 */
type Scalar = string | number | null;
type Row = Record<string, Scalar>;

export interface Database {
  select<T>(query: string, bindValues?: Scalar[]): Promise<T>;
  execute(query: string, bindValues?: Scalar[]): Promise<unknown>;
}

// Parents precede children; reverse this order for deletion.
const TABLES = [
  "settings", "categories", "contacts", "accounts", "fixed_assets",
  "deductions", "advice_log", "allocation_rules", "transactions",
  "journal_entries", "recurring_rules", "invoices", "invoice_items",
] as const;
const AUTOINCREMENT_TABLES = TABLES.filter((name) => name !== "settings");

interface Backup {
  format: "wakerun-full";
  version: 2;
  tables: Record<string, Row[]>;
  sequences: Row[];
}

function quote(identifier: string): string {
  return `"${identifier.replace(/"/g, '""')}"`;
}

function validScalar(value: unknown): value is Scalar {
  return value === null || typeof value === "string" ||
    (typeof value === "number" && Number.isFinite(value));
}

async function columns(db: Database, table: string): Promise<string[]> {
  const info = await db.select<Array<{ name: string }>>(`PRAGMA table_info(${quote(table)})`);
  if (!info.length) throw new Error(`Missing table: ${table}`);
  return info.map((column) => column.name);
}

async function assertSchema(db: Database): Promise<void> {
  const entries = await db.select<Array<{ name: string }>>(
    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
  );
  const actual = entries.map((entry) => entry.name).filter((name) => name !== "_sqlx_migrations");
  if (actual.length !== TABLES.length || TABLES.some((name) => !actual.includes(name))) {
    throw new Error("Unsupported database schema");
  }
}

function validateRows(rows: unknown, expected: string[], table: string): asserts rows is Row[] {
  if (!Array.isArray(rows)) throw new Error(`Invalid table: ${table}`);
  for (const row of rows) {
    if (!row || typeof row !== "object" || Array.isArray(row)) throw new Error(`Invalid row: ${table}`);
    const keys = Object.keys(row);
    if (keys.length !== expected.length || expected.some((key) => !keys.includes(key)) ||
        keys.some((key) => !validScalar(row[key]))) throw new Error(`Invalid columns or values: ${table}`);
  }
}

function validateSequences(rows: unknown): asserts rows is Row[] {
  if (!Array.isArray(rows)) throw new Error("Invalid SQLite sequence");
  const names = new Set<string>();
  for (const row of rows) {
    if (!row || typeof row !== "object" || Array.isArray(row) ||
        Object.keys(row).sort().join(",") !== "name,seq" ||
        typeof row.name !== "string" || !AUTOINCREMENT_TABLES.includes(row.name as never) ||
        names.has(row.name) || !Number.isSafeInteger(row.seq) || row.seq < 0) {
      throw new Error("Invalid SQLite sequence");
    }
    names.add(row.name);
  }
}

async function rollback(db: Database): Promise<void> {
  await db.execute("ROLLBACK");
}

export async function makeBackup(db: Database): Promise<string> {
  await db.execute("BEGIN");
  try {
    await assertSchema(db);
    const tables: Record<string, Row[]> = {};
    for (const table of TABLES) {
      const expected = await columns(db, table);
      tables[table] = await db.select<Row[]>(`SELECT * FROM ${quote(table)}`);
      validateRows(tables[table], expected, table);
    }
    const sequences = await db.select<Row[]>("SELECT name, seq FROM sqlite_sequence");
    validateSequences(sequences);
    if (tables.settings.length !== 1 || tables.settings[0].id !== 1) {
      throw new Error("Invalid settings record");
    }
    const result: Backup = { format: "wakerun-full", version: 2, tables, sequences };
    const encoded = JSON.stringify(result);
    await db.execute("COMMIT");
    return encoded;
  } catch (error) {
    await rollback(db);
    throw error;
  }
}

export async function restoreBackup(db: Database, json: string): Promise<void> {
  const data: unknown = JSON.parse(json);
  if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error("Invalid backup");
  const backup = data as Partial<Backup>;
  if (backup.format !== "wakerun-full" || backup.version !== 2 ||
      !backup.tables || typeof backup.tables !== "object" || Array.isArray(backup.tables) ||
      !Array.isArray(backup.sequences)) throw new Error("Unsupported or incomplete backup");
  const tableKeys = Object.keys(backup.tables);
  if (tableKeys.length !== TABLES.length || TABLES.some((table) => !tableKeys.includes(table))) {
    throw new Error("Backup does not contain every required table");
  }

  await db.execute("BEGIN");
  try {
    await db.execute("PRAGMA defer_foreign_keys = ON");
    await assertSchema(db);
    const schemas: Record<string, string[]> = {};
    for (const table of TABLES) {
      schemas[table] = await columns(db, table);
      const rows = backup.tables[table];
      validateRows(rows, schemas[table], table);
    }
    if (backup.tables.settings.length !== 1 || backup.tables.settings[0].id !== 1) {
      throw new Error("Invalid settings record");
    }
    validateSequences(backup.sequences);
    for (const table of [...TABLES].reverse()) await db.execute(`DELETE FROM ${quote(table)}`);
    for (const table of TABLES) {
      const keys = schemas[table];
      const placeholders = keys.map((_, index) => `$${index + 1}`).join(", ");
      const statement = `INSERT INTO ${quote(table)} (${keys.map(quote).join(", ")}) VALUES (${placeholders})`;
      for (const row of backup.tables[table]) await db.execute(statement, keys.map((key) => row[key]));
    }
    await db.execute("DELETE FROM sqlite_sequence");
    for (const row of backup.sequences) {
      await db.execute("INSERT INTO sqlite_sequence (name, seq) VALUES ($1, $2)", [row.name, row.seq]);
    }
    const violations = await db.select<unknown[]>("PRAGMA foreign_key_check");
    if (violations.length) throw new Error("Backup has invalid foreign keys");
    await db.execute("COMMIT");
  } catch (error) {
    await rollback(db);
    throw error;
  }
}
