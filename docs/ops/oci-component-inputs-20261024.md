# OCI component input placement — 2026-10-24

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
