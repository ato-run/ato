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
tax identifiers; the experimental UI warns about this before export.
Encryption, compatibility with legacy upstream backups, thorough multi-tab
and failure testing, and tax-year correctness remain unresolved. Upstream
stores receipt attachments as data URLs in `transactions.receipt_path`, so
they are covered by the table backup; this has not yet been tested with a
real attachment.

`test-backup.mjs` exercises this component against the original migration
files using Node 22's SQLite implementation. Compile `backup.ts` to an empty
temporary directory as an ES module, copy the test alongside the generated
`backup.js`, and pass the absolute path to the upstream `src-tauri/migrations`
directory when running the test. On 2026-11-05, a synthetic full roundtrip,
sequence preservation, incomplete or malformed backup rejection, foreign-key
rollback, and rejection of a newer schema passed. This does not validate the
Tauri connection pool or any hosted/browser implementation.

## Experimental browser adapter

`assemble_browser.py` operates on a disposable copy of exactly the pinned
upstream source. It verifies upstream file hashes before applying the overlay;
it refuses to operate on a Git checkout. The overlay includes the tested npm
manifest and lock. Run `npm ci`, `python3 package_licenses.py`,
`python3 package_source.py`, and `npm run build` in that copy; configure the
npm cache and temporary directory inside the workspace. The original
SQLite migrations and business queries are retained. The browser adapter uses
SQL.js, IndexedDB and a Web Lock held across each exported database operation;
failed operations discard their in-memory changes. It stores full-backup JSON
in a separate local browser database and can download those backups. Browser
data can be evicted or cleared and does not transfer automatically to another
device or app origin. Browser AI requests may encounter CORS restrictions.

On 2026-11-05, a local Chrome session completed initial setup, recorded a
synthetic expense, created a synthetic invoice, and retained each after a page
reload. A second tab could read the first tab's saved data. Manual and automatic
backup entries were created in the browser. The same revision built after
dependency updates and a full npm dependency audit reported zero findings;
the updated router required explicit settings and report links, which were
checked for settings navigation. The source page listed the complete build
inputs; a locally generated archive's 185 files matched the source tree
byte-for-byte. That count predates later test and notice changes and is not a
claim about a later build. The separate SQLite test exercised full backup
restore and rollback. Four additional tests used the SQL.js asm.js engine,
fake IndexedDB and a simulated Web Lock to exercise concurrent updates,
failed writes, unfinished transactions and a backup roundtrip with invoices
and transactions. Run them with `npx vitest run tests/browser-sql.test.ts
--exclude '**/browser-adapter/**'`; the exclusion avoids treating the bundled
adapter source as a second test suite. Real multi-tab simultaneous writes, browser file-picker
restore and the shipped WebAssembly build's fault handling remain unverified.

The notice generator includes production dependency notices. Radix packages
without a bundled license all report the same pinned upstream gitHead as the
included fallback. The `react-remove-scroll-bar` npm package reports a gitHead
that was not found in its stated repository; its included fallback is from
the repository's newer pinned commit and must be checked against the historical
package before production distribution.

Upstream's calculator states that it uses 2024 tax rules. Its UI also described
the amount of a blue-filing income deduction as a direct tax reduction. The
adapter corrects several visible instances based on the [National Tax Agency's
explanation](https://www.nta.go.jp/taxes/shiraberu/taxanswer/shotoku/2072.htm),
but this is not a complete tax or accounting review. The software must not be
presented as validated for current-year tax filing. Its source distribution,
license notices, all routes, browser persistence, failures and isolation,
backup compatibility, and hosted behavior require verification before it is
published as usable.
