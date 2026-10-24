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
Execution measurements will be appended after shared Linux target handover.
The source mirror/target are owned by another active task until that handover.

No staging/production deployment, feature switch, migration, real hosted Run,
paid provider call or production key was used. Fixture Coordinator and cache
hydration do not establish actual Hosted API/R2 or GPU/provider acceptance.
Group input/output placement remains a separate implementation slice.
