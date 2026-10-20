# Desktop local preparation and native process proposals

Status: draft; implementation scoped to the Apple Silicon macOS acceptance.

## Contract and policy

The user explicitly requests a local copy with `no_vm = true` and
`offline_after_save = true`. These are orchestration/admission constraints,
not semantic roots or fields added to K. The original portable bundle is frozen
by transport SHA-256. Existing declared eligible D are preferred; several
eligible D require an explicit selection. No OCI, VM, remote execution or
silent fallback is permitted by this preparation profile.

A proposal must retain the exact ContractRef, logical Surface and declared
filesystem state boundary. The shared Rust portable-application authority owns
schema/digest/closure validation and eligibility. Actual local import/start
still owns materialization, sandbox admission, execution and Run-scoped K
verification. A preparation assessment is not a successful Run receipt.

This first native route supports one arm64 Mach-O executable in an immutable
workspace, with explicit `native.os`, `native.arch`, `native.executable` and
`native.encoding` runtime declarations. `identity` and bounded `gzip` encoding
are supported; the latter expands at most 128 MiB. Scripts and foreign images
are refused. The Runner materializes the digest-verified image and uses the
existing process OS sandbox, declared HTTP Port and filesystem state remapping.
No shell recipe, Docker socket, VM launcher or provider credential is passed
to the app. Runtime placement differs from source build tooling.

External Bindings and network grants are refused by this first offline profile.
Pinned Python runtimes and dependencies are retained by the existing local
Instance store after successful preparation. Offline behavior must additionally
be exercised with external networking disabled: K only proves its declared
observations, not every user interaction or arbitrary application behavior.

## Producer boundary

`ato app prepare INPUT --no-vm --offline-after-save --output VALIDATED`
reports ready, needs_selection or needs_proposal. `--derivation` is explicit;
`--candidate` validates a proposed bundle against the original input.
`ato app form-native INPUT --workspace DIR --proposal FILE --output CANDIDATE`
compiles a strict `ato.native-process-proposal/1` (executable, encoding, argv,
cwd, env, target_os and target_arch) through the existing common builder.
Unsupported original observations are refused, never weakened or recaptured.

Desktop persists a private `preparations/<random-id>/request.json` with schema
`ato.local-formation-request/1`, immutable input digest, constraints, host target,
source revision URL and the originating cloud Instance ID. A producer reads it,
builds explicit inputs, and writes `candidate.capsule`, followed atomically by
`proposal.json` (`ato.local-formation-proposal/1`, request_id,
input_bundle_sha256, candidate_bundle_sha256, producer). Unknown fields,
misbound digests, wrong target and cancelled requests are refused. The native
bridge copies the submitted candidate before validation; only validated bytes
and the admitted declared D proceed to the normal local import endpoint.

There is no embedded LLM or model credential. In the initial acceptance the
interactive coding agent performs reasoning and source build work. Future
Codex, Claude Code or DeepSeek harnesses may consume this same request/proposal
boundary; they cannot assert Contract satisfaction or bypass Rust admission.
An unattended producer, its permission UI and lifecycle supervision are future
work. Cancelling a pending request fences later submissions; an import that has
already committed retains the existing Instance lifecycle semantics.

## Acceptance

- Notes reuses its declared pinned Python D without VM execution.
- Memos forms a new native D from an immutable upstream source revision while
  preserving the original HTTP Contract and filesystem state mount.
- Both execute through Desktop, perform actual data writes, stop, cold-start
  with external networking disabled, retain data, and emit fully satisfied
  Run-scoped receipts. Operating system reboot and other macOS versions are
  recorded separately if untested.
