# Single-container OCI Model Set delivery

Status: draft implementation contract, 2026-10-24.

This extends DATA_PLANE_IMMUTABLE_INPUTS.md and preserves the accepted
PROTOCOL_ADAPTER.md / MATERIALIZATION.md boundary: input identity is logical;
mount paths, Runner identity and cache placement are realization evidence.

The existing portable application profile validates the existing canonical
ato.model-set/1 manifests for LocalProcess and single OCI routes, including
Native OCI presentation. Static Web and OCI service groups retain explicit
refusal. No wire/profile/semantic schema or Contract/D canonicalization changes.
Every selected input still pins its manifest digest; external model objects
remain outside the bundle and are verified by the existing ModelCache.
Validation admits a shape; it neither proves delivery nor grants execution.

Hosted single OCI routes with Model Sets require both advertised capabilities
runtime_feature=oci_data_plane_v1 and isolation:runtime_launch=host-boundary-v1,
and active execution authorization. Selection and the final capability read
must both enforce them before claiming storage or creating a Run. Existing
Native file admission uses the same requirements. Routes without delivered
inputs preserve their existing grant-free compatibility path; Process retains
its current delivery/authorization policy. A missing Grant is never a valid
execution path for a selected OCI Model Set route.

Use the existing Data Grant, Runner delivery and cache materialization. Pass
only granted, verified read-only input directories to oci_data::attach. It
creates guest paths under /.ato-data/inputs and ATO_INPUT_PATH_<ID>; the private
cache, credentials and host paths never enter the container. Outputs use the
existing bounded ATO_OUTPUT_DIR, separate from workspace/state/input mounts,
and are collected through the existing stop/save path. Native presentation
and Contract readiness remain separate; this change grants neither devices
nor Surface capabilities and does not make the CLI local OCI executor deliver
models automatically.

Component-specific inputs and shared writable state are outside this slice.
The group refusal must remain: there is no existing component input/output
assignment in BoundStep or launch spec v2. Do not broadcast input files or
outputs to sibling services by removing that guard.

Validation gates: canonical single OCI/Native manifest acceptance and report
projection; unchanged K, legacy process and wire behavior; corrupt/noncanonical
manifest rejection; Static/group refusal; required capability loss and missing
execution authorization; cache-backed read-only model mounts with separate
output and confirmed stop on real CPU Linux Docker. GPU/provider creation,
deployment, flags and production credentials are not part of this change.
