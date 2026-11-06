# Portable-application execution pipeline

Status: Current technical reference.

This page describes the portable-application execution path as implemented, not
an intended design. It was derived from the source at commit `b0b2219` by
following `ato pack`, `ato run`, `ato app` and the shared attempt layer.
Where documentation and code differ, this page follows the code and names the
difference.

Scope: portable applications (wire v3 `ato.portable-application/1` and v4
`ato.portable-application/2`) on the local path. The computation-root wire v2
lifecycle (`init` / `resume` / `stop` / `encap`) uses different commands, a
different root (`ComputationRef`), and is not covered here.

## 1. Artifacts and identity

A `.capsule` file is a JSON document with an `index`, `payloads`, an optional
`portability` manifest and optional `signatures`
(`lib/objects/src/portable_bundle.rs`). The reader dispatches on
`index.version`:

| Input | Root | Profile |
|---|---|---|
| wire v2 | `ComputationRef`, `index.root` | computation-root |
| wire v3 | `root_contract_ref` | `ato.portable-application/1` |
| wire v4 | `root_contract_ref` | `ato.portable-application/2` |

An unknown version is refused, not reinterpreted. The SHA-256 of the whole file
is transport equality only. For the portable profiles the Capsule identity is
`ContractRef`: the SHA-256 of the JCS-canonical `ato.contract/1` object. A
`ContractRef` and a `ComputationRef` are different domains and are never cast
into each other.

## 2. Authoring to canonical K and D

`ato pack <source> -o app.capsule` calls `build_authored_bundle_v2`, which parses
`ato.capsule/2` (`lib/formation/src/capsule_toml_v2.rs`), binds the authoring
draft with `ato_formation::authoring::bind`, and writes the canonical objects:

- `BoundContract` is `{ schema: "ato.contract/1", requirements }`. Requirements
  are sorted by id; a Contract is a set of conditions, not a transcript.
- `BoundDerivation` is `{ schema: "ato.derivation/1", inputs, runtimes, steps,
  ports, state, effects, ... }`. Steps keep authored order because a route is
  a sequence.
- `contract_ref` is the SHA-256 of `serde_jcs::to_vec(&contract)`;
  `derivation_ref` is the same digest over the Derivation. Neither includes the
  source tree unless the Contract explicitly carries a `workspace@1`
  requirement.

The authoring grammar's `identity` field is authoring metadata: it is checked
during binding and is not part of K's canonical bytes. `ato.capsule/2` accepts
`behavioral` and refuses `source-bound`; a source-bound K is authored with the
`ato.capsule/1` grammar, which can add a workspace identity observation.

## 3. Bundle validation

`decode_portable_application_bundle` performs four checks before anything is
realized:

1. Size limits: 768 MiB per bundle, 64 MiB per object, 256 MiB decoded,
   10,000 objects.
2. Shape and payloads, with `deny_unknown_fields` on every structure; v3 must
   be `ato.portable-application/1` and v4 must be `ato.portable-application/2`.
3. Canonical transport: re-serializing the decoded document as JCS must equal
   the input bytes exactly. A pretty-printed or reordered bundle is rejected.
4. Object closure: every descriptor must be reachable from the roots
   (`root_contract_ref`, `application_ref`, `derivations`, optional
   `instance_snapshot_ref`); duplicate roots, missing objects, non-sha256
   references and unknown schemas fail closed.

`validate_bundle_for_derivation` then checks the selected D against the bundle:

- the selected reference must be declared;
- `root_contract_ref` must equal the canonical Contract's digest and the
  selected D its canonical digest;
- the initial route profile (one surface, one workspace input, one serving
  step, one HTTP port for the single-step case; a service group is the
  multi-step exception) must match the Application surface;
- the single-step initial profile forbids a build, more than one state slot and
  non-pure effects (the service-group case has its own shape checks);
- static routes require an `ato.application/1` surface; process and OCI routes
  require `ato.application/2`; OCI requires a digest-pinned image, platform and
  resource limits;
- a v4 Instance snapshot must be the exact indexed object, exactly one
  Contract requirement, and its filesystem resource must match the selected
  Derivation state slot.

No entry re-resolves a sealed Derivation's runtimes. The execution-time runtime
policy is checked here as well, so a bundle pinning an affected runtime is
refused at any entry.

## 4. Admission

`ato run` selects a Derivation before admission: `--derivation` for a
multi-D bundle; a single declared D is selected automatically. The caller then
calls the shared `run_attempt`, whose first step is admission
(`lib/runtime-attempt/src/admission.rs`):

- native-surface routes are refused unless a capable native Runner admits them;
- a host-condition requirement is refused on this runtime;
- a Model Set input is refused when the runtime has no data plane to deliver it;
- the authorization variant must match the selected Derivation; exploration and
  functional-verification grants carry their own ceiling;
- only `pure`, `idempotent` and `record-substitutable` effect classes are
  executed. `requires-confirmation` and `non-repeatable` are refused for every
  authorization variant in this build;
- declared platform must include the current OS/arch;
- the runtime policy (for example an affected Node version) is re-checked;
- a browser Contract requires a process candidate and a contained browser
  verifier.

Admission failure is not an execution attempt: no start record is written and
no workload runs.

## 5. Attempt: record, realize, observe, verify

After admission the attempt layer writes a durable start record before the
first effect (staging, build, dependency fetch, launch). A start that cannot be
recorded prevents execution. A later end that cannot be recorded leaves the
attempt as `StartedUnfinished`; the next reader must treat it as UNKNOWN.

The realizer for a portable bundle is `PortableBundleExecutor`
(`apps/portable-application/src/portable_run.rs`), which realizes:

- static web from the sealed tree;
- a process route in the sandbox;
- an OCI container or service group under Runner-owned limits.

Preparation is a Launch phase, separate from observation: a declared startup
probe runs first, then the fixed K observations. HTTP observations use no
redirects, treat any received status as evidence, wait at most 60 s for the
first answer, 10 s per request, and do not hash bodies over 8 MiB. TLS
handshakes get 10 s each and are read from the same still-running candidate.
Scenarios run after the read-only GETs and before the verification point; v1
runs them only on a fresh instance (no state slot, no Instance snapshot),
otherwise the scenario observation is a failure rather than a silent skip.

The verification point is `verify_observed_candidate`
(`lib/runtime-attempt/src/verification.rs`): it compares the observed inputs,
HTTP responses, TLS observations and scenario transcripts with the frozen K.
`fully_satisfied` requires every requirement to be `Satisfied`; the runtime
path has no deferred verdict. A failed or undecided observation is an attempt
failure, not a weaker K.

## 6. Receipts

The receipt is created at the verification point and names K, the selected D,
the target, observations with evidence, `fully_satisfied`, and the execution
evidence (realization, runtime, pid/container, endpoint, attempt and Run).
`bundle_sha256` is optional because a Formation attempt before packaging has no
bundle to hash. The receipt is evidence of that attempt; it is not identity.

The schema has three versions in the current tree:

| Schema | Added evidence |
|---|---|
| `ato.contract-verification-receipt/1` | base |
| `ato.contract-verification-receipt/2` | portability profile |
| `ato.contract-verification-receipt/3` | TLS evidence |

The CLI writes a receipt to `--verification-receipt` only for an accepted Run.
A failed Run's receipt remains attempt evidence; it is not published as the
caller's receipt. This is deliberate: a refused Run must not look published.

## 7. Continuation: Run, Instance, Formation

- **`ato run`** is ephemeral. `--no-open` and `--verification-receipt` are
  accepted only for portable v3/v4; the v2 computation path refuses them. On a
  verified Run the candidate is handed off and kept running until Ctrl-C or
  signal; on a failed Run it is stopped. A service group is one Application:
  if any service exits, the Run is reported stopped.
- **`ato app import` / `start`** create a durable local Instance. Import
  accepts only portable v3/v4, validates the selected D, stores the bundle by
  its transport digest and records `contract_ref`, `application_ref`,
  `selected_derivation_ref` and any `data_snapshot_ref`. Start spawns a worker
  with a Run id and token, delivers Bindings on stdin, requires the worker to
  become active within 60 s, and exposes the Run's receipt. A restored snapshot
  is verified against its content references before use. Stopping records the
  stop result; an unconfirmed stop is retained for recovery, not reported as
  clean.
- **`ato form`** is the source-candidate workflow. It uses the same attempt and
  verification authority but stops the candidate after verification. The
  deterministic path is the default; decision and proposal providers, when
  enabled by explicit policy and budget, can only choose or propose, never
  execute or alter a frozen K.

## 8. Sandbox

Process execution uses the in-tree `ato-sandbox` crate (Landlock on Linux
5.13+, Seatbelt on macOS). On Linux the launch path builds a Bubblewrap argv
with a read-only system bind set, a read-only workspace and a write scope, then
re-enters the Runner binary as a sandbox shim so Landlock is applied by the
process that execs the workload. The portable process path uses the same
mechanism through `__portable-sandbox-exec`. Development mode skips
enforcement; it is not a security boundary and this is not a container or VM.

## 9. Invariants that the code enforces

1. K is frozen before an attempt; a Run does not recapture or rewrite it.
2. `ContractRef` is identity for the portable profile; `.capsule` bytes are
   transport; `ComputationRef` is not interchangeable with either.
3. Acceptance is all-or-nothing: every required observation must be satisfied
   at the verification point.
4. Every attempt records its own evidence; a receipt from one attempt is not
   evidence for another.
5. Only the deterministic verifier decides K; a model or a build result cannot.
6. Only the disposable effect classes execute; a non-repeatable or
   confirmation-required route is refused.
7. Validation, admission, execution and verification are separate failures;
   one is not reported as another.
8. Unknown versions, profiles, fields, schemas or references fail closed.
9. Selection of a Derivation is explicit for a multi-D bundle; there is no
   implicit fallback.
10. Sandboxing and placement are realization concerns, not identity.

## 10. Known limits of this path

- The initial portable route profile is narrow: one surface, one workspace
  input, one serving step and one HTTP port for the single-step case; at most
  one state slot; no build; pure effects.
- Scenario v1 runs only on a fresh instance.
- Browser verification is limited to process candidates with a contained
  verifier.
- Local Run receipts are published only on acceptance; a failed attempt's
  receipt is attempt evidence.
- Instance snapshot restore is bounded to declared resources and assets;
  general cross-host state restore and a proven Hosted saved-data round-trip
  are not established here.
- The computation-root v2 lifecycle is a different path with different
  commands and a different root.

## 11. Where documentation previously differed

This page is the current reference for these points:

- The receipt schema is not only `/1`; `/2` adds the portability profile and
  `/3` adds TLS evidence.
- A seal decision may accept deferred checks, but a runtime acceptance has no
  deferred verdict: `fully_satisfied` requires every observation satisfied.
- `ato run` accepts a single declared D without `--derivation`; it requires the
  flag only when the bundle declares several.
- `--no-open` and `--verification-receipt` are portable v3/v4 features; the v2
  computation path refuses both.
- The current sandbox is `ato-sandbox`; Nacelle is donor code and is excluded
  from the workspace.

## Implementation map

| Concern | Source |
|---|---|
| Wire decode and transport validation | `lib/objects/src/portable_bundle.rs` |
| Canonical K/D and binding rules | `lib/formation/src/authoring.rs`, `lib/formation/src/capsule_toml_v2.rs` |
| Bundle and route validation | `apps/portable-application/src/lib.rs` |
| Portable realization | `apps/portable-application/src/portable_run.rs` |
| Admission and in-flight policy | `lib/runtime-attempt/src/admission.rs` |
| Attempt lifecycle | `lib/runtime-attempt/src/attempt.rs` |
| Verification and receipt | `lib/runtime-attempt/src/verification.rs`, `lib/formation/src/verify.rs` |
| Canonical receipt evidence | `lib/formation/src/receipt.rs` |
| Local Instance | `apps/portable-application/src/local_instance.rs` |
| Sandbox | `lib/sandbox/src/lib.rs`, `lib/runtime-attempt/src/launch/sandbox.rs` |
| CLI commands | `apps/cli/src/lib.rs` |

See also [Formation, verification and receipt](../concepts/formation-verification.md),
[Runner and hosting](../guides/runner-hosting.md) and the
[documentation index](../README.md).
