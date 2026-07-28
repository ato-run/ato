# No-binding restore with a guest-agent vsock device

Status: Draft

## Problem

`ReadyStateManifest.has_vsock` records whether the Firecracker snapshot contains
a vsock device. The restore classifier treated that device as proof of a
runtime-binding channel and rejected every manifest where `has_vsock = true`
but `supervisor_build` was absent.

Pinned-v1 capsules use vsock for the guest-agent control channel during build,
capture and restore even when the capsule declares no runtime bindings. Their
manifest therefore correctly has a vsock device and no supervisor receipt.
Conflating transport presence with binding policy makes these accepted
no-binding snapshots impossible to restore.

## Decision

`supervisor_build` is the binding-policy declaration:

- absent: no-binding artifact; plain `restore_snapshot` /
  `restore_snapshot_preview` only
- present with an empty binding list: zero-binding supervisor artifact; plain
  restore lane
- present with names: binding-required artifact;
  `restore_snapshot_with_bindings` plus runner opt-in

`has_vsock` remains a required restore-compatibility fact for every supervisor
artifact, but a vsock device without `supervisor_build` is a valid no-binding
guest-agent transport. The runner opens no binding service for that class.

The with-bindings lease kind still rejects it as a kind/artifact mismatch, and a
supervisor receipt without vsock remains fail-closed.
