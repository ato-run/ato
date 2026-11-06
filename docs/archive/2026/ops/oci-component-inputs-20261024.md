# OCI component input placement — 2026-10-24

> Status: historical evidence / past plan. Language: en.
> Preserved content; relative navigation updated. Current [documentation map](../../../README.md).

Scope: child of single OCI ModelSet support (PR1535 production
`a4e167c921e5dc76d58ac523deb4e27ef0e007c0`). No provider, staging, secret, flag
or shared service mutation. The accepted authority is the selected-D Rust
report → existing lease-bound Grant; no new Core field/table or credential
model is introduced. The draft physical contract is in
`docs/rfcs/draft/PORTABLE_OCI_SERVICE_GROUP.md`.

## Local regression evidence

- Red control: a group with a real ModelSet and an explicit `oci.input_ids`
  assignment was rejected by the parent validator as an unmodelled runtime
  fact (`.tmp/logs/group-profile-red.log`).
- Green: the same bundle passes, preserves K and its manifest digest, emits
  exact per-service selectors, and permits explicit read-only sharing.
- Mac own-target tests cover missing and explicit-empty selectors, legacy
  canonical fixture bytes, null/unknown/duplicate/unassigned selectors,
  normalized environment collisions, missing/extra Grant inputs, no Grant,
  exact logical IDs, environment conflicts, output/input overlap and symlink
  refusal. The unmapped sibling receives no input/output mounts or variables.
- Real loopback Grant delivery tests distinguish missing Grant, legacy empty
  Assets and explicitly selected empty Assets; only the latter creates the
  owned empty directory after the real matching Grant is read.
- Relevant IPC, portable application, runtime attempt and connected Worker
  suites: 546 passed, 16 existing opt-in tests ignored in the initial combined
  Mac run (`.tmp/logs/group-third-suites.log`). Later final checks below supersede
  this intermediate count.

The opt-in Linux two-container fixture uses a pinned public Python image and
canary model bytes, not staging credentials or a real owner Run. It exercises
model hash/cache verification, explicit read-only consumer, sibling with no
input namespace, independent outputs, actual HTTP component responses and the
existing Rust all-K verifier, stop-time output saves with a global count cap,
and cleanup after a failing sibling. Linux results are recorded only after
execution; no mock success is described as Hosted or staging acceptance.

## Real Linux CPU container evidence

The initial real two-container run passed with the existing public pinned image
`python@sha256:c4634f578a412db396771b61b064c6e546c9d6414c7fb5b1b05d5871f1885f7b`:
nonroot 11,962 ms; root HostBoundary workload UID/GID 65534:65534 10,034 ms.
No image pull or GPU resource was needed. The fixture hydrates a canary model
through the real ModelCache, then uses `launch_service_group` and the ordinary
Docker Adapter. Only the consumer gets the model; the sibling asserts absent
input variables and an absent input mount namespace. The model write fails,
both component output bytes survive confirmed stop, reserve/part/complete saves
both component-relative keys, and max_count=1 still caps the whole Run at one.
Real HTTP responses satisfy both original expectations; one missing observation
or a 503 cannot satisfy K. A failed second container stops the first and leaves
no owned container/network. A final test-only extension also checks missing or
foreign output users against HostBoundary before launch.

The model bytes, resolved Grant context and output Coordinator are local canary
fixtures; the container test does not claim an actual API-issued fenced Grant.
This proves common runtime placement and save protocol, not an actual selected-D
API lease, Hosted authorization, R2 persistence, Native Surface, model inference
quality or GPU/provider acceptance. API group admission and factory capability
checks are a separate dependent PR. There is no automatic approval or K capture.

The added IPC vector is `service-group-inputs.json`; the Rust canonical bytes
and digest are `sha256:c38b591b70fb2e101908948bec01daa5d9677f02dfb1d8474e01c10fe89ffdcd`.
The original group fixture/digest remains unchanged. The API copies this vector
rather than adding a canonicalization implementation.

Own Linux mirror: `/home/ekohsuke/ato-run/.tmp/model-group-v1/source`.
All 568 Rust/Cargo/config inputs were hashed against the local checkout and
refreshed before serialized target reuse. Final source manifest SHA-256:
`fbf95b9bdd2d9e2b1343ea583b66a655959bd86f93f528c5a7d7ace48aba8383`.
An initial regression harness used an overlong TMPDIR and hit SUN_LEN in three
session tests plus Chrome abort; that failure log is retained. Tests were then
run from short fresh workspace `.tmp/gmg.XXXXXX` roots with empty Docker config,
fresh HOME/ATO_HOME/exploration roots, env-i and fd8192. The Worker test shim is
a separately compiled binary from these exact source inputs. Shared sources,
prior immutable artifacts and other agents' target contents were not deleted.

## Final related regression results

- Mac: 546 passed / 16 opt-in tests ignored across IPC, portable application,
  runtime attempt and connected Worker; all-target Clippy with `-D warnings`
  passed. Parent single-OCI test/doc head `0f80d0017` was merged, and its five
  ModelSet regressions plus the new Rust golden vector were also rechecked.
- Linux: 548 passed / 23 opt-in tests ignored, zero failures: Worker 169,
  portable application 117, IPC 81 and runtime attempt 181. All-target Clippy
  for those four crates passed with `-D warnings`; fmt and diff checks passed.
- Independent read-only security review found no new P1 or higher issue in
  the production boundary. Its final 11-file content manifest is
  `636f6e9b7cfc2c4bb3cd10331f7943efd6eea9faff160349347ef0c3784b19c1`.
  The only full-file change after the initial review was the parent's 16-line
  test follow-up; production code remained frozen.

No staging/main merge, deployment, feature switch, migration, actual Hosted Run,
Provider API request, paid resource or operator credential was performed.

Final opt-in runs from the same 568-input source manifest (code/test head
`37e58c7f1`) passed after the UID-negative fixture extension: nonroot 10,992 ms;
HostBoundary 10,105 ms. Missing and foreign workload UIDs were refused before
launch. Both source-hash checks after real execution and after normal regression
matched all inputs. The test executable SHA-256 was
`39841b9e54d79f5169ce9e36fccb39516d7fd654da52d3cd196705330cb42391`;
the same-source Worker shim SHA-256 was
`eb3c289fc09a13951e0fa9f56e8dd537066badf9b1dd00f357e917c9d479251f`.
These are local test artifacts, not published or installed Runner artifacts.
