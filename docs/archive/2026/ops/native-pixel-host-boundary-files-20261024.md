# Native Pixel + Instance files under a root HostBoundary Runner

Date: 2026-10-24. CPU-only Linux Docker acceptance; no installed/shared Runner,
service, API/PWA deployment, runtime flag, production credential or paid resource
was changed. This child is based on integration/native-files-managed-v1
`a5f3105849d467fd7cfcc57975ff1e9fdf74ac54` (#1533 + #1534).

## Change and boundary

The existing trusted HostBoundary UID/GID now supplies the physical Native GUI
application/helper identity. Runner construction enables that existing boundary
and checks workspace reachability before helper preparation. Preparation freezes
the helper user privately. A conflicting explicit application UID/GID is rejected
before materialization or native input. Ordinary nonroot execution retains its
existing helper ownership.

Only the freshly created `display/x11` and `display/rfb` directories are handed
to the configured nonroot UID/GID. The runtime/work root, application source,
auth mount and fake Runner credential canary retain their original owners and
modes. No world-writable mode, arbitrary-D-user chown, new authentication or
semantic identity was introduced. Existing common OCI data mounts enter only the
application. The display helper receives neither input nor output mounts.

Security review covered the three production files without edits or Cargo use;
no new P1 or higher finding. The reviewed production diff SHA-256 is
`edf6d5051e2a2969cae0d908edacde4972cfd3504c3a5b59f2eca3f923ee27fe`.

## Frozen integration baseline, before this child

The unmodified a5 integration was exported into a fresh Linux directory. All 562
tracked Rust/Cargo inputs matched the manifest before/after tests; manifest
SHA-256 `3e40c30995520257bc00873d648962751a91439c07b422fc27c9a03b754374aa`.

- Default serial libraries: 471 passed / 23 default ignored (worker169,
  runtime176, OCI38, netd83, gateway5).
- Native matrix: 28 passed, including 13 real Docker acceptance cases.
  Terminal lease 2.574s; Pixel original-K positive 3.968s and independently
  authored mismatching-K negative 3.984s. Positive receipts were fully satisfied;
  the mismatch remained false.
- Common file actual: ordinary UID 5.093s; root boundary UID/GID65534 2.500s.
  Read-only input, output surviving stop, conflicting UID rejection and zero
  owned resources were checked.
- Five-crate all-target Clippy and workspace fmt passed.
- Managed Broker CPU fixture cases were **not rerun** in this integration
  acceptance; their separate #1532 evidence is retained. The local TLS fixture
  authority was already stopped.

## Root GUI + files acceptance and controls

The added ignored exact test is
`native_dispatch_tests::real_host_boundary_pixel_lease_preserves_original_k_and_instance_files`.
It starts in a fresh process with `host_boundary::active() == None` and invokes
`ConnectedWorker::new`; it does not manually enable the boundary first. All
control/API credentials are fake and local. It uses actual Docker attach,
TigerVNC, scoped gateway, private HTTP/Rust K proof, Data Plane input delivery and
stop-time output upload.

The static GUI fixture reads a granted seed input, confirms write-open fails,
copies the exact bytes to the granted output directory, and only then paints the
original expected green frame on its first input. The local server verifies the
uploaded path, size, digest and bytes and records completion. Tests inspect live
application RO/RW mounts, helper mount exclusion and configured user, private
child directory ownership, scoped authorization failures, pre-materialization
wrong-UID refusal, original K, receipt and complete stop/journal/listener/container/
network cleanup. A root-owned 0600 fake credential canary remains unchanged.

First fixed run: helper preparation 6.852s, lease 4.607s, original K satisfied,
`result.json` saved; one exact test passed in 12.20s. Preparation timing is kept
separate from lease timing. This is cached helper admission, not a claim about
cold archive loading or shared staging readiness.

Two controls are deliberately distinct:

1. **Original a5 production + new acceptance harness:** old production files,
   only the new test harness overlaid into a fresh a5 export. Actual root
   construction reported Native Pixel unavailable because the helper selected
   UID0, and the explicit availability assertion failed in 6.97s before lease
   publication/K/receipt. This reproduces the combined root GUI + files gap.
2. **New helper implementation + only old constructor ordering:** the frozen
   helper identity validation rejected UID0 during construction, exit101. This
   tests sensitivity to the new required order. It is not evidence that the old
   ordering alone was a permanent production failure: the old readiness check
   logged failure and could dynamically reevaluate identity later.

The existing workspace ancestor search guard remains unchanged. `/home/ekohsuke`
is private and therefore unsuitable for this root construction acceptance. Tests
used the existing searchable `/opt/ato` workplace with a newly owned 0711 `.tmp`
and uniquely marked temporary children, without changing any existing parent,
service or configuration permissions. Initial placement/umask harness failures
were retained separately; they emitted no successful lease or K receipt.

## Final child regression

The fixed child used a fresh 562-input manifest with SHA-256
`b7cc969d7e218be9bd3e50999d2a461064fc53ce60bf81318f665fed9a7fd824`.
Default serial libraries passed 471 tests / 24 default ignored; the additional
ignored test is the separately executed root acceptance. Ordinary Native matrix
passed 28/28 in 195.06s. Terminal lease 2.940s, Pixel positive 4.418s/K true and
Pixel negative 3.835s/K false. The final fresh root exact test also passed:
preparation 6.884s, lease 4.516s, total 12.09s; original K satisfied, output saved
and all owned resources reclaimed. Five-crate all-target Clippy (`-D warnings`),
workspace fmt and before/after input hashes passed. The uniquely marked temporary
root fixture directories were then removed; existing workplace permissions and
services were left in place.

## Reproduction and remaining limits

Shared target: `/home/ekohsuke/ato-run/.tmp/native-surface-20261023/runner-source/target`.
Cargo is `/home/ekohsuke/.cargo/bin/cargo`; use jobs2/debug0/incremental0,
`--offline --locked`, fd limit8192, fresh short workspace `.tmp`, serial tests.
Refresh all own Rust/Cargo input mtimes and verify the content manifest when
switching source directories; do not rely on a shared target's stale rmeta.
Build the same-source worker binary and set `ATO_TEST_WORKER_BIN` for process
sandbox tests. Library-test executables are not valid sandbox shims.

CPU inputs:

- Python `python@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b`.
- Helper `ato.local/native-helper@sha256:7076a8aadff1e92a09ba843928380aecfb00395cde9bce697e926174af04a9bd`.
- Application `ubuntu:24.04@sha256:52df9b1ee71626e0088f7d400d5c6b5f7bb916f8f0c82b474289a4ece6cf3faf`.
- Static CPU fixture SHA-256 `31803c4aab6b549a7174ad727f6adb0b898079a92b7015949d06adfd3814bc6d`.

Root test execution uses `sudo -n env -i` with only minimal PATH, an empty private
Docker config and task-local ATO_HOME/TMPDIR/test roots, helper/archive/fixture
paths, UID/GID65534. The ordinary Native matrix excludes only the separately
executed root-only exact test because it requires that fresh root process.

Original K is `7397f86163f3f61040ef5ebcfef7b2308172502ea89b41471a10be37a2bf7d6a`;
root files selected D is
`sha256:41773d7bacaa723304b98e1d8fd0d964d9f30ecf68c8872cf92bf9344c50bed4`.
The authored 50ms Pixel settle remains unchanged. No observed value rewrites K,
no repeated input/hash polling and no Surface-only success were introduced.

Default-off admission, immutable archives, one slot, exact cgroup capacity,
nonroot/read-only/network limits and matching API receipt/access gates remain
required. Root-owned state mounts and other arbitrary UID mappings retain their
existing restrictions. No shared Runner feature was enabled and no staging
browser end-to-end claim is made by this CPU acceptance.

Task-local evidence was retained under
`.tmp/fgnhi/.tmp/host-boundary-check/`: final logs in `logs/`, original-a5
control in `control-a5/`, exact shell harnesses and input manifest in the parent.
The fresh modified source was
`/home/ekohsuke/ato-run/.tmp/native-pixel-host-boundary-20261024/source`;
the original-a5 control used a separate `native-pixel-a5-root-control-20261024`
directory. These are acceptance scratch exports, not installed artifacts.
Shared Cargo ownership was released to the ModelSet acceptance owner after all
checks; no overlapping builds were run.
