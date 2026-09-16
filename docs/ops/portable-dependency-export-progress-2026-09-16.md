# Datasette dependency export progress — 2026-09-16

This is a partial implementation record, not the final four-route/offline
acceptance. Production was not changed.

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
  compressed layers total 85,165,556 bytes. The current 32 MiB Hosted upload
  limit cannot carry this file.

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
