# Run file delivery into single-container OCI

The existing Data Plane delivered authorized Instance Assets/Model Sets and
resolved an output directory, but the common OCI projection ignored both.
This connects those existing runtime-private inputs to the common single OCI
Adapter used by ordinary OCI and Native Terminal/Pixel applications. No new
storage table, file server, semantic root, K/D field or secret binding is added.

Inputs are read-only directory mounts under `/.ato-data/inputs/{index}`. The
existing `ATO_INPUT_ASSETS_DIR` / `ATO_INPUT_PATH_*` environment names point to
those guest paths. The Run output is a distinct writable directory mounted at
`/.ato-data/outputs`, exposed as `ATO_OUTPUT_DIR`; existing Worker output saving
still happens only after a confirmed workload stop. Host paths stay out of
environment values, receipts and identity.

Admission rejects unsafe/duplicate/already-bound environment names, guest
workspace/state mount overlaps, symlink or non-normalized host directory paths,
and output overlap with inputs/workspace/state. A HostBoundary root Worker
uses the existing ownership helper to hand only the validated Run output tree
to its configured unprivileged workload UID/GID. Authored arbitrary UIDs never
authorize chown. An explicit OCI user that differs from this configured output
identity is refused before output creation; existing OCI writable-mount owner
and root-denial policy remains in force. No0777 or identity rewrite is used.

No-data launches keep their prior projection. Service groups with Data Plane
files now fail closed with `oci_group_data_plane_unsupported`, because the
current context has no component-scoped file placement; files are not silently
ignored or broadcast to every service. Native display helpers retain their
independent empty workspace and limited X11/auth/RFB mounts.

The Runner advertises `runtime_feature=oci_data_plane_v1` only when both OCI and
HostBoundary isolation are actually available. API #869 requires it together
with `isolation:runtime_launch=host-boundary-v1` and existing execution
authorization for a Native file grant. This does not enable a shared Runner,
feature flag or new secret. Existing capability advertisement stays unchanged
for namespace-only or non-OCI Runners.

## Validation

- macOS: runtime-attempt175/175 and Worker162/162,14 default-ignored, serial;
  both crates' all-target Clippy with warnings denied and format checks passed.
- Linux: runtime-attempt176 passed/3 ignored, Worker163 passed/16 ignored,
  serial; both crates' all-target Clippy and format checks passed.
- A separate real Docker fixture ran twice against the existing pinned Python
  image below: ordinary non-root execution and root HostBoundary execution with
  an explicitly configured65534:65534 workload. Both read the input, rejected
  input writes, wrote output, retained output after stop, refused a different
  declared user and ended with no owned container/network.
- Independent read-only review found the initial root-owned-output gap; the
  shared ownership correction was reviewed and the root case explicitly rerun.

| Actual fixture | Time including I/O, stop and negative check |
| --- | ---: |
| Non-root Runner / matching OCI user | 6,385 ms |
| Root HostBoundary / configured non-root user | 3,478 ms |

Image:
`python@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b`.
These are single CPU fixture measurements, not startup/ready/K or staging
latency. No production key, paid provider, installed Runner, service or live
credential was used. The root case used `sudo -n env -i` for only one exact
ignored test in the newly built test binary, a fresh empty DOCKER_CONFIG, a
fresh owned test root and explicit fixture variables.

All550 Rust/Cargo input hashes matched local before and after Linux testing;
all input mtimes were refreshed before reusing the serialized shared target.
Six-file overlay manifest SHA-256:
`6436e0562c195438c1669456334d4e90ec4d9dcd55eba39ac81ccda0d0ce135e`.
Detailed local records are in `.tmp/oci-data-review/`.

The first Linux test compile exposed missing test-only imports and an unwrapped
labels Result; both were fixed. A macOS parallel full test invocation observed
one `model_cache_busy` failure in the duplicate-digest delivery test. The same
Data Plane source bytes passed once on a related baseline; that does not prove
the intermittent parallel condition resolved. The full changed suite then
passed serially without skips (175 tests). This record does not classify the
parallel failure as a proven pre-existing defect or silently discard it.

## Explicit reproduction and remaining gates

Use fresh ATO_HOME/TMPDIR/ATO_EXPLORATION_TEST_ROOT under owned workspace .tmp.
The CPU fixture needs Linux Docker and a digest-pinned Python image:

```sh
ATO_OCI_DATA_TEST_IMAGE='python@sha256:<existing-image-digest>' \
  cargo test --offline --locked -p ato-runtime-attempt --lib \
  delivered_files_are_read_only_and_outputs_survive_owned_container_stop \
  -- --ignored --nocapture --test-threads=1
```

The root-only variant additionally sets `ATO_OCI_DATA_TEST_HOST_BOUNDARY=1` and
explicit nonzero `ATO_OCI_DATA_WORKLOAD_UID` / `ATO_OCI_DATA_WORKLOAD_GID`, using
the same exact test binary with a minimal environment. Ordinary Worker Process
regression requires the same-source built `ATO_TEST_WORKER_BIN` and fd limit8192.

This proves the OCI file delivery primitive and ownership boundary. Real
staging owner upload→grant→Native Run→stop-time Asset persistence→browser
download is not yet exercised. Input snapshots are fixed at launch; live file
transfer is unsupported. Group placement, incompatible explicit users and
non-HostBoundary data-grant delivery are refused. Clipboard and v1 in-stream
file-transfer capability remain disabled; the separately authorized common
Assets channel is distinct from the RFB/Terminal transport.
