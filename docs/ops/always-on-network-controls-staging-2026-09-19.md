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
