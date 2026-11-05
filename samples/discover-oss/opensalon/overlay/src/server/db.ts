type Storage = { query(sql: string, params?: unknown[]): Promise<{rows: Record<string, unknown>[]; meta?: {changes?: number; last_row_id?: number}}> };
let storage: Storage | undefined;
export function initDB(value: Storage | undefined): void { storage = value; }
function current(): Storage { if (!storage) throw new Error("Local database unavailable"); return storage; }
export async function query<T = Record<string, unknown>>(sql: string, params: unknown[] = []): Promise<T[]> { return (await current().query(sql, params)).rows as T[]; }
export async function get<T = Record<string, unknown>>(sql: string, params: unknown[] = []): Promise<T | undefined> { return (await query<T>(sql, params))[0]; }
export async function run(sql: string, params: unknown[] = []): Promise<{changes: number; lastInsertRowid: number}> { const result = await current().query(sql, params); return {changes: result.meta?.changes ?? 0, lastInsertRowid: result.meta?.last_row_id ?? 0}; }
