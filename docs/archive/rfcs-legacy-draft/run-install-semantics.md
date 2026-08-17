---
title: "Run and Install Semantics"
status: draft
date: "2026-05-28"
author: "@egamikohsuke"
ssot:
  - "crates/ato-cli/src/cli/root.rs"
  - "crates/ato-cli/src/cli/commands/run.rs"
  - "crates/capsule/src/foundation/install_lifecycle/"
related:
  - "../accepted/ATO_CLI_SPEC.md"
  - "../accepted/RUNTIME_AND_BUILD_MODEL.md"
  - "ATO_HOME_LAYOUT.md"
  - "APP_SESSION_MATERIALIZATION.md"
  - "BUILD_MATERIALIZATION.md"
---

# Run and Install Semantics

## 1. Overview

`ato run` and `ato install` must describe different ownership contracts before
the project introduces `ato dev`.

- `ato run` is an **ephemeral local production rehearsal**.
- `ato install` is **durable local app registration**.
- `ato dev` is explicitly deferred and is not specified by this RFC.

Both commands may share resolver, materialization, launch, policy, and receipt
machinery. They must not share user-visible identity or state semantics by
accident.

## 2. Scope

### In scope

- Define the command intent for `ato run` and `ato install`.
- Define identity boundaries for run sessions, installed apps/profiles,
  installed revisions, and executions.
- Define state ownership boundaries for caches, sessions, receipts, installed
  registries, profiles, consent, and secrets.
- Define how Desktop should reuse CLI launch/session machinery without silently
  changing ownership.
- Define JSON and receipt implications.
- Reserve future hook points for auth, context aggregation, payment,
  entitlement, Private Store, secrets, consent, and audit receipts.

### Out of scope

- Implementing `ato dev`.
- Hot reload, watch mode, or a new development server contract.
- Large runtime refactors.
- New auth, payment, entitlement, context, secret, or consent providers.
- Desktop UI redesign.

## 3. Command Semantics

### 3.1 `ato run`: ephemeral local production rehearsal

`ato run <target>` means:

> Resolve the target as if it were going to run under Ato's production launch
> contract, materialize only the state needed for this local session, execute it,
> and record what happened without registering a durable installed app.

`ato run` is not a project development mode. It may execute a target whose
declared command happens to be `npm run dev`, `uvicorn --reload`, or another
framework-specific command, but that is a property of the selected capsule
target. The command-level contract is still a local production rehearsal:
policy, materialization, identity, receipts, and cleanup are modeled as a run
session.

Required properties:

- A run session must not silently create an installed app.
- A run session may create session-local caches, tool/source/build
  materializations, logs, and receipts.
- Reusable materializations are allowed only when their lifetime and ownership
  are cache/session scoped or explicitly promoted by another command.
- `ato run` may launch Desktop as a presentation surface, but Desktop must
  preserve run ownership.

### 3.2 `ato install`: durable local app registration

`ato install <target>` means:

> Resolve the target into a durable local app record, bind it to an install
> profile, create or update an immutable install revision, and make future
> launches addressable through installed-app identity.

`ato install` is not just a prefetch for `ato run`. It records that the user has
chosen to keep the app locally under Ato's installed-app lifecycle.

Required properties:

- An install must create or update an installed app registry entry.
- An install must create or update an install profile.
- An install revision must be immutable and addressable independently from the
  profile that currently points at it.
- User profile state, consent decisions, secret references, OS integrations, and
  app state belong to the installed-app lifecycle, not to an ephemeral run
  session.

### 3.3 `ato dev`: deferred

`ato dev` is not defined here. Until a separate RFC accepts it, no help text,
README language, or Desktop surface should imply that `ato run` is the future
`ato dev` contract.

Future `ato dev` work should decide, at minimum:

- whether dev sessions have a separate identity namespace
- whether file watching and hot reload are owned by Ato or by the target command
- whether development state can observe or mutate installed-app profiles
- how development receipts differ from production rehearsal receipts

## 4. Identity Boundaries

### 4.1 Run session identity

A run session identity describes one ephemeral invocation and its session-owned
state. It may be backed by a UUID-style run id and session record under the
session/runs area of `ATO_HOME`.

It is not an install profile key and must not be reused as one.

Run session identity owns:

- session directory
- process/session record
- session-local logs
- session-local dependency/process state
- receipt references for the invocation

### 4.2 Install app/profile identity

Installed app/profile identity describes durable local ownership. Existing
implementation terms are:

- `InstalledAppId`: stable for the lifetime of the installed app
- `ProfileId`: stable profile name such as `default`
- `InstallProfileKey`: stable composite key used by shortcuts, dashboards, and
  installed-app launch paths

This identity survives app updates. It must not be minted by a plain `ato run`.

### 4.3 Install revision identity

Install revision identity describes an immutable frozen revision of an installed
app. Existing implementation terms are:

- `ArtifactBuildId`: content-addressed artifact build identity
- `InstallRevisionId`: immutable revision id derived from the artifact build

An install profile may point at different revisions over time, but a revision
itself must not be mutated in place.

### 4.4 Execution identity

Execution identity describes the resolved launch world. It is computed from
launch conditions such as source, recipe, lock, runtime, dependency outputs,
environment closure, filesystem view, network/capability policy, entrypoint,
argv, cwd, and state bindings.

Execution identity is shared conceptually by both `ato run` and install-owned
launches, but the parent identity differs:

- run-owned launch: execution belongs to a run session
- install-owned launch: execution belongs to an install profile and revision

For installed launches, `CapsuleInstanceKey` combines install profile key,
install revision id, and execution id. Plain `ato run` must not fabricate an
install profile key just to fit this shape.

## 5. State Boundaries

### 5.1 Cache, tool, source, and build materialization

`ato run` may use or create reusable materializations when they are explicitly
cache-scoped or session-scoped:

- resolved tools/runtimes
- fetched source checkouts
- dependency derivation caches
- build materialization records
- provider-backed synthetic workspaces

These artifacts are not installed-app registration. Reusing them must not imply
the user has kept the app.

`ato install` may reuse the same low-level materializations but must promote the
selected result into install-owned state: app record, profile, immutable
revision, source provenance, lock, and artifact manifest.

### 5.2 Session records, logs, and receipts

`ato run` owns session records, logs, and receipts for the invocation. These may
outlive the process for audit/debug retention, but they remain run/session
records.

Install-owned launches also emit session records, logs, and receipts. Those
records must carry enough installed-app identity to answer:

- which install profile launched this?
- which immutable revision launched this?
- which execution identity was observed?

### 5.3 Installed app registry

Only install/update/promote flows may write durable installed-app registry
entries. A plain `ato run` may suggest `ato install` as a next step, but must not
silently write:

- `app.json`
- `profile.json`
- `current_revision`
- OS launcher integrations
- install-owned state directories

### 5.4 Profiles, state, consent, and secrets

Install profiles are durable user choices. Profile data may include env refs,
secret refs, port policy, concurrency policy, isolation preference, and future
context/payment/auth bindings.

Secret values must not be copied into install profiles or receipts. Profiles
store references only.

Consent decisions must be scoped to the identity they approve. A consent granted
for a run-owned launch does not automatically approve all installed launches of
the same source. A consent granted for an install profile/revision does not
automatically approve unrelated run-owned launches.

Persistent app state belongs to installed-app lifecycle. Ephemeral run state
belongs to the run session unless the user explicitly promotes or binds it.

## 6. Desktop Relationship

Desktop is a presentation and control surface over Ato launch/session machinery.
It is not a separate execution engine.

Desktop launch should reuse the same launch/session machinery as the CLI where
possible. The ownership mode must remain explicit:

- install-owned Desktop launch may use installed app/profile identity
- run-owned Desktop launch must preserve run session identity
- run-owned Desktop launch must not silently create an installed app
- Desktop may offer an explicit "install/keep" action that changes ownership

Closing a Desktop window is not equivalent to uninstalling an app and is not
equivalent to deleting a run receipt. Window lifecycle and process/session
lifecycle remain separate concepts.

## 7. JSON and Receipt Implications

Machine-readable output should expose ownership explicitly.

Recommended fields for launch-like JSON envelopes:

```json
{
  "ownership": "run-session",
  "run_session_id": "run_...",
  "installed_app_id": null,
  "install_profile_key": null,
  "install_revision_id": null,
  "execution_id": "exec_...",
  "receipt_path": "..."
}
```

For install-owned launch:

```json
{
  "ownership": "installed-app",
  "run_session_id": null,
  "installed_app_id": "app_...",
  "install_profile_key": "ipk_...",
  "install_revision_id": "rev_...",
  "execution_id": "exec_...",
  "capsule_instance_key": "cik_...",
  "receipt_path": "..."
}
```

Receipts should identify:

- command boundary (`ato run`, `ato install`, `ato launch`, Desktop launch)
- ownership class (`run-session` or `installed-app`)
- run session id when run-owned
- installed app/profile/revision ids when install-owned
- execution identity
- consent and policy hashes
- secret references without secret values

Receipts must not make a run-owned launch look like an installed-app launch.

## 8. Future Hook Points

The boundary in this RFC is intended to support later features without special
cases.

### Auth

Auth/session tokens should bind to the ownership model. A one-off `ato run` may
use a temporary auth context; an installed app/profile may persist a stable auth
binding or refresh policy.

### Context aggregation

Context providers may contribute user, workspace, organization, or device
context. Run-owned launches should receive only context approved for that run.
Install-owned launches may use profile-scoped context bindings.

### Payment and entitlement

Payment or entitlement checks should attach to the installed-app/profile/revision
identity when the user keeps an app. A run-owned trial/rehearsal may have a
different entitlement boundary.

### Private Store

Private Store installs should use the same installed-app identity model as public
Store installs. Visibility, organization membership, entitlement, and audit
metadata are additional policy inputs; they must not create a separate install
mental model.

### Secrets

Secret providers should resolve secret references at launch time. `ato install`
may store durable secret refs in a profile; `ato run` may use ephemeral refs,
env-file refs, prompt values, or session-scoped secret handles.

### Consent

Consent records should include the ownership class and relevant identity keys so
that approvals cannot drift between run-owned and install-owned launches.

### Audit receipts

Audit receipts should be able to reconstruct whether an action was a rehearsal,
an install, an install-owned launch, an update, a rollback, or a promotion.

## 9. Decision Pending

### 9.1 Should `ato install` immediately launch?

Decision pending.

Options:

- install only, then print an explicit `ato launch <install_profile_key>` next
  step
- install and prompt to launch in TTY contexts
- install and launch by default unless `--no-launch`

This must not be fixed by this RFC. The answer affects Desktop and Store UX,
payment/entitlement timing, consent timing, and receipt boundaries.

### 9.2 Should promotion from `ato run` to `ato install` preserve run state?

Decision pending.

The safe default is no implicit state promotion. If a future "keep this app"
flow preserves state, it must show what moves from session-owned state into
installed-app state and emit a receipt for that promotion.

## 10. Acceptance

- This RFC lands without a large runtime refactor.
- README/help text no longer implies `ato run` is just a dev-server shortcut.
- `ato dev` remains explicitly out of scope.
- `ato run` is documented as ephemeral local production rehearsal.
- `ato install` is documented as durable local app registration.

## 11. Conformance Snapshot (as of #335 + run/install guardrails)

This snapshot captures the observable shapes that distinguish run-owned and
install-owned state under `ATO_HOME` at the time these guardrails were added.
It is meant to be read alongside §5 (state boundaries) and §7 (JSON/receipt
implications), and to be kept honest by
[`crates/ato-cli/tests/run_install_semantics.rs`][guardrails].

### 11.1 File shapes that mark install-owned state

A regression check that wants to assert "this command did not install
anything" looks for any of the following under `ATO_HOME`:

- `instances/<installed_app_id>/app.json` — installed-app registry entry
- `instances/<installed_app_id>/profiles/<profile_id>/profile.json` — install profile
- `instances/<installed_app_id>/profiles/<profile_id>/current_revision` — profile revision pointer
- `revisions/<install_revision_id>/…` — immutable revision tree

Paths are produced by
[`capsule::foundation::install_lifecycle::store::InstallInstanceStore`][store].
The exact directory names may move in a later layout refactor; the file-shape
predicates in the guardrail test are deliberately structural rather than
hard-coded to a single path.

### 11.2 Run-owned state that `ato run` may create

A run-owned launch may write under:

- `runs/`, `run-sessions/` — CLI-owned session sidecars and ephemeral state
- `apps/ato-desktop/sessions/` — Desktop direct-read session records
- `cache/`, `toolchains/`, `runtimes/`, `engines/`, `store/`, `projections/` — caches
- `executions/` — execution receipts

These paths must never imply the user has kept the app.

### 11.3 Session record discriminator

`ato_session_core::record::StoredSessionInfo` already carries the
discriminator described in §7. Until a normative helper crystallizes,
treat the following predicate as the canonical run-vs-install check on a
session record:

```rust
fn is_run_owned(record: &StoredSessionInfo) -> bool {
    record.installed_app_id.is_none()
        && record.install_profile_id.is_none()
        && record.install_profile_key.is_none()
        && record.install_revision_id.is_none()
}
```

`capsule_instance_key` is derived from the four identifiers above plus
`execution_id` and so cannot meaningfully be set on a run-owned record.

A regression that promotes any of the four primary identifiers to
non-`Option`, or to a non-`null` default on the wire, would silently make
every `ato run` session look install-owned to receipt consumers and to the
Desktop fast path. The guardrail test pins both shapes — run-owned and
install-owned — against this predicate.

### 11.4 Out of scope for the guardrails

These remain documented but unverified by the current guardrails:

- OS shortcut records (no normative storage layout yet)
- Update channel records (deferred until update flow lands)
- "App card" surfaces in Desktop (UI-level, covered by Desktop tests)

[guardrails]: ../../../crates/ato-cli/tests/run_install_semantics.rs
[store]: ../../../crates/capsule/src/foundation/install_lifecycle/store.rs

## References

- `docs/rfcs/accepted/ATO_CLI_SPEC.md`
- `docs/rfcs/draft/ATO_HOME_LAYOUT.md`
- `docs/rfcs/draft/APP_SESSION_MATERIALIZATION.md`
- `docs/rfcs/draft/BUILD_MATERIALIZATION.md`
- `docs/execution-identity.md`
- `crates/capsule/src/foundation/install_lifecycle/ids.rs`
- `crates/capsule/src/foundation/install_lifecycle/store.rs`
- `crates/ato-cli/tests/run_install_semantics.rs`
