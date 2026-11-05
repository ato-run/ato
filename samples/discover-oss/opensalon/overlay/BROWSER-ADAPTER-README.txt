OpenSalon browser preview

Based on https://github.com/clawnify/OpenSalon commit
72cc34a758c95be94ece086480dc4006928374a8. Upstream README declares
AGPL-3.0, while the upstream root LICENSE contains MIT; both are included
here without resolving that inconsistency. This adaptation retains upstream
client, Hono routes, schema, scheduling rules, and sample data. It replaces
Clawnify packages with Hono OpenAPI and a local browser SQLite adapter.

This is a sample-data-only preview. The database exists only in this browser
and origin. There is no shared server database, cross-device synchronization,
backup, or external agent API. Clearing browser storage deletes its contents.
A public trial uses a separate temporary origin. Do not enter real client
information. Requires IndexedDB, WebAssembly, and Web Locks.

Build with Node 22, pnpm 11 and Python 3: pnpm install --frozen-lockfile;
python3 package_source.py; pnpm build. The source page provides this modified
source, dependency lock, and notices.
