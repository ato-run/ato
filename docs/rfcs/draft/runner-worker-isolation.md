# Runner worker isolation

Status: staged implementation, following the authorized workspace runner split plan (rev.2).

The worker is the shared realization engine. Managed provisioning and User enrollment remain outside it. Coordinator, artifact transport, and backend configuration must be replaceable without changing Computation identity or lifecycle semantics.

## First increment: backend and ingress configuration

`public_base_url` is optional. Private process execution reports a local port without inventing a public URL. API policy can still reject launches requiring public ingress. Firecracker target and TAP settings form a VM-specific configuration group; both are required together, neither is required for a process worker. Only a configured VM backend advertises VM lease kinds and backend capabilities. Browser Activity execution additionally requires the browser executable and run-control verifier.

The existing CLI flags and environment names for configured managed workers are preserved. A public URL cannot carry credentials, query or fragment. Runner tokens are hidden in clap environment-value output.

## Second increment: preserving failed work

Each worker holds an OS file lock for its work root. A nonempty lease directory on startup blocks claims: it may contain uncommitted state or unresolved physical resources. Successful completion removes the lease directory; any execution, commit, report or cleanup error preserves it and exits the worker. Recovery must first prove physical shutdown and preserve/reconcile the working state; deleting the directory or advancing a timeout alone is not recovery. This host-local guard does not replace API allocation generations or quarantine records.

Process execution continues active heartbeats. After reaping a group leader, stop escalates to SIGKILL if residual children remain, and verifies the group is gone before packing state. Linux tests execute the actual worker sandbox shim rather than libtest; readiness for a failed executable uses a real probe, not an always-successful fake.

## Remaining acceptance

This increment does not claim closed execution: the control plane and runtime artifact transports are still HTTP-based. Coordinator abstraction, local/private storage acceptance, retryable recovery/reconciliation and API slot generations remain Phase 2 / Phase 1d work. Managed production replacement remains gated on new-artifact reproduction and the Phase 3 canary.
