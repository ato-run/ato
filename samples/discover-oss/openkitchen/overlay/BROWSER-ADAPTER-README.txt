OpenKitchen browser preview

Based on https://github.com/clawnify/OpenKitchen commit
16619bdf3ca1d4a96e3fd2067b3aa496655d4cd0. That project's README declares
AGPL-3.0; the license text is included. This modified version uses the
upstream routes, schema, domain calculations and UI with an in-browser SQLite
adapter; it adds ingredient and recipe forms and removes the Clawnify runtime
libraries from the browser build. It is a local preview: browser storage only,
no cross-device sync, backup, external API or agent, OCR, or file uploads.
Use sample data only. Clearing browser storage deletes the saved database.

Build with Node 22, pnpm 11 and Python 3: pnpm install --frozen-lockfile;
python3 package_source.py; pnpm build.
A WebAssembly-capable secure browser with IndexedDB and Web Locks is required.
The original Clawnify server entry remains for reference and is not the
entry used by this browser build. The source page provides the corresponding modified source for the browser
build and includes the exact dependency lock.
