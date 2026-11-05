# OpenSalon browser preview

This overlay adapts [OpenSalon](https://github.com/clawnify/OpenSalon) at
commit `72cc34a758c95be94ece086480dc4006928374a8` into a browser-only
sample-data preview. It retains upstream client features, Hono routes, SQLite
schema, seed data, and appointment conflict logic. Requests execute locally
against SQLite in WebAssembly. Web Locks serialize access and IndexedDB saves
snapshots. Clawnify packages whose npm metadata lacks a license are replaced
with Hono OpenAPI and a small local storage adapter.

The upstream README says AGPL-3.0; its root LICENSE contains an MIT license.
This overlay includes both texts and the upstream notice without claiming to
resolve the discrepancy. Third-party production license texts are provided.

This preview is not a complete hosted deployment: there is no server-side
database, backup, shared or cross-device data, or external agent API. Data is
local to a browser origin and disappears when its storage is cleared. Users
are warned to use sample data only. The source page includes modified sources,
build instructions, dependency lock, and license notices.

To reproduce, use Node 22, pnpm 11 and Python 3. Clone the exact upstream
commit into a clean directory and copy this `overlay` into it, then run
`pnpm install --frozen-lockfile`, `python3 package_source.py`, and `pnpm build`.
The source manifest intentionally omits itself because it is generated.

On 2026-11-05, the 46 upstream tests passed against the modified routes. In a
local Chrome preview, a sample booking survived reload and an overlapping
booking showed the conflict. `pnpm audit --prod` reported no known
vulnerabilities. A separate clean checkout rebuilt the final `dist` byte-for-byte. The owner
Instance `cinst_01M6N99VM9QGG20VP5HQQY2EWW` runs schema
`csch_01M6N9EC6JZ3GQ36T6M8A79VBS` and is published as Discover listing
`dapp_01M6N9JCPZGDHZEGZ8SWG434JA`. On stg, a sample booking survived a
schema update and a reload on its appointment screen. The source ZIP
downloaded from the owner Instance contained all 73 source files byte-for-byte.
The listing says the public trial and a newly added copy were unverified.

On 2026-11-06, opening the Discover listing and using its `…` menu to open the
trial in a separate tab reached the browser preview. A synthetic appointment
was created and remained visible after reloading. A second synthetic booking
for the same staff member and time displayed the existing appointment as a
conflict and offered an explicit override. This checks the direct public
trial in one browser, not multi-user sharing or backup. The Discover embedded
frame stayed on its loading screen during this test; a completed embedded
launch and a newly added copy remain unverified. Only synthetic data was used.
