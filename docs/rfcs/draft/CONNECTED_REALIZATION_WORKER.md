# Connected Realization Worker

Status: draft application-integration ADR (2026-08-24)

## Decision

The hosted runtime consumer is a small application component, not a new Core
primitive and not a revival of `ato runner serve`.

It consumes the current control-plane contracts:

1. runner heartbeat;
2. `GET /v1/runners/:id/leases/next`;
3. `portable_capsule_v2` commands;
4. lease-scoped object-graph index and object downloads;
5. forward-only lease status, ready, control, and stopped reports.

Every downloaded graph is passed through `ato-runtime-object-graph`, the same
application library used by the independent Validator Agent. The API's ready
state is authorization to download, not authorization to execute without local
verification.

## Runtime assembly

The worker constructs a restore-only registry containing
`ato.materialize.vm.snapshot@1`. It does not call the ordinary CLI registry and
does not register Source, Replay, or OCI fallback paths. After local graph
validation it supplies live `RunnerCapabilities`, the VM Materializer,
ActuatorProvider registry, ContractVerifier registry, port bindings, and a
Hosted/TenantIsolated environment to `RealizationPlanner`.

A separate capture-capable factory requires all three physical owners:

- an `ActiveFirecrackerRealization`;
- its `RecordWriter` Capture Barrier;
- a capture work root.

That factory uses `FirecrackerBackend::with_capture_source`. Restore-only
workers use `FirecrackerBackend::new`, keeping accidental live capture out of
ordinary CLI and restore processes.

## Single-cut capture

`VmCaptureRequest` carries only the existing target ComputationRef. The active
capture source performs exactly one Capture Barrier after ingress freeze,
interaction quiescence, and VM pause. The returned `CapturedVm` owns the sealed
frontier reference. `VmSnapshotMaterializer::encode` validates that returned
frontier and writes it into the descriptor. Neither the request nor
`MaterializerContext.record_frontier_ref` supplies a precomputed VM frontier.

## Firecracker Surface boundary

Each restore owns a network namespace containing the snapshot's logical TAP
name. A worker-internal relay process runs inside that namespace and forwards
the guest TCP endpoint to a per-lease Unix socket. This allows concurrent
restores to reuse an identical guest address without a host route collision.

The socket chain is deliberately two-stage:

```text
guest TCP
  -> netns-only relay
  -> per-lease UDS
  -> hidden loopback Contract relay
  -> [Contract PASS + Realization.publish]
  -> published loopback slot relay
  -> existing runner ingress
```

The published listener is not bound until `accept_candidate` has completed.
The TAP can therefore exist while the external Surface remains unreachable.

## Fresh bootstrap boundary

`FreshFirecrackerRealization` reuses only low-level Firecracker mechanics:
kernel boot, relative rootfs backing path, namespaced TAP, vsock, pause/resume,
full snapshot/create, and cleanup. It accepts a bootable current rootfs already
associated with the caller's existing ComputationRef. It neither builds a
ComputationRef nor interprets application-specific state.

The current Draft stack still needs a staging integration that turns the 2048
current Source/Replay candidate into that bootable rootfs. Until that physical
input exists and is exercised on Linux/KVM, the worker must not report VM
Acceptance.

## Failure policy

- expired, malformed, wrong-Bundle, or wrong-root leases fail before restore;
- corrupt bytes, forged semantic references, a forged VM target, or an invalid
  RecordFrontier fail in the shared graph validator;
- Planner incompatibility and VM restore failure are VM failures, with no
  fallback;
- Contract or publication failure quiesces the candidate;
- stop removes the published relay before quiescing the VM and deleting the
  per-lease graph/cache directory.

