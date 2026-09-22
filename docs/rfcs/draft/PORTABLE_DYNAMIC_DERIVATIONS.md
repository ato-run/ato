# Portable Dynamic Derivations

Status: draft implementation contract  
Date: 2026-09-16

## Decision

`ato.portable-application/1` keeps `CapsuleId = ContractRef`. A Python
process and an OCI container are alternative Derivations of that Contract,
not new Capsule or Application kinds. Runtime facts such as executable path,
PID, container ID, image availability, endpoint, Run, lease, and attempt are
receipt evidence and never inputs to `ContractRef`.

This draft adds versioned descriptors without changing the meaning or digest
of existing objects:

- `ato.application/2` describes a logical Surface by Port and initial path.
  It does not require a static artifact or a file matching an HTTP path.
- `ato.portable-tree/2` permits general immutable workspace files while
  retaining path, size, media type, and content digest validation.
- `ato.process@1` selects the generic process Adapter. Its Derivation declares
  runtime constraints, argv, cwd, environment, workspace input, and HTTP Port.
- `ato.oci@1` selects the Runner-owned OCI Adapter. Its Derivation declares a
  digest-pinned image, platform, resource limits, argv, environment, read-only
  workspace input, and HTTP Port.

Existing `ato.application/1`, `ato.portable-tree/1`, and Static Web behavior
remain valid and retain their prior digests.

## Four boundaries

1. Bundle validation proves schema, canonical digests, reference closure,
   declared Derivation membership, and descriptor consistency for every D.
2. Runtime admission is applied only to the explicitly selected D. It checks
   runtime and platform availability, pinned dependencies or image identity,
   privileges, and limits. An unavailable OCI runtime does not invalidate a
   valid Python route.
3. Execution materializes the common workspace read-only, starts the selected
   process or container, establishes its logical Port, and owns cleanup.
4. Contract verification observes that Run's HTTP endpoint and compares the
   result with the original K. It cannot recapture or rewrite K.

A dynamic HTTP observation path is an endpoint, not a workspace path. Static
file lookup is a Static Web optimization only.

## Hosted execution

Hosted import transports the original bundle through quarantine and the Rust
validator. It stores the validator-produced workspace artifact, persists the
explicit DerivationRef, and dispatches through the existing Hosted Run,
operator-managed Runner, lease, runtime route, and Surface path. Formation and
Cloudflare Workers do not own application processes.

The verification job is claim-fenced and waits for the selected lease to be
ready. A successful hosted receipt must match bundle SHA-256, ContractRef,
DerivationRef, Run, lease, attempt, realization, and the exact set of K
requirement IDs; every observation must be `satisfied`.

## Isolation and lifecycle

Process execution uses a per-Run read-only workspace, writable runtime
directory, and OS-native sandbox. It may bind only Runner-assigned HTTP ports;
TCP egress is denied unless a future Derivation explicitly declares an allowed
destination. A Hosted Runner that cannot fully enforce this network policy must
fail admission. OCI execution additionally requires a read-only root,
read-only workspace mount, internal network, loopback-only Runner forwarder,
dropped capabilities, `no-new-privileges`, PID/memory/CPU limits, and no Docker
socket or privileged mode. Cancellation, timeout, lease expiry, readiness
failure, and normal stop must terminate the process/container and release the
Port and temporary resources without affecting another Run.

## Current interoperability gate

The first real-application gate is Datasette 0.65.2 with one immutable
`catalog.db`. One byte-identical bundle contains a Python 3.12 route with a
hash-locked wheelhouse and a `linux/amd64` OCI route pinned by image digest.
The OCI route is explicitly network-dependent until OCI image bytes can fit
the existing bundle transport; that limitation must be reported rather than
hidden by increasing the upload limit.

The same K requires an HTTP 200 entry response, the two ordered `items` rows,
and a quantity sum of 7. Acceptance requires fully satisfied receipts for
local CLI and staging Hosted Runner execution of both Derivations.
