import { HTTPException } from 'hono/http-exception';
import { get, run } from '../db';

// Table names in callers are code literals; PATCH field names come from JSON.
// Limit those fields before ever interpolating them into an SQL identifier.
const UPDATE_FIELDS: Record<string, ReadonlySet<string>> = {
  suppliers: new Set(['name', 'contact_name', 'email', 'phone', 'notes']),
  ingredients: new Set(['name', 'category', 'unit', 'cost_cents', 'supplier_id', 'par_level', 'allergens', 'active']),
  recipes: new Set(['name', 'kind', 'category', 'yield_qty', 'yield_unit', 'menu_price_cents', 'target_food_cost_pct', 'notes', 'active']),
  invoices: new Set(['supplier_id', 'invoice_number', 'invoice_date', 'total_cents', 'currency', 'status', 'source', 'file_path', 'notes']),
  documents: new Set(['kind', 'name', 'issuer', 'reference', 'issued_date', 'expiry_date', 'status', 'file_path', 'reminded_at', 'notes']),
};

export function newId(): string {
  return crypto.randomUUID();
}

export async function insert(table: string, data: Record<string, unknown>): Promise<string> {
  const id = (data.id as string) ?? newId();
  const row: Record<string, unknown> = { id, ...data };
  const cols = Object.keys(row).filter((key) => row[key] !== undefined);
  await run(
    `INSERT INTO ${table} (${cols.join(', ')}) VALUES (${cols.map(() => '?').join(', ')})`,
    cols.map((key) => row[key]),
  );
  return id;
}

export async function update(table: string, id: string, data: Record<string, unknown>): Promise<void> {
  const allowed = Object.hasOwn(UPDATE_FIELDS, table) ? UPDATE_FIELDS[table] : undefined;
  if (!allowed || !data || typeof data !== 'object' || Array.isArray(data)) {
    throw new HTTPException(400, { message: 'Invalid update' });
  }
  const cols = Object.keys(data);
  if (cols.some((key) => !allowed.has(key))) {
    throw new HTTPException(400, { message: 'Invalid update field' });
  }
  const present = cols.filter((key) => data[key] !== undefined);
  if (present.length === 0) return;
  const sets = present.map((key) => `${key} = ?`);
  sets.push("updated_at = datetime('now')");
  await run(`UPDATE ${table} SET ${sets.join(', ')} WHERE id = ?`, [
    ...present.map((key) => data[key]), id,
  ]);
}

export async function remove(table: string, id: string): Promise<void> {
  await run(`DELETE FROM ${table} WHERE id = ?`, [id]);
}

export async function byId<T>(table: string, id: string): Promise<T | undefined> {
  return get<T>(`SELECT * FROM ${table} WHERE id = ?`, [id]);
}
