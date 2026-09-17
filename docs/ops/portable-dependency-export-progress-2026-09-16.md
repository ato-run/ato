# Datasette dependency export progress — 2026-09-16

This record began as a partial implementation checkpoint. Later sections now
include the completed four-route, Hosted Surface, v4 transport, and isolated
offline acceptance. Production was not changed.

## Fixed semantic identity and local transport artifacts

- Source: `samples/datasette-cpu.capsule`, Datasette 0.65.2, ContractRef
  `sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19`.
- Python DerivationRef:
  `sha256:94ab606ee88d41da3af332e72d5da151fd0920a3aadc9c8c94aefaa80518b21f`.
- OCI DerivationRef:
  `sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f`.
- `thin`: `.tmp/datasette-evidence/datasette-thin.capsule`, 74,275 bytes,
  SHA-256 `271d70a2d8c3561bb42e72ca24f4954a511403f3d5e629d51914b35ac08e7e55`.
  It externalizes 30 digest-pinned PyPI wheel objects and retains one external,
  digest-pinned OCI image. It embeds semantic objects and the database.
- `cached`: `.tmp/datasette-evidence/datasette-cached.capsule`, 7,000,996
  bytes, SHA-256
  `e23f5ed24504ca8d953df12b1390c77825a6a785ce00f87c135f694baac3deeb`.
  It embeds the 30 wheel objects but retains the external OCI image. Neither
  export changed ContractRef or either DerivationRef.
- `offline`: `.tmp/datasette-evidence/datasette-offline.capsule`, 120,583,935
  bytes, SHA-256
  `f439923321bfa748af3a8993ab4c7c495f8f0d81ae5f9504c8add92b539591a2`.
  It embeds the Python wheels and a Docker 29 OCI-layout archive. The archive
  contains the pinned manifest, config, and seven layer blobs; each digest,
  declared byte size, and platform was verified before export. The seven
  compressed layers total 85,165,556 bytes. At this checkpoint, the 32 MiB
  Hosted upload limit could not carry this file; the later v4 transport
  acceptance records the bounded replacement.

The OCI image is
`docker.io/datasetteproject/datasette@sha256:0f57db16cf4eb6cca57f1cedaa0a696bca1c65a1d75b8f7ee372c2dd909a32a0`
for `linux/amd64`. Its image reference and platform remain D execution
conditions; the transport packing mode is not part of K or D.

## Observed local behavior

`ato run <thin> --derivation <Python D> --no-open --verification-receipt
<file>` fetched and SHA-256-checked 30 wheels and satisfied all three HTTP
observations. The v2 receipt at
`.tmp/datasette-evidence/cli-python-thin-v2-receipt.json` records those 30
refs in `execution.dependency_fetches`, with `fully_satisfied: true`.

`ato run <cached> --derivation <Python D> --no-open --verification-receipt
<file>` also satisfied the three observations while `HTTP_PROXY` and
`HTTPS_PROXY` pointed at an unreachable loopback port and `NO_PROXY` admitted
only `127.0.0.1,localhost`. Its receipt is
`.tmp/datasette-evidence/cli-python-cached-blocked-network-receipt.json`.
This is evidence that Python wheel installation used embedded bytes, not a
network request; it is not a system-wide outbound firewall test.

Running `thin` with the same blocked proxy failed explicitly with
`dependency unavailable: sha256:03ac140115f39d4295288a9adf74fdc6ae607f6ef44abee8466520458207242b`.
Unit tests reject embedded wheel tamper/omission, sparse-object omission,
false offline metadata, and fetched bytes with the wrong size or SHA-256.
The new archive parser also rejects a modified OCI layer before Docker load.

`ato run <offline> --derivation <Python D> --no-open` satisfied all three
observations under the same proxy-block condition. Its receipt is
`.tmp/datasette-evidence/cli-python-offline-blocked-proxy-receipt.json`.
The byte-identical offline bundle was copied to the Linux/amd64 validation
host and its SHA-256 was rechecked there. `ato run <offline> --derivation
<OCI D> --no-open` loaded the embedded image archive through the Runner-owned
Docker Adapter and satisfied all three observations. The receipt is
`/home/ekohsuke/.ato-staging/.tmp/portable-datasette/export-policy/cli-oci-offline-blocked-proxy-receipt.json`.
The host has Docker Engine 29.1.3. The Docker store already contained this
image, so this is **not** image-absent acceptance. Proxy variables alone are
also not a daemon-wide registry block; the no-network guarantee is still
untested in an isolated target.

A separate macOS Docker Desktop 29.1.3 (Linux/arm64 daemon) began with no
Datasette image. It exposed a loader defect: the verified archive loads an
untagged image ID but does not install the pinned `repository@digest` alias;
inspecting that alias failed. The OCI Adapter now loads the selected platform,
validates the local manifest/config image ID and platform, then runs that
local ID with `--pull=never`. A second image-absent run reached a running
`linux/amd64` container without a registry pull. It could not reach the
container's private Docker bridge IP from macOS, so its HTTP observations
did **not** pass; this is not offline acceptance. Ctrl-C while waiting for
readiness then returned in under one second, with no receipt, container, or
per-Run network left. The exact test image was removed afterward; the
interrupted test's temporary workspace was moved to Trash for recovery.

The fix was then built at ato `91c81f03` in the isolated Linux/amd64 source
checkout, without replacing any staging service. The same offline file (SHA
above) satisfied all three original observations through the local-ID launch
path. Receipt:
`/home/ekohsuke/.ato-staging/.tmp/portable-datasette/export-policy/cli-oci-offline-local-id-receipt.json`.
It uses receipt schema v2, records the original K and OCI D, and names the
embedded image load. This Linux daemon still had the image beforehand. A
dedicated image-absent Linux Docker store with outbound blocking remains
necessary for full acceptance; the shared staging daemon's existing image
must not be deleted to simulate it.

The v3 Static and Process interop `.capsule` fixtures both passed their CLI
HTTP observations after the v4 changes. A separate OCI run held open behind
a loopback Caddy reverse proxy served `/` with status 200 and 1,549 body
bytes using the `s0-rstg002.ato.run` Host header. This rules out a generic
Caddy-to-PortForwarder failure in that local configuration, not the Hosted
Cloudflare path. A timed SIGTERM then returned cleanly and left no container
or per-Run Docker network. Before the signal-handler fix, termination had
left a test container; that exact test container/network were removed.

A diagnostic Caddy invocation briefly installed a local CA in the validation
host's trust store. The exact certificate, symlinks, autosaved user config,
and temporary Caddy key material were removed, and the trust store was
rebuilt. No diagnostic listener remains.

Reproduce:

```sh
cargo run -q -p ato-cli --bin ato -- export-plan samples/datasette-cpu.capsule --portability thin --json
cargo run -q -p ato-cli --bin ato -- export samples/datasette-cpu.capsule --portability thin --output .tmp/datasette-evidence/datasette-thin.capsule
cargo run -q -p ato-cli --bin ato -- export samples/datasette-cpu.capsule --portability cached --output .tmp/datasette-evidence/datasette-cached.capsule
cargo run -q -p ato-cli --bin ato -- export samples/datasette-cpu.capsule --portability offline --oci-archive .tmp/datasette-evidence/datasette-image.tar --output .tmp/datasette-evidence/datasette-offline.capsule
cargo run -q -p ato-cli --bin ato -- run .tmp/datasette-evidence/datasette-thin.capsule --derivation sha256:94ab606ee88d41da3af332e72d5da151fd0920a3aadc9c8c94aefaa80518b21f --no-open --verification-receipt .tmp/datasette-evidence/cli-python-thin-v2-receipt.json
```

`ato export` refuses to overwrite an existing output; choose a fresh filename
when rerunning. `cargo test -p ato-cli --lib`, `cargo test -p
ato-portable-application --lib`, `cargo test -p ato-adapter-oci --lib`, and the
portable-bundle and verification receipt tests passed.

## Hosted OCI Surface and remaining acceptance

The earlier Hosted OCI Run satisfied K at the Runner's internal Port, but its
public Surface returned Cloudflare 524. The runtime-launch code assigns the
Runner's ingress slot port to the logical endpoint for both Process and OCI;
the OCI Adapter forwards that host port to the container's guest port. The
staging Caddy configuration maps s0 to port 8420 and s1 to 8421. These read-only
checks have not located the failure hop. Current staging PWA authentication
and Cloudflare API credentials cannot create a fresh comparable pair of Runs,
and the user-facing browser handoff did not permit OCI Surface inspection.
No fix, browser acceptance, or OCI receipt for a new Run is claimed.

Wire v4 is currently CLI-local; Hosted import and Surface paths still accept
v3. The remaining requirements are: trace a fresh OCI Run through Cloudflare
to the same logical Surface, open and operate Datasette in an authorized
browser, test the offline OCI loader against an image-absent Docker store,
run a real outbound-blocked offline acceptance, and rerun all four routes with
the exact same supported bundle representation. No fallback, upload-limit
expansion, or production deployment was used to conceal these gaps.

## v0 PR 1 local implementation checkpoint (not staging acceptance)

The subsequent `feat/portable-v0-oci-surface` work starts from ato
`4a92cecd`, API `c545b2fa`, and PWA `f503d773`. It adds a Run-scoped OCI
port-mapping diagnostic (container IP and guest Port → Runner forward Port),
correlated in the API with the selected DerivationRef, Run, lease, runtime
route, and public Runner host. The app proxy logs the root navigation's dial,
headers, and complete HTML body separately. Logs omit credentials and query
strings; none of these observed host values enter K or D identity.

Hosted import now reports Surface status separately from the Contract receipt.
For a dynamic route, the API requires the selected Run/lease's ready route and
probes the public Runner origin at the declared initial path, consuming the
response body with a size and time bound. The PWA waits for both Verified and
Surface Ready before offering Open App; a failed public probe does not rewrite
or invalidate K. This is a gate and diagnostic, **not yet a demonstrated 524
repair**. The cinst app-proxy hostname and actual browser operation still need
fresh staging acceptance.

Local checks: `cargo check` for the OCI Adapter and Connected Worker; their
87 tests; API typecheck, 7 new Surface tests, and 31 existing Surface/App proxy tests;
PWA typecheck, AppReady DOM test, and build passed. A broader API route suite
failed 13 tests because its test D1 lacks tables such as `runtime_routes` and
`telemetry_client_events`; the same 13 failures reproduced from untouched API
`c545b2fa` under the same command. No staging or production deployment was
performed at this checkpoint. Staging PWA was signed out when checked, so no
new browser receipt or screenshot is claimed.

## Hosted OCI Surface staging acceptance (completed)

The fresh browser run on 2026-09-16 used these revisions:

- ato `0aaeab98`
- ato-api `02f01c07`
- ato-pwa `17e1257`

Staging API Worker version
`1a2f59a3-1619-4c91-8711-1b4ac67ca97f` served the API change at 100% after
the safe deploy preflight and postflight both found 41 secrets; `/health`
returned 200. The PWA remained at version
`cdc3f0e4-584a-4fa3-ac57-cf132b7372f7`. Runner slot s0 used
`/usr/local/bin/ato-connected-realization-worker-0aaeab98`, whose SHA-256 is
`781c32934729e218864820778670d5cc09135b24ce06f4c63c00fa94bdf5f8a7`.
Production was not changed.

The reproduced 524 was not a Contract or container failure. The OCI
PortForwarder copied the upstream response, but when the upstream closed it
waited for the client-to-upstream copy thread without half-closing the client.
Caddy could retain and reuse that apparently live connection; requests such as
`/catalog` then reached the ingress but never received response bytes. The
forwarder now propagates EOF in both directions with `Shutdown`, and a unit
test keeps the downstream side open while asserting that an upstream close
still reaches it. The OCI Adapter's five tests and all 83 Connected Worker
tests passed.

The first restart also exposed a separate lifecycle bug: re-import returned a
successful receipt from a lease whose expiry had passed but whose database
status remained `ready`. Hosted import now treats a missing, terminal, or
expired lease as requiring a new Run and verification attempt. Its focused API
test asserts that the new response is pending with no retained receipt while
the completed old job remains immutable. The focused test and API typecheck
passed.

The final fresh attempt consumed the same v3 file bytes:

- bundle SHA-256:
  `sha256:88e4fd8d7ec46db05347bd4b03cfb3ceb7598e31fadd233d2bb68bb55df860e6`
- ContractRef:
  `sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19`
- OCI DerivationRef:
  `sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f`
- Run: `run_01M2NDDWA1EY6SM1XYNDE440N9`
- lease: `01M2NDDWFAAG1YD5ZHJKJV9EDY`
- verification attempt: `pav_01M2NDDWRTYP7D9SC8SPR9XM6A`
- container:
  `2afcb28a6f6bd814261246fd41462aa358b90439b5d411a3f448c7b11aa2e182`
- runtime route: `rrt_01M2NDDWPDCV46Q5D4GK610W82`, generation 5,
  upstream `https://s0-rstg002.ato.run`

The receipt is `ato.contract-verification-receipt/1`, carries those exact
bundle, K, D, Run, lease, attempt, image digest, and container values, and has
three satisfied observations with `fully_satisfied: true`. The PWA separately
reported `Contract: Verified` and `Surface: Ready`. In the real browser the
Datasette 0.65.2 top page loaded, the `items` table showed `A / 2` and `B / 5`,
the SQL editor executed `select sum(quantity) as total from items`, and the
result was `7`. Consecutive root, table, and SQL navigations no longer returned
524.

Screenshots are retained outside Git under:

- `.tmp/datasette-evidence/screenshots/pwa-oci-ready-fixed.png`
  (`sha256:881ffc2510582428cab0226b1d0084f5a1f6d782133ea891d5184803393e4955`)
- `.tmp/datasette-evidence/screenshots/pwa-oci-datasette-sum-fixed.png`
  (`sha256:dd1a4b537cda9a63dd8649a23845fa365058a5f80cf91e7b53160fd0bca0e858`)

This completes the Hosted OCI Surface acceptance item. It does not complete
Portable v0: image-absent outbound-blocked offline acceptance, Hosted/PWA v4
transport, durable Instances, saved-data/Asset round-trip, Bindings, User
Runner placement, and authoring integration remain separate work.

## Portable v4 Hosted transport staging acceptance (completed)

The next stacked change used these revisions:

- ato `e877f064`
- ato-api `31f12a0f`
- ato-pwa `3f3cf89`

The API and PWA accept the explicit version/profile pairs v3 /
`ato.portable-application/1` and v4 / `ato.portable-application/2`. The
transport limit is 256 MiB while the Worker request-body lane remains bounded
at 32 MiB. Larger files must use the existing presigned R2 PUT path; the API
does not buffer or proxy their body. Migration
`0269_portable_application_v4_transport.sql` was applied to staging only.
Staging API Worker version `b1d215d8-a43e-4302-a27f-44234ad4b364` and PWA
version `63cf061a-f877-46ff-aaef-a979e056a043` served the acceptance build.
Production was not changed.

The first browser attempt reached `prepare` but the presigned PUT failed
before validation because `ato-store-artifacts-stg` had no browser CORS rule.
The committed staging configuration permits only
`https://stg-app.ato.run`, only `PUT`, and only the `Content-Type` request
header; it exposes `ETag` and does not make the bucket public. After applying
that configuration, the same browser flow advanced through Checking,
Uploading, Verifying, Hosted Run, and Surface readiness.

The accepted file was the existing byte-identical offline v4 bundle:

- bundle ID: `bnd_01M2NHGRB165RP8H9W6EDWH04W`
- size: 120,583,935 bytes
- bundle SHA-256:
  `sha256:f439923321bfa748af3a8993ab4c7c495f8f0d81ae5f9504c8add92b539591a2`
- ContractRef:
  `sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19`
- OCI DerivationRef:
  `sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f`
- Instance: `cinst_01M2NHSEYJK5YZ6YGZG24T7KVR`
- Run: `run_01M2NHSF2D0EFZZ2YVQ7WQGJNF`
- lease: `01M2NHSF6NJESM41JCG4NGZKJW`
- verification attempt: `pav_01M2NHSFKC6YG8NZXH67G3EC7R`
- container:
  `95dc304649a6c833c8c3e2b30f08c5a2165b69d098865784178c742c3edebad3`
- runtime route: `rrt_01M2NHSFGD4W4AS9ZQJPT9A6ZJ`, generation 1,
  status `ready`, Runner `01KX0SWDPP2GA41NEXQXNDCC0D`

The persisted hosted receipt is `ato.contract-verification-receipt/1`, carries
that exact bundle SHA, K, OCI D, Run, lease, attempt, pinned image digest, and
container ID, and reports the three original observations as `satisfied` with
`fully_satisfied: true`. The PWA separately displayed `Contract: Verified`
and `Surface: Ready`. In the newly created public Surface, the real browser
opened Datasette, showed `A / 2` and `B / 5`, and used Datasette's SQL editor
to execute `select sum(quantity) as total from items`; the returned total was
`7`.

The exact `e877f064` CLI also ran the same file through both local
Derivations. Python on macOS satisfied all three observations. OCI was run on
the compatible Linux/amd64 validation host with Docker Engine 29.1.3 and also
satisfied all three; its local receipt records `embedded_oci_image_loaded`.
The macOS Docker Desktop attempt was interrupted after its documented private
bridge-IP reachability failure and was not counted as a PASS. Re-exporting the
offline v4 input as offline produced byte-identical bytes and the same SHA,
while v4 planning/export tests assert that packing policy changes do not
change K or either D.

Focused validation passed:

- `cargo fmt --all -- --check`
- `cargo test -q -p ato-portable-application` (19 tests)
- `cargo test -q -p ato-cli --test portable_application` (7 tests)
- API `pnpm typecheck` and the v4 transport tests
- PWA `npm run typecheck`, four portable client tests, and `npm run build`

Three failures in the broader API capsule-network route suite reproduced
unchanged on its parent branch under the same command: unavailable public
Runner capacity, the newer `managed_pool_unavailable` error code versus the
old assertion, and a live-continuation 409 versus 202. They are not counted as
new v4 regressions. Portable v4 Hosted transport was accepted here; the later
sections in this record and the Instance snapshot record close the isolated
offline and durable saved-data acceptance items.

## Empty-store, route-less offline OCI acceptance (completed)

The final local OCI gate ran on `ubuntu-sugamo`, Linux 7.0.0-27 x86_64 with
Docker Engine 29.1.3. The CLI binary was built from ato `77973a62`; its
SHA-256 was
`2cb111ea4ef721afcd3f06ebd7b382071d2d11661029a8a4123bc41eda0f65e6`.
The reusable harness is `scripts/portable-offline-oci-airgap.sh`, completed by
`4d9f75af`.

The harness enters a new network namespace containing only loopback, starts a
private Docker daemon with a new data root and no preloaded images, and leaves
the host mount namespace shared with containerd/runc. The daemon may create
only its disconnected `docker0` route; the namespace has no default route.
Docker's containerd snapshotter is disabled for this private store so the
classic image store is both empty and scoped to the new data root. The Ato OCI
Adapter still creates its per-Run Docker network with `--internal`, uses
`--pull=never`, and receives no Docker socket in the workload.

The exact previously accepted offline file was used without regeneration:

```text
file          datasette-offline.capsule
size          120,583,935 bytes
bundle SHA    sha256:f439923321bfa748af3a8993ab4c7c495f8f0d81ae5f9504c8add92b539591a2
K             sha256:d4bb7e9ae1be0f6b884d0d58561d7f974092de447da3eea9e2ffe5279b004a19
OCI D         sha256:d3e7428a6c75e7d5726319ca61f06631e1811a698b2dd16b531029038dfd701f
image         docker.io/datasetteproject/datasette@sha256:0f57db16cf4eb6cca57f1cedaa0a696bca1c65a1d75b8f7ee372c2dd909a32a0
platform      linux/amd64
```

Before the Run, `docker image ls -q` returned zero entries. With no external
route, `ato run` loaded the verified embedded manifest, config, and seven
layers and emitted a `cli-local` receipt with `fully_satisfied=true`. All
three original observations were performed against the live container and
were satisfied:

```text
datasette-entry      HTTP 200  body sha256:51dd0ae5820a17212e335917cc29e0e83ed343e09cd5b3f8c14dc57e92ea9898
items-in-id-order    HTTP 200  body sha256:8607a7b55d2f0b4f9169a1c63b1554a60a6af1c8424ab591aa9a451265e0ee83
quantity-total       HTTP 200  body sha256:b4d3392b69f97f35ad0c927a0401766b538ebbf1baabd4ee14f620b0ac54632f
```

After verification, the Adapter removed the container and per-Run network.
The harness recorded zero remaining containers and zero final images, stopped
the private daemon, and left no socket or matching process. The immutable test
evidence is retained outside Git at:

```text
/home/ekohsuke/.ato-staging/.tmp/portable-datasette/export-policy/
  offline-airgap-evidence-2026-09-17/
```

Its receipt SHA-256 is
`d7f34075429a8b73b48c7599ce631d07a90c4a9cba2cc7e72aaa8fa410a21d9c`.
Failed diagnostic data roots, their four dedicated containerd namespaces, and
the temporary decoded image tar were removed after the passing evidence was
copied. They are not recoverable; the source Capsule and retained evidence
were not removed.

This completes the image-absent, external-route-less offline acceptance. It
does not claim that the host kernel or Docker Engine is transported in the
Capsule: those remain declared host capabilities, as required by the offline
profile definition.
