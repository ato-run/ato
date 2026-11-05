import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { makeBackup, restoreBackup } from "./backup.js";

const migrationDir = process.argv[2];
if (!migrationDir) throw new Error("Pass the pinned upstream migrations directory");
const sqlite = new DatabaseSync(":memory:");
for (const file of ["001_init.sql", "002_seed_categories.sql", "003_transactions_to_account.sql", "004_add_profile_fields.sql", "005_recurring_and_invoices.sql"]) {
  sqlite.exec(readFileSync(`${migrationDir}/${file}`, "utf8"));
}
const db = {
  async select(query, values = []) {
    const params = Object.fromEntries(values.map((value, index) => [`$${index + 1}`, value]));
    return sqlite.prepare(query).all(params);
  },
  async execute(query, values = []) {
    const params = Object.fromEntries(values.map((value, index) => [`$${index + 1}`, value]));
    return sqlite.prepare(query).run(params);
  },
};

await db.execute("UPDATE settings SET user_name = $1, my_number = $2, invoice_prefix = $3 WHERE id = 1", ["Example", "synthetic", "TEST-"]);
await db.execute("INSERT INTO contacts (name) VALUES ($1)", ["Sample contact"]);
await db.execute("INSERT INTO transactions (date,type,amount,account_id,contact_id) VALUES ($1,$2,$3,$4,$5)", ["2026-01-01", "income", 1000, 1, 1]);
await db.execute("INSERT INTO journal_entries (transaction_id,date,amount) VALUES ($1,$2,$3)", [1, "2026-01-01", 1000]);
await db.execute("INSERT INTO recurring_rules (name,type,amount,account_id,day_of_month,start_date) VALUES ($1,$2,$3,$4,$5,$6)", ["Rent", "expense", 300, 1, 1, "2026-01-01"]);
await db.execute("INSERT INTO invoices (invoice_number,issue_date,contact_id,linked_transaction_id) VALUES ($1,$2,$3,$4)", ["TEST-001", "2026-01-01", 1, 1]);
await db.execute("INSERT INTO invoice_items (invoice_id,description,amount) VALUES ($1,$2,$3)", [1, "Work", 1000]);
await db.execute("INSERT INTO advice_log (advice_key) VALUES ($1)", ["synthetic"]);
await db.execute("INSERT INTO contacts (name) VALUES ($1)", ["Deleted synthetic contact"]);
await db.execute("DELETE FROM contacts WHERE id = 2");
const original = await makeBackup(db);
await db.execute("UPDATE settings SET user_name = 'Different'");
await db.execute("DELETE FROM invoice_items");
await restoreBackup(db, original);
assert.deepEqual(JSON.parse(await makeBackup(db)), JSON.parse(original));
await db.execute("INSERT INTO contacts (name) VALUES ($1)", ["Next contact"]);
assert.equal((await db.select("SELECT max(id) AS id FROM contacts"))[0].id, 3);

const beforeFailures = await makeBackup(db);
const incomplete = JSON.parse(original);
delete incomplete.tables.invoices;
await assert.rejects(() => restoreBackup(db, JSON.stringify(incomplete)));
assert.deepEqual(JSON.parse(await makeBackup(db)), JSON.parse(beforeFailures));

const invalidKey = JSON.parse(original);
invalidKey.tables.contacts[0]['name) VALUES ("bad"); DROP TABLE contacts; --'] = "bad";
await assert.rejects(() => restoreBackup(db, JSON.stringify(invalidKey)));
assert.deepEqual(JSON.parse(await makeBackup(db)), JSON.parse(beforeFailures));

const invalidReference = JSON.parse(original);
invalidReference.tables.invoice_items[0].invoice_id = 9999;
await assert.rejects(() => restoreBackup(db, JSON.stringify(invalidReference)));
assert.deepEqual(JSON.parse(await makeBackup(db)), JSON.parse(beforeFailures));

await db.execute("CREATE TABLE future_data (id INTEGER)");
await assert.rejects(() => restoreBackup(db, original), /Unsupported database schema/);
await assert.rejects(() => makeBackup(db), /Unsupported database schema/);
await db.execute("DROP TABLE future_data");
assert.deepEqual(JSON.parse(await makeBackup(db)), JSON.parse(beforeFailures));
console.log("Wakerun backup: full SQLite roundtrip, sequences, validation and rollback passed");
sqlite.close();
