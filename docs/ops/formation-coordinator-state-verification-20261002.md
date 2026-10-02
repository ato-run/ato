# Coordinator-backed Formation state verification

This gate is separate functional acceptance of an existing retained Codex D.
It does not start an ordinary authorized Run, change frozen K, seed the API
producer, deploy a Worker, or apply a remote migration. Source exploration
still does not advertise a persistent-state assignment capability.

The API receiver is pinned to
`fa825a176b715962351466dc1a7149bec5979c7f`. Its existing
`dispatchRuntimeLaunch` and `bindRunToLease` record a verification fixture's
instance, Run, state slot, lease and writer fence in isolated local D1. The
dummy launch spec is used only for these grants; `/bin/true` is never launched.
The real workload is the retained artifact's existing D and common Runtime
attempt, with the original K and Exploration effect ceiling.

`state-verification-receiver.mjs` bundles the clean API pin and a private
fixture dispatch entry. `state-verification-coordinator.mjs` initializes a
fresh local D1/R2 and exposes only the production lease-state routes on its
loopback front port. A mode-0700 filesystem command directory invokes fixture
dispatch internally with an ephemeral private control key. The front port
cannot call fixture dispatch. The entry is not a deployment artifact.

`state-verification-retained.rs` accepts a mode-0600 assignment file with
`api`, `lease_id`, `token_file`, `slot_id` and the dispatched `spec`.
The token file must also be private. The helper validates the spec and fixed
artifact identity, uses existing `LeaseStateArtifactTransport` and
`session::prepare_run`, and projects only the authenticated attachment into
`VerificationStateBindings`. The retained candidate is launched and verified
by `run_attempt`. Each functional replay has its own 300-second deadline;
this is not a resumed Search and cannot replenish a Search's budget.

After explicit confirmed stop, existing `session::commit_run` packs and
uploads state, commits a revision with the current fence, and releases the
writer. An unconfirmed stop quarantines the writer. Failed attempts known not
to have started or confirmed stopped abort the writer. No state bytes, token
values or resolved host paths are emitted to public evidence.

Local preflight passed: no bearer and an invalid bearer return 401; an
unassigned state key returns 404; the assigned grant returns its current
fence. The helper passed an offline compile and clippy with warnings denied.
Actual Linux Runtime stop/commit/restore/UI persistence is pending in this
commit. These checks do not complete Kutt or the API producer arm.

Build the helper in an owned `.tmp` external Cargo project pointing to the
tracked Rust source. Copy the workspace `Cargo.lock`, resolve offline, then
use `--locked --offline`; it needs the existing Connected Runner, Formation,
Formation Worker, Runtime attempt, IPC, netd, anyhow, serde/serde_json and the
Connected Runner's reqwest 0.12 features. Record the exact helper source,
lock and binary digests before execution. Freeze the receiver bundle hashes,
retained descriptor/archive and original K/D in the preregistration.

Preserve previous local-fixture state evidence and every UNKNOWN attempt.
No model call is needed by this functional gate. A successful gate proves
the authenticated transport integration of the retained Runtime path, not
production availability of Formation state provisioning.
