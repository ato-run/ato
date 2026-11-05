# Beveren FSM integration investigation

Status: local frontend build evidence only; **not installed or published on
staging**. Beveren FSM is a Frappe/ERPNext application, distinct from the
Puter Field Service preview. Its scheduling interface calls Frappe APIs and
requires the server's session, boot context and CSRF token. A static export is
not an operating instance of Beveren FSM.

The inspected upstream source is
`Beveren-Software-Inc/Field_Service_Management` commit
`df0ae701a0f389e3d81c5ec6d77b1041486cda86`. The upstream root `license.txt`
contains AGPL-3.0; `beveren_fsm/hooks.py` says `app_license = "mit"`.
This discrepancy and notices for the full Frappe, ERPNext and frontend
dependency closure require review before distribution.

The upstream `schedule/package-lock.json` SHA-256 is
`d81c53f8f3d99b4ec65b9b68df0b225830e37bc3fc1fd1032537363fde2d1e8d`.
On 2026-11-06 `npm ci` with npm 11.20.0 rejected it because optional esbuild
and Rollup platform entries were missing. `schedule-optional-packages.json`
records 43 registry entries needed to complete the lock, each marked optional
and pinned by SHA-512. `prepare_schedule_lock.py` requires that exact original
lock and addition file, preserves every existing package entry, checks the
result digest, and refuses to overwrite its output:

```sh
python3 samples/discover-oss/beveren-fsm/prepare_schedule_lock.py \
  <upstream-checkout>/schedule/package-lock.json .tmp/beveren-schedule-lock.json
```

The completed lock SHA-256 is
`764c4225f02b41ac0fee3e6ef2350223ed44782f7d43c9472dabc3cd021a5783`.
With it, an install using `npm ci --ignore-scripts --os=linux --cpu=x64
--libc=glibc` and a Linux x86-64 Node 22.23.2 image pinned at
`sha256:43aeff40f4afc22e83f7589a2f37e111cff5ca84529571f1c8415bcc5fcc21b2`
built the unchanged frontend twice with Vite 5.4.20. Both builds ran as a
non-root user with a read-only container root, no network, and the same
synthetic Bench configuration; all five resulting files matched byte-for-byte.
The generated `index.html` still contains Frappe template substitutions;
neither the frontend nor Frappe was run or tested on staging.

On 2026-11-06, `npm audit --omit=dev` against the completed lock reported 23
affected dependency entries: 14 high and 9 moderate, including transitive
dependencies. This is an advisory inventory, not proof that each issue is
reachable in the built browser bundle. The lock completion does not update or
remediate those dependencies; investigate and test fixes before deployment.

The map view refers to external CDN assets and OpenStreetMap tiles. Actual
use may make network requests and disclose map activity; it needs an explicit
network and privacy design. A deployable instance also requires a compatible
Frappe/ERPNext stack, database, queue, scheduler, app installation, secret
bootstrap, durable storage, backup and restore, source/notices, and end-to-end
verification. None of those is established by the frontend build.
