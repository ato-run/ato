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
The subsequent actual Linux gate passed. Helper source is pinned to
`e7e75b5249e0f2eaadbf47561a7da6aeaff57a12`; the controller's hidden-file
correction is `26eb1fbca13951d39d53b36af4abf73654fcab53`. Common Runtime code
remains `e185f2d97d869615e23a83f950efb147d317f6b7`. The artifact was formed
at `3ac36fa2739cf8d9555026e93a4ca79905a6d4a7`; no source re-exploration is
claimed at the newer pin.

Two real contained changedetection.io launches passed original K
`sha256:ba864ae817c507b0b277c2d657390ba25fb7f78ecc9566150fd0a7040be36fb6`
and D `sha256:298cb3ac5d58809ba45eb09ef182cbb05ac5fbd1ff55ddeaafbcd8e3c6f153df`,
with distinct fresh attempts. The first Run created, edited and paused an owned
watch in Chromium 149.0.7827.0. Confirmed stop preceded Coordinator commit of
revision `isrev_01M3X99DCPZR6RDVFKM3VT7XQE` at writer fence 1. A separately
assigned Run received fence 2 and restored those bytes through authenticated
`LeaseStateArtifactTransport`. Its fresh browser confirmed the same title and
paused state, deleted the watch and confirmed final cleanup. The final
revision is `isrev_01M3XA0M6Y8FFWTDCQ2C25ESP5`, with the first revision as
parent. Both grants are committed; no active writer or quarantine remains.
Stored revision bytes total 55,296. Both Runtime egress counters are zero.

Nine real Coordinator boundary cases passed: missing/invalid bearer 401,
another active Runner 404, unbound lease 404, unassigned key 404, hidden fixture
dispatch 404, stale commit fence 409, concurrent writer 409, assigned grant 200.
Wrong-fence and competing-writer probes did not alter the current slot or
create a revision. Fixture dispatch itself uses the pinned API service, not
mock grants. Scope records were checked against each actual helper binding.

Preserve the initial pre-execution Coordinator startup failure (AppleDouble
metadata was mistaken for WASM) and two browser deletion failures. The browser
first waited for an absent confirmation button, then tried a native dialog.
Actual DOM showed a custom confirmation dialog; waiting for editor network-idle
and confirming its Delete button produced a complete fresh-browser PASS. The
application, D and K were unchanged during those harness corrections.

Preregistration SHA-256:
`ef56ad1a8093ab75aedcfa443d1dd6fe5861f00e01550ffa6e78bcf495ce08ab`.
Public metadata proof SHA-256:
`d8ef5cc590d002cf25c93573ee3a8021a7fcd6ce056725c8bdc4228ca708eb52`
([proof](evidence/formation-coordinator-state-verification-20261002.json)).
The dedicated Coordinator was stopped after collecting final metadata; both
Runtime stops are confirmed. Earlier fixtures and shared services were preserved.

This completes the owned local Coordinator/retained Runtime round trip and UI
persistence gate. It does not implement production source-exploration state
provisioning, initialize Kutt, verify external watch fetching, or complete the
independent API producer arm. No new inference or paid provider call occurred;
the 23 remaining small-gate calls, monetary budget and original Search state
are unchanged. Ordinary Run permission, deployment and remote migration remain
separate.

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
