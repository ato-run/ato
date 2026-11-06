# Puter.js apps on ato.run (ato-puter-shim)

[HeyPuter/everything](https://github.com/HeyPuter/everything) (MIT, 51 apps)
uses Puter.js as its only backend, so app data lives in each user's Puter
account. `ato-puter-shim.js` replaces `<script src="https://js.puter.com/v2/">`
with a small `window.puter` that keeps the data in the ato.run Instance through
`window.storage` (shared scope). Puter itself (AGPL-3.0) is not bundled.

Provided: `puter.env`, `puter.auth` (always signed in as the Instance),
`puter.kv` (get/set/del/list/incr/decr/flush), `puter.fs` (write/read/stat/
delete/readdir/mkdir/rename/copy/move, 4 MB per file, stored as base64).
Not provided: `puter.workers` (team sign-in, outbound email), `puter.ai`,
`puter.hosting`; calls reject with "… is not available on ato.run".

Limits:

- `window.storage` personal scope is refused on staging, so the shim uses the
  shared scope: everyone who can open the Instance shares the data.
- `kv.incr` is read-modify-write, not atomic across devices.
- A cold load fetches every record of a collection, eight requests at a time.

## Sign-in policy

Single-user and single-company OSS apps run on ato.run with their own sign-in
turned off, because ato.run already authenticated the viewer. The rule:

- Replace the app's auth at one point (here `puter.auth`; for server apps, a
  trusted identity passed by ato.run) instead of deleting auth code app by app,
  so the upstream diff stays small.
- Keep passing a user identity to the app. The shim fixes it to
  `ato:<Instance>` until ato.run can hand the signed-in user to the page.

Preconditions; if one does not hold, keep the app's own sign-in:

1. The app is reachable only through ato.run. A server app whose port can be
   reached directly must not drop its sign-in.
2. The Instance is private. Storage is shared scope, so anyone who can open
   the Instance sees all data; do not use this for published or shared apps.
3. Roles inside the app (admin, staff, ...) are kept; only sign-in is replaced.

Not suitable: apps where several people need separate data, or where outside
people sign in (customer portals, client galleries, booking forms).

Tests: `node --test samples/discover-oss/puter-compat/ato-puter-shim.test.mjs`.

## Staging check, 2026-11-07

`../puter-property-management/build.sh` built upstream commit
`c17d23be5ab4141742139c227ab2fd592397985e` as one HTML file, uploaded through
the stg Studio "新規作成" page as a private app (Instance
`cinst_01M6RT6F58RXR51498AVJ6VY0P`, not published to Discover). The app opened
signed in, the company setup was saved ("Saved"), 37 keys appeared under
`puter1:kv:pm1:`, and after deleting the page's IndexedDB and localStorage a
reload restored the company name from ato.run storage. That load took 20–30 s
with sequential reads, before the eight-way parallel read was added; the
uploaded build predates that change. Records beyond the initial setup (property,
lease, payment), attachments, a second device and sharing were not tested.
Only synthetic data was used.
