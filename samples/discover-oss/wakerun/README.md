# Wakerun staging adapter work

This is work toward running [Wakerun](https://github.com/mikaneco/wakerun) on
ato.run. A browser-only staging preview is published; it is not a verified
replacement for the original application. The inspected
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
included fallback. The `react-remove-scroll-bar` 2.3.8 npm package reports a
gitHead that was not found in its stated repository and contains no license
file. The fallback is the upstream MIT license file at commit
`7301c160fda44cb8cf2b9fdfde61efad35736196` (SHA-256
`a79aae0c0f21990d9d963bb3c5a79cdcea9a46f8523ba55c58d7fe776b6ebc84`).
Its parent is the npm 2.3.7 gitHead, and upstream merged the license addition
in [PR 76](https://github.com/theKashey/react-remove-scroll-bar/pull/76).
The npm registry's integrity-checked 2.3.7 and 2.3.8 archives contain exactly
the same 26 non-manifest files; their manifests differ only in version and the
`react-style-singleton` dependency range. Both declare MIT and the same
author. `verify_scrollbar_releases.py` checks these exact archives and the
fallback license without extracting them. This strengthens the evidence for
using the upstream notice, but does not prove that the published archives were
reproducibly built from the Git tree or settle all distribution obligations.
The current staging build predates this additional verification.

To repeat the release comparison, download the two archives from the official
registry URLs in the command below into a local workspace `.tmp` directory,
then run:

```sh
curl -fsSL https://registry.npmjs.org/react-remove-scroll-bar/-/react-remove-scroll-bar-2.3.7.tgz -o .tmp/scrollbar-2.3.7.tgz
curl -fsSL https://registry.npmjs.org/react-remove-scroll-bar/-/react-remove-scroll-bar-2.3.8.tgz -o .tmp/scrollbar-2.3.8.tgz
python3 samples/discover-oss/wakerun/verify_scrollbar_releases.py \
  .tmp/scrollbar-2.3.7.tgz .tmp/scrollbar-2.3.8.tgz \
  samples/discover-oss/wakerun/overlay/browser-license-fallbacks/react-remove-scroll-bar-LICENSE.txt
```

Upstream's calculator states that it uses 2024 tax rules. Its UI also described
the amount of a blue-filing income deduction as a direct tax reduction. The
adapter corrects several visible instances based on the [National Tax Agency's
explanation](https://www.nta.go.jp/taxes/shiraberu/taxanswer/shotoku/2072.htm),
but this is not a complete tax or accounting review. The software must not be
presented as validated for current-year tax filing. Its source distribution,
license notices, all routes, browser persistence, failures and isolation,
backup compatibility, and hosted behavior require further verification before
it is presented as usable for real accounting.

On 2026-11-05, the static browser preview was imported on staging as Instance
`cinst_01M6NF5G57B1SB74WH2M9T0P9H` and listed as Discover app
`dapp_01M6NFCN9FKSCSRYP6N7FBW11Y`. Its bundle SHA-256 is
`c158220fef051e1e0cf4ee1307e3b18dcdb7dac197396683ec86aef40a2272d1`,
ContractRef is `c43fdad728b3b1ee174d8723b6ef085f9b8ae1bb4c734358e7253684f24bbb4e`,
and DerivationRef is `feb167ba0096eb87bdc5c2026cc789d05ca02960186889ba8669be771eccf504`.
The local receipt and staging UI both confirmed the Contract. In the staging
owner Instance, setup and a synthetic expense succeeded and survived reload;
the source page listed 188 files and the license page opened. The Discover
source tab reported that the files were unavailable; the app embeds its own
source page, and the listing discloses this limitation. A source ZIP download,
an added copy and the public trial were not verified. The listing explicitly
prohibits real data and tax filing and notes local-only storage and old-backup
incompatibility.

A separate local browser test on the same assembled preview exercised two
simultaneous synthetic expense writes in separate tabs, reloaded and observed
both, downloaded a `.wakerun` backup, added a third expense, and restored the
file through the browser's file input. The two saved expenses survived the
restore and reload, and the later expense was absent. A browser download from
the compiled static source page produced a ZIP whose 188 filenames and bytes
matched the source manifest. These are local browser tests; they do not verify
staging Discover's source tab, the public trial, or an added copy.
