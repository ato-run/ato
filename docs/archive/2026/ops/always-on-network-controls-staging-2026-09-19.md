# Always-on and TCP network controls staging acceptance — 2026-09-19

Status: **PASS** for the scoped staging acceptance. Production was not
deployed, enabled, migrated, or otherwise changed. The test Instance was left
in `on_demand` / `stopped` state.

## Deployed inputs

| Item | Value |
|---|---|
| ato branch | `feat/always-on-network-controls`; Runner code commit `9afe9998` |
| API branch | `feat/always-on-network-controls`; commit `09e1523c` |
| PWA branch | `feat/always-on-network-controls`; commit `e427467` |
| API Worker | `ff48d677-1510-48fa-b989-251c21c1841a` |
| PWA Worker | `a17637ad-1889-4827-92d1-5bc5baaba5df` |
| PWA asset | `/assets/index-Bp4lSzVn.js`, SHA-256 `ed180e93ee5410835067b2f5ad19e889338813a12e1fbc3fce57fa2647fbb322` |
| Dedicated Runner | `01KX0SWDPP2GA41NEXQXNDCC0D` on `ubuntu-sugamo` |
| Runner binary | `/usr/local/bin/ato-connected-realization-worker-9afe9998`, SHA-256 `5f38e9da2518760e2bd5baabe33e5e18eb57f33634b7ed28a3cfb4f4b7e1a7d6` |
| Validator binary | source commit `46e315b2`, SHA-256 `6fbaf2eec55e7e98e105d93bea9f29d47abc93b69819a24383ab52a963b42fb7` |
| Staging migrations | `0282_instance_runtime_network_controls.sql`, `0283_portable_network_setup_gate.sql`, `0284_reusable_fixed_tcp_addresses.sql` |

Only the dedicated Runner was changed. The six shared Runner units were left
on their prior version. The dedicated systemd service runs as its ordinary
user with no ambient capabilities. Its only privileged path is an exact
passwordless helper allowlist for the two startup `iptables --wait 5 -C/-I`
rules on `atoe+`: TCP port 1080 accept followed by default deny. The active
rules recorded 39 broker packets and zero default-deny packets at the end of
acceptance.

## Fixture and authority

| Item | Value |
|---|---|
| Bundle SHA-256 | `sha256:952b1e6322ea79d84598ff076935899f32ee8bd21c6a48608c9a9351b5d7a4e2` |
| ContractRef | `sha256:adc919903a0b7f4a27e670ae4cc500536d1ed4c955fd52fb0d04193171dc425c` |
| DerivationRef | `sha256:6ead45c2cc3966446374b76bc59ed6274c5b30e1cc43f2200a89caff104dd591` |
| Import | `pai_01M2WZQPG5J48ZS41C9443740Y` |
| Instance | `cinst_01M2WZQPDGFSKN2C154RH3MHRF` |
| Egress grant | `egr_3c179bf0-1b47-4c12-ae70-8965c8f9bde8`: service `backend`, `1.1.1.1/32`, TCP 443 |
| Fixed TCP allocation | `tcp_34d3a612-983d-4374-b3bb-d78bd96cbcb3`: `smtp.tcp`, `0.0.0.0:19001` -> guest 2525 |

The PWA upload first remained pending with “Waiting for network access
approval…”. The API's typed Binding projection preserved
`ato.tcp-egress@1`, so the PWA did not misclassify `smtp_egress` as a secret.
After the operator grant and allocation, the same idempotent import resumed
and reached Verified / Ready. The App detail page showed both the approved
egress target and fixed TCP address.

## Runtime acceptance

The following checks passed on both the initial always-on Run and the
automatically restarted Run:

| Check | Observed result |
|---|---|
| Web surface | `GET /healthz` -> `ok` |
| Approved egress through SOCKS broker | `GET /egress` -> `socks-status=0` |
| Protected destination denial | `GET /denied` -> `socks-status=2` |
| Fixed TCP ingress | `probe` to host `127.0.0.1:19001` -> `fixed-tcp:probe` |
| Direct container egress | connect to `1.1.1.1:443` -> errno 101 (`ENETUNREACH`) |

The backend used a dedicated `atoe...` internal bridge. Ordinary Docker
bridge fallback was unavailable. The broker accepted only the exact approved
CIDR/port; loopback remained denied.

The first ready Run, `run_01M2X1WVSV5D0Z3YHK5TRQ6WVN` / lease
`01M2X1WVSVPCEWD7ZG82A9X68M`, renewed its execution authorization from
generation 1 to 2 at `2026-09-19T14:45:38.470Z`. Killing its backend container
produced a failed physical lease, detached fixed-TCP generation 3, incremented
the policy failure count, and caused the controller to create replacement Run
`run_01M2X23E6028MK4B7N1QSX7972` / lease
`01M2X23E60742BETGP9CPJGKAS`. It was ready about 15 seconds after the detected
exit with the same logical allocation at generation 4. No manual restart was
issued.

## Stop/renewal race found and closed

The first explicit stop exposed a Runner ordering race: the control plane
atomically committed `on_demand` / `stopped`, revoked the renewable authority,
then requested physical stop, but the Runner attempted a due renewal before
reading the stop fence. The refused renewal caused cleanup to be reported as a
failure even though all workloads were gone.

Commit `9afe9998` reads the stop fence first and renews only when the Run is to
continue. Unit tests cover both the revoked-at-stop short circuit and the
normal stop-before-renew ordering. On the deployed fix, final Run
`run_01M2X38N8DQ60EYZBDENEETNYE` / lease
`01M2X38N8DQRHPCZZJWG7QSANT` renewed to authorization generation 2, then the
PWA “Use on demand” action produced this durable ordering:

1. Policy generation 6 became `on_demand` / `stopped` at
   `2026-09-19T15:09:39.857Z`, and all Instance authorizations were revoked.
2. The lease stop request was recorded at `2026-09-19T15:09:40.260Z`.
3. The Runner confirmed teardown and the lease became `stopped` with no error
   at `2026-09-19T15:09:51.786Z`.
4. Fixed-TCP generation 5 became `detached` at
   `2026-09-19T15:09:52.180Z`.

The backend required the bounded forced-stop path (exit 137), while the web
service stopped gracefully. Both were physically absent afterwards; the clean
stop receipt is based on confirmed teardown, not process exit code zero. A
later D1 query found zero replacement Runs after the stop, and the PWA showed
“Starts when opened”.

## Verification

- Runner: `cargo test -p ato-connected-realization-worker` — 129 passed;
  `cargo clippy -p ato-connected-realization-worker --all-targets -- -D warnings`;
  `cargo fmt --all -- --check`.
- OCI Adapter: 21 passed; three Linux-Docker integration tests were skipped on
  the macOS development host and the real Linux Docker path was exercised by
  this staging acceptance.
- API: typecheck passed; network-admin regression suite 3/3 passed, including
  reuse of an address whose previous allocation is revoked.
- PWA: typecheck, targeted tests, and production build passed. The deployed
  HTML referenced the expected hashed asset, whose bytes matched the local
  build exactly and contained only the staging API origin.
- API health returned HTTP 200 on the active version. Production was untouched.

## Boundary follow-up — 2026-09-20

The additional restore, connection-lifetime, address-handover, and renewal
boundaries were implemented after the acceptance above. This follow-up used:

| Item | Value |
|---|---|
| ato commit / Draft PR | `7391dbee` / [#1365](https://github.com/ato-run/ato/pull/1365) |
| API commit / Draft PR | `41a436f6` / [#663](https://github.com/ato-run/ato-api/pull/663) |
| PWA commit / Draft PR | `7d05963` / [#400](https://github.com/ato-run/ato-pwa/pull/400) |
| API Worker | `be123584-aa2b-482b-ab6a-7b7029dc8401` |
| PWA Worker / asset | `7d9829a2-fc97-463f-adf3-526c2aae2383` / `/assets/index-DNnP0zLt.js` |
| Runner binary | `/usr/local/bin/ato-connected-realization-worker-7391dbee`, SHA-256 `60427a09c5624bfba13c94cc7858e87fc1be1a64757844a2ed0fd7a319712c12` |
| Dedicated Runner PID | `783035` throughout the clean fixed-address handover |
| Additional staging migration | `0285_restore_external_effects_hold.sql` |

### Restore external-effect hold

The stateful follow-up bundle had bundle SHA-256
`sha256:ba412a8abb5f9789d71a7d3630485d89e4a99d1832ac482b71b8e37999b88643`,
ContractRef
`sha256:adc919903a0b7f4a27e670ae4cc500536d1ed4c955fd52fb0d04193171dc425c`,
and DerivationRef
`sha256:59d02da636021846b82017dca72a8160c46d0ad85750eab33d5c68b55d6b8200`.
Instance `cinst_01M2X74EN2Q17AXEEH26955P7C` used volume
`svol_01M2X77D2MNZZ7SBAR91CBFCX7` and state slot
`isslot_01M2X77CTKFPSBRSWKJ1GFK2G4`.

Checkpoint operation `vop_01M2X7AKQK8MKDGKD6RJHYE931` captured marker
`checkpoint-A` as revision `isrev_01M2X7ANNVAPAX27EN6AGDVPRV`. After the live
volume was changed to `after-checkpoint`, restore operation
`vop_01M2X7RPMWRQH3M2KXP0XGZ1E3` restored that revision under writer fence 5
and completed with recovery-hold generation 2 still `active`.

While the hold was active:

- the egress grant `egr_13bcba25-09ae-4326-993c-a21a2051f2e0` stayed enabled
  and allocation `tcp_2bc19ec5-1b92-4962-b76c-12cb2cb6376c` stayed allocated;
- all runtime HTTP routes and fixed-TCP bindings remained detached;
- a normal **Open App** attempt created no Run (the Instance remained at five
  recorded Runs);
- toggling **Use on demand** and then **Keep running** changed policy
  generation to 3 but did not release the hold or create a Run; and
- the PWA displayed the restore-specific warning and exact **Allow network
  access** action.

The hold was explicitly released only for that operation and generation. The
always-on reconciler then created Run `run_01M2X80GQ9YPGT3JC4TXAJK6EY`, the
HTTP route became ready, fixed-TCP binding generation 4 became active, the
persisted marker read `checkpoint-A`, `/egress` returned `socks-status=0`, and
the fixed address returned `fixed-tcp:post-restore`.

The staging admin surface required a fresh Cloudflare Access login. The test
did not reuse or request an OTP. Operator-only staging mutations therefore
used guarded direct D1 statements and append-only `admin_audit_events`; this
includes operation creation/recovery, exact hold release, and network
allocation changes. The PWA display and normal owner wake/policy paths were
still exercised. This is a limitation of the staging evidence: the exact
release HTTP endpoint is covered by API tests, not by an authenticated admin
browser call in this run.

One malformed staging-only operation ID was rejected by the Runner before it
opened the volume. It was recovered only when the state-slot ID, writer Run,
writer fence 4, operation, and hold generation 1 all matched; the audit event
is `aae_restore_probe_recover_20260920`. It is retained as an interrupted
operation rather than hidden from the record.

### Connection ownership and clean fixed-address handover

With Run `run_01M2X8EV77QRNBADWTYYXZCWHA` serving as owner A, the exact
address returned `fixed-tcp:clean-A`. A live fixed-TCP client observed EOF
after the stop (`16.486s` from connect); the binding reached `detached`, and
the Run and lease both reached `stopped`. The Runner process stayed PID
`783035`.

Allocation A `tcp_2bc19ec5-1b92-4962-b76c-12cb2cb6376c` was then revoked at
allocation generation 8. The same `0.0.0.0:19001` address was assigned to the
already verified Instance `cinst_01M2WZQPDGFSKN2C154RH3MHRF` as allocation B
`tcp_34d3a612-983d-4374-b3bb-d78bd96cbcb3`. Run
`run_01M2X8JAQMAFDPEED2T11957MA` installed binding generation 9 and returned
`fixed-tcp:clean-B`, still from Runner PID `783035`. B was stopped after the
proof. A late operation naming allocation A cannot match B's allocation ID,
generation, Run, or lease; deterministic Runner tests exercise that stale-A
case directly.

The egress probe established a real SOCKS connection, but the selected
Cloudflare endpoint closes an idle connection after about ten seconds. Its
EOF is therefore **not** claimed as stop-caused staging evidence. Bounded
broker cancellation, connection-slot return, bidirectional EOF, and both
fixed/egress connection groups are established by the Runner tests; a
controlled long-lived relay is reserved for the Mail fixture acceptance.

### Follow-up verification

- Runner: all 134 tests passed; targeted boundary suite 7/7 passed; clippy
  with warnings denied and rustfmt check passed.
- API: typecheck passed; targeted suites 61/61 plus always-on suite 7/7 passed.
- PWA: typecheck, targeted suite 7/7, and production build passed. `pnpm lint`
  could not run because this checkout has no `eslint` command.
- Production, public ingress, low ports, public mail sending, and feature flags
  were not changed.
