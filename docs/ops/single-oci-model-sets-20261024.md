# Single OCI Model Set profile acceptance — 2026-10-24

Base: a5f3105849d467fd7cfcc57975ff1e9fdf74ac54.
Branch: feat/portable-oci-model-inputs. API companion: fix/oci-model-data-admission.

The portable validator previously rejected every non-Process route containing
Model Sets, including Native single OCI. Allow the existing OciContainer
realization to carry verified model manifests, preserving Static/group refusal.
Report projection, digest/closure validation, ModelCache, Data Grant and common
oci_data::attach are reused. No K, D, wire, profile, provider or schema changes.
Native remains presentation of the same single OCI realization. Format admission
does not grant devices or local execution; the common local executor's default
model-delivery refusal remains intact. Hosted API admission requires both the
single OCI Data Plane feature and HostBoundary capability plus active execution
authority; it must be released with the API companion.

Red: canonical single OCI/Native model-input test failed on the original
process-only gate. Green: portable application lib 116/116, runtime-attempt lib
175/175 and hosted validator integration 11/11. This includes both Terminal and
Pixel canonical bundles, unchanged ContractRef, verified manifest-only bundle
closure and report metadata, payload tampering rejection, explicit unchanged local runtime model/Native
refusal without a capable executor, legacy Process, and
explicit Static/group refusal. Common OCI projection tests now include a Model
Set and Asset together, verifying both RO mounts and separate writable output.
All-target Clippy with -D warnings for both crates and cargo fmt/check passed.
Logs are under this checkout's .tmp/model-*.log. No tests were disabled.

Commands use TMPDIR=$PWD/.tmp and CARGO_TARGET_DIR=$PWD/.tmp/cargo-target.
The real Linux Docker test has been extended to hydrate/verify/materialize a
small Model Set through ModelCache, read it in the real container, reject its
write, combine it with an Asset, stop the owned container, and save the output
using the existing reserve/part/complete protocol against a local HTTP fixture.
Linux actual CPU Docker acceptance passed from source
`e5b5b9d2289e0dcd59368da5d34c739881dcf63d`, using the exact Cargo-reported
runtime-attempt test executable built from an independent source mirror.

| Actual mode | Elapsed after cache preparation, including spawn, I/O, stop, save and negative cleanup |
| --- | ---: |
| Non-root Runner / matching OCI user | 2,772 ms |
| Root HostBoundary / explicit 65534:65534 workload | 2,490 ms |

Both verified the real read-only model mount and Asset mount, denied writes to
both, combined their bytes in output, confirmed stop, then saved the result via
existing reserve/part/complete HTTP protocol. The fixture verified returned
Asset identity and uploaded bytes. A mismatching workload user was refused;
owned container/network scanner found no remaining resource. Cache verification
also succeeded after workload stop.

Image already present on the existing Linux host:
`python@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b`.
No pull, GPU or paid resource was created. Coordinator/save target and initial
model hydration are fixtures; the container, mounts, denied writes, stop and
multipart client are real. This is not an actual Hosted API/R2 save or a full
Native Surface acceptance.

All 564 tracked Rust/Cargo/config hashes matched local before and after the
Linux runs; input mtimes were refreshed before serialized shared-target reuse.
Manifest SHA-256: `fcbe7bb01e623eb3f079a0574ba8e6a43664734b8a3065a019a41187ed1bb4d1`.
Mirror: `/home/ekohsuke/ato-run/.tmp/model-oci-e5b5/source`.
Cargo used offline/locked, jobs2, debug0 and no incremental compilation. Runs
used fd limit8192, distinct fresh ATO_HOME/TMPDIR/exploration roots, empty
DOCKER_CONFIG, and env-i; the root mode invoked only the exact same-source test
binary with explicit HostBoundary UID/GID. No Worker suite used an older Worker
binary. The shared target was explicitly released to the component-input task
after both exact tests; this source mirror and local logs remain as evidence.

No staging/production deployment, feature switch, migration, real hosted Run,
paid provider call or production key was used. Fixture Coordinator and cache
hydration do not establish actual Hosted API/R2 or GPU/provider acceptance.
Group input/output placement remains a separate implementation slice.
