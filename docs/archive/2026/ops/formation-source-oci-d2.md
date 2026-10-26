# Formation 6b-D2 — source → OCI materialization (offline integration)

> Later: the builder boundary below (caller-provided private daemon) was
> replaced by a CLI-owned private session; see
> [formation-source-oci-d2-hardening](formation-source-oci-d2-hardening.md).
> This first ledger is kept as recorded.

## State

**implemented · unit-tested · actual offline integration verified** on
`ubuntu-sugamo` (x86_64, Linux 7.0.0-27-generic x86_64 GNU/Linux, Docker 29.1.3).
**Not merged, not deployed. WBO actual not run** (needs the explicit online
policy and the profile changes in the [design](../rfcs/draft/FORMATION_SOURCE_OCI.md)).
Model calls 0; no remote migration; #1421 untouched; host network never granted.

- Code: `b6a3ee784bef`, `ato` sha256 `6c15d42abe0beaab…`.
- Platform linux/amd64 (the existing OCI executor needs Docker; the aarch64
  host has none). aarch64 is deferred.

## Path

frozen source archive (digest-verified) → root `Dockerfile` + context →
BuildKit in a **new private dockerd** (own data root, empty classic store,
loopback-only netns, `--network=none --pull=false`, production socket refused)
→ `docker save` by image ID → **repack to referenced blobs only** → existing
`verify_oci_archive` → authored `ato.capsule/2` OCI route → existing `ato pack`
→ existing `ato export --portability offline --oci-archive` → existing
`scripts/portable-offline-oci-airgap.sh` (`ato run`, new empty private store,
no route) → Verifier receipt.

## Materialization evidence (fixture)

- Base: `python:3.12-alpine@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b` frozen archive `sha256:4efd64c75d1eedff…`
  (read-only docker save from the host daemon before the build (host daemon never given to the builder); host image/container counts 67/5 before and after).
- Source closure `sha256:2e483639041e96f1…`, Dockerfile `sha256:78f8fe80843e5b05…`.
- Output image `ato-source/2e483639041e@sha256:356bb232dd7ed005605e38c28f297600083579e385550119e5f88301a217772c`; config `sha256:544ae56d0f542f45…`;
  6 layers; archive `sha256:d3d2cec402b37d33…` (51189760 bytes);
  repack dropped 6 legacy v1 layer-JSON blobs a classic store writes.
- Image config read from the artifact: Cmd `python3 -m http.server 8080 --directory /srv`,
  ExposedPorts ['8080/tcp'] (= explicit binding 8080), WorkingDir `/srv`.
- Profile divergence recorded: the current OCI route runs with `--workdir /app`
  (harmless here: the fixture's Cmd uses absolute paths only).
- Build ~3306 ms. Reproducibility is not claimed.
- Pack: ContractRef `sha256:9bf1dde591e75171…`, DerivationRef `sha256:ddb304a0a7d008f1…`; offline bundle 68260837 bytes.

## Runs from empty private stores (cache-less reuse of the saved artifact)

| Run | fully_satisfied | Receipt | Observation | Store images / containers after |
|---|---|---|---|---|
| run-1 | `True` | `sha256:624a777db3f3500a…` | satisfied (GET / → 200) | 0 / 0 |
| run-2 | `True` | `sha256:6bd0b2c08b765351…` | satisfied (GET / → 200) | 0 / 0 |

Same bundle, K and D in both runs; the embedded image was loaded each time.
**K is GET / = 200 only: typed-K PASS, not functional success.**

## Negatives (each on a new private daemon)

| Case | Code | Cause observed | Private store images / containers after |
|---|---|---|---|
| archive_bound | `source_oci_artifact_bounds` | error: source_oci_artifact_bounds: 51189760 bytes exceeds the archive bound | 0 / 0 |
| base_digest_mismatch | `source_oci_base_digest_mismatch` | error: source_oci_base_digest_mismatch: python:3.12-alpine@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b archive bytes differ from the frozen digest | 0 / 0 |
| build_network | `source_oci_build_failed` | 5.274 wget: bad address 'example.com' | 0 / 0 |
| build_timeout | `source_oci_build_timeout` | error: source_oci_build_timeout: build exceeded 5 s | 0 / 0 |
| context_escape | `source_oci_build_failed` | ERROR: failed to build: failed to solve: failed to compute cache key: failed to calculate checksum of ref e37b4b9d-3424-4662-9aa0-4ef0b696ebef::ia4knphas0nfdr459f6lqsxt6: "/outside.txt": not found | 0 / 0 |
| host_network_run | `source_oci_build_failed` | ERROR: failed to build: failed to solve: failed to load LLB: network.host is not allowed | 0 / 0 |
| privileged_run | `source_oci_build_failed` | ERROR: failed to build: failed to solve: failed to load LLB: security.insecure is not allowed | 0 / 0 |
| production_socket | `source_oci_builder_socket_refused` | error: source_oci_builder_socket_refused: the host's production Docker socket is never given to a source build | 0 / 0 |
| store_not_empty | `source_oci_builder_store_not_empty` | error: source_oci_builder_store_not_empty: the private builder store must be empty; no other job's images or cache | 0 / 0 |
| unauthorized_external_input | `source_oci_build_failed` | ERROR: failed to build: failed to solve: alpine:3.20@sha256:0000000000000000000000000000000000000000000000000000000000000000: failed to resolve source metadata for docker.io/library/alpine:3.20@sha256:000000000000000000000000000000000000000 | 0 / 0 |

No failure fell back to host execution. The host daemon's image/container
counts were 67/5 before and after all runs.

## Not covered here

Stop-unconfirmed handling is the existing OCI Adapter's (`SpawnCleanupUnconfirmed`),
not re-exercised. No WBO, no online build, no functional or persistence check.
