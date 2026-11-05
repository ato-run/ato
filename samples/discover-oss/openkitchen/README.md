# OpenKitchen browser preview

This overlay adapts [OpenKitchen](https://github.com/clawnify/OpenKitchen) at
commit `16619bdf3ca1d4a96e3fd2067b3aa496655d4cd0`, whose README declares
AGPL-3.0, into a browser-only **preview**. It retains the upstream SQLite
schema, Hono routes, seed data and food-cost calculations. The browser
adapter runs those routes against SQLite in WebAssembly, serializes requests
with Web Locks, and saves a database snapshot in IndexedDB. It adds forms for
ingredients, recipes and recipe items. The Clawnify packages used by the
original application do not declare licenses in their published npm metadata;
the browser build does not include them.

This is not the complete upstream deployment: it has no server-side database,
backup, cross-device or multi-user synchronization, external agent API, OCR,
or upload support. The user-facing warning says to use sample data only.
Clearing browser storage removes the data. The public trial has a separate
temporary origin. A source page provides the modified files, license texts,
dependency lock and an in-browser source ZIP download.

To reproduce, use Node 22, pnpm 11 and Python 3, clone the exact upstream
commit into a clean directory, then copy the `overlay` files into it:

```sh
cp -R /path/to/openkitchen/overlay/. .
pnpm install --frozen-lockfile
python3 package_source.py
pnpm build
```

On 2026-11-05, the stg owner Instance was updated to schema
`csch_01M6N8E0PBDPM8540TJADNWFZB` and published as Discover listing
`dapp_01M6N7V7YE48668Z85ZRND6Y5V`. In the owner Instance, a synthetic
ingredient and recipe produced a 30% food cost, and the result survived a
reload and subsequent application updates. The final source ZIP downloaded
from that Instance contained 39 files, each byte-identical to the local build
inputs. A separate clean checkout rebuilt the final `dist` byte-for-byte;
`pnpm audit --prod` reported no known vulnerabilities at the time.

Discover trial launch failed twice with the generic "could not open app"
screen. An existing Excalidraw listing failed in the same way, while the
existing 2048 listing opened. The common cause is not established. A newly
added OpenKitchen copy and public-trial source download remain unverified;
the listing states these limitations. The preview does not establish that
all requested OpenKitchen capabilities or other OSS integrations are complete.
