# Wakerun staging adapter work

This is work toward running [Wakerun](https://github.com/mikaneco/wakerun) on
ato.run; it is **not yet an app or a staging deployment**. The inspected
upstream revision is `e372df3972373fca1209c9b68a476ad06df265e5`, whose README
states MIT licensing. Wakerun is a Tauri desktop application using SQLite; its
database contains private accounting information, and may contain a My Number
and AI API keys.

At that revision, `src/lib/export.ts` omits `recurring_rules`, `invoices`,
`invoice_items`, and `advice_log` from its manual export and does not restore
all settings. Its automatic backup includes several additional tables, but
the same restore path still does not restore them. Do not describe that
roundtrip as a complete backup.

`backup.ts` is a candidate full SQLite backup/restore component for the
pinned five-migration schema. It includes all 13 application tables and
SQLite AUTOINCREMENT sequences, rejects missing/unknown tables and invalid
values, checks foreign keys, and uses a transaction for rollback. It rejects
the upstream legacy v1 format rather than silently restoring an incomplete
backup. The caller **must** hold an exclusive database lock and use one
connection throughout each operation; the component is not connected to
Wakerun yet. Its JSON output contains all settings including any API key and
tax identifiers, so the eventual UI must clearly disclose this before export.
Encryption, file attachments outside the database, browser persistence,
automatic backup retention, multi-tab coordination, and import of old
upstream backups remain to be designed and validated.

`test-backup.mjs` exercises this component against the original migration
files using Node 22's SQLite implementation. Compile `backup.ts` to an empty
temporary directory as an ES module, copy the test alongside the generated
`backup.js`, and pass the absolute path to the upstream `src-tauri/migrations`
directory when running the test. On 2026-11-05, a synthetic full roundtrip,
sequence preservation, incomplete or malformed backup rejection, foreign-key
rollback, and rejection of a newer schema passed. This does not validate the
Tauri connection pool or any hosted/browser implementation.
