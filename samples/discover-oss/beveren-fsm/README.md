# Beveren FSM integration investigation

Status: local synthetic workflow and security investigation only; **not
installed or published on staging**. Beveren FSM is a Frappe/ERPNext application, distinct from the
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

## Isolated backend and authorization checks

A local-only image built offline from the pinned upstream app and
`frappe/erpnext:v15.124.5` image digest
`sha256:6e016c0fc3aec5b32c14ff7c2ac97523cc864748c83ce75197779b8fe5a665a8`.
It installed Beveren 0.0.1 beside Frappe 15.126.0 and ERPNext 15.124.5.
With a private Docker network, synthetic credentials, Redis and MariaDB, it
installed the app, authenticated a synthetic administrator, served the actual
scheduling page and JS, and created and retrieved a synthetic customer,
service request, service order and order movement. The web container used a
read-only root and a non-root user. The scheduler was disabled. MariaDB used a
Linux tmpfs because the macOS shared filesystem caused a case-sensitivity
failure; this test does **not** establish durable storage or restart recovery.
An SQL dump of the synthetic site was imported into a separate database on the
same server; both databases contained 729 tables, one service request and one
service order. A second local Frappe process using the restored database and a
copy of the synthetic site directory accepted the synthetic administrator
login and returned the restored request and order. This is not a disaster
recovery test: the database server, host, Redis and app image were shared, the
site directory copy was not captured atomically with the dump, and uploaded
files, encryption-dependent data and scheduler recovery were not exercised.

The original source also failed two actual role checks. A synthetic Website
User without Service Request or Service Order access received HTTP 200 and
could read another user's service request through Beveren's custom list API;
the same user could record an order movement despite the standard order API
returning HTTP 403. `prepare_list_permission_fix.py` and
`prepare_movement_permission_fix.py` make narrow, digest-fenced changes to the
inspected source. With those files mounted into the local web container, both
unauthorized actions returned HTTP 403. An administrator could still list and
update the synthetic data; a user granted write access to only the order was
refused without mutation, and the same user succeeded after the linked request
was also shared. A separate synthetic appointment test showed that both the
original custom appointment list and single-record endpoints returned a
restricted record despite the standard endpoint returning HTTP 403. The narrow
`prepare_appointment_permission_fix.py` candidate made both endpoints reject
the unshared user, while an administrator and a user explicitly granted read
access could still retrieve it. These checks cover only those endpoints and do
not prove all fields and linked objects are protected. An unshared user could
also retrieve a synthetic invoice through the original appointment invoice
API. With the candidate checks, that user was refused; sharing the appointment
alone did not grant invoice access, and sharing both did. An unrelated order
parameter was rejected. Other whitelisted methods, including scheduling APIs, still need
authorization and security review before exposure. The image itself contains
the unpatched source unless these generated files are incorporated.

The unassigned-order scheduling API also exposed a submitted synthetic order
to a user whose standard order read returned HTTP 403. With the local
`prepare_schedule_permission_fix.py` candidate mounted, the unshared user was
refused and an administrator or a user explicitly granted read access could
still list it. This does not establish that the other scheduling operations or
the underlying cross-appointment filtering are secure.

An independent arm64 local experiment combined MariaDB and the app image on a
single persistent volume and used a separate internal, ephemeral Redis. After
creating a synthetic customer, request, order and private attachment, it
restarted the database container and manually restarted Frappe; the records
and attachment were readable. A later synthetic amd64 candidate combined the
database, Redis with AOF, web process and workers under one local supervisor;
it verified normal restart, bound-secret initialization, restricted access and
refusal to restart after an unclean stop. See `local-test/single-volume/README.md`
for the exact scope and serious limitations. This is not a hosted Runner test
or a production-ready process and storage arrangement.

In the same isolated stack, the actual scheduling API created and submitted a
synthetic appointment against a submitted order, assigned a synthetic
technician and changed the order to Scheduled. An unauthorized reschedule was
rejected without a change; an administrator rescheduled it successfully.
Separately, the original purchase-order and purchase-invoice mapping endpoints
returned details of an unreadable order to a restricted user. The local order
source candidate adds source-read and target-create checks to all five order
mapping endpoints. Tests rejected an unshared user and an order-read-only user;
an administrator and a non-admin operator with the required roles and order
share could produce the two synthetic purchase documents. A subsequent test
showed that the original mapping also disclosed a supplier from an unreadable
linked request. The updated candidate hides that supplier until the request is
shared; it preserves mapping for a user who can read only the order. This is
not a full authorization audit of the application.

`local-test/` contains the synthetic-only Dockerfile, Compose configuration and
request probes used for these observations. They are not a production
deployment and must never be pointed at real customer data.

On 2026-11-06, `npm audit --omit=dev` against the completed lock reported 23
affected dependency entries: 14 high and 9 moderate, including transitive
dependencies. This is an advisory inventory, not proof that each issue is
reachable in the built browser bundle. The lock completion does not update or
remediate those dependencies; investigate and test fixes before deployment.

The map view refers to external CDN assets and OpenStreetMap tiles; the
geocoding API sends entered queries to Nominatim. Actual use may disclose map
activity or searches and needs an explicit network and privacy design. The
unpatched movement method saves documents with `ignore_permissions=True`; the
local role checks above confirmed that this was reachable without write access.
A deployable instance also requires a compatible
Frappe/ERPNext stack, database, queue, scheduler, app installation, secret
bootstrap, durable storage, backup and restore, source/notices, and end-to-end
verification. None of those is established by the frontend build.
