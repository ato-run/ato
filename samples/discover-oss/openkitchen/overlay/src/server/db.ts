import type { Database } from 'sql.js';

// One database is installed while the browser adapter holds its exclusive
// origin-wide lock. Each SQL statement is executed against that database.
let current: Database | undefined;
export function setLocalDatabase(database: Database | undefined): void { current = database; }
function execute(sql: string, params: unknown[] = []) {
  if (!current) throw new Error('Local database is unavailable');
  const stmt = current.prepare(sql);
  try {
    stmt.bind(params.map((value) => typeof value === 'boolean' ? Number(value) : value) as any[]);
    const rows: Record<string, unknown>[] = [];
    while (stmt.step()) rows.push(stmt.getAsObject());
    return { rows, changes: current.getRowsModified() };
  } finally { stmt.free(); }
}
export async function query<T = Record<string, unknown>>(sql: string, params: unknown[] = []): Promise<T[]> {
  return execute(sql, params).rows as T[];
}
export async function get<T = Record<string, unknown>>(sql: string, params: unknown[] = []): Promise<T | undefined> {
  return (await query<T>(sql, params))[0];
}
export async function run(sql: string, params: unknown[] = []): Promise<{changes: number; lastInsertRowid: number}> {
  return {changes: execute(sql, params).changes, lastInsertRowid: 0};
}
