# Portable Native Derivations (macOS / Windows)

Status: Draft (reviewed 2026-11-09; observation/admission/save rules revised
on review; no implementation yet)
Owner: Portable application runtime
Related: [K_D_MODEL](K_D_MODEL.md),
[PORTABLE_DYNAMIC_DERIVATIONS](PORTABLE_DYNAMIC_DERIVATIONS.md),
[PORTABLE_APPLICATION_BUNDLE_V3](PORTABLE_APPLICATION_BUNDLE_V3.md),
[TAURI_DESKTOP_MIGRATION](TAURI_DESKTOP_MIGRATION.md), CAPSULEHUB_E2EE_COORDINATOR.md

## Purpose

Extend `ato.portable-application/1` with native desktop routes so that one
immutable identity K can offer a browser route and native macOS/Windows
routes as alternative Derivations. This is the adding-K-and-D request from the
2026-11-09 discussion: game capsules (static web now, emulator player as the
flagship) that can also be built as native applications for macOS and Windows.

This adds D kinds only. It changes no hash, no identifier, no public Contract
meaning, and introduces no new semantic root: a native route is an Adapter-
level realization concern, same as the Python process and OCI routes.

## Proposed Derivation kinds (first increment)

One kind per platform keeps admission criteria legible; a shared kind with a
platform field is acceptable if the descriptor review prefers it:

- `ato.native-macos@1` — a macOS executable artifact (.app bundle or single
  signed binary), declared with artifact digest, architecture, executable
  path, and signature policy.
- `ato.native-windows@1` — a Windows executable artifact declared the same
  way (Authenticode signature policy).

Artifacts travel as bundle payload blobs (digest-covered by the existing
closure verification). Larger artifacts reference out-of-band pinned bytes
only if payload transport proves insufficient; that is an escalation, not a
default.

## Packaging guidance (author-facing)

- Tauri is the recommended first packaging tool: small bundles, and the same
  WebView lineage as the ato desktop shell. Electron is valid but heavy.
- The runtime MUST NOT special-case Tauri, Electron, or any other packaging
  tool. A native D is any digest-pinned executable artifact; author tooling
  produces it, the runtime only verifies and launches it.
- Per-capability guidance (window vs loopback Web Surface lifecycle for the
  app) reuses the TAURI_DESKTOP_MIGRATION security table as authoring advice,
  not as a runtime contract.

## Four boundaries (same shape as PORTABLE_DYNAMIC_DERIVATIONS)

1. **Bundle validation** proves descriptor schema, canonical digests, payload
   integrity, reference closure, and declared Derivation membership for
   **every** D before realization.
2. **Runtime admission** applies only to the explicitly selected D: platform
   and architecture must match the host, the pinned artifact digest must be
   present, the declared signature policy must verify, and the runtime must
   be able to fully enforce the D's declared mandatory policies — an
   environment that cannot enforce them fails admission. Optional extra
   defenses are not required by admission. An unavailable platform must not
   invalidate another valid D.
3. **Execution** materializes the workspace input read-only, launches the
   declared executable with explicit user confirmation, establishes a logical
   Port, and owns cleanup on stop, timeout, and crash.
4. **Verification** observes the root K's full requirement set through the
   same common verifier as every other D. Selecting a native D removes
   nothing from K: if K requires HTTP observations, the native route must
   expose the loopback HTTP Port(s) those requirements name and the runtime
   must issue the same observations against them. Artifact-integrity and
   launch/process evidence are recorded as execution evidence, but a
   `surface_ready` or "the artifact is intact and it launched" result can
   never set `fully_satisfied=true` while any K requirement is unobserved or
   unsatisfied. A native receipt therefore reports the same meaning as any
   other receipt: the same ContractRef, the same requirement set, per-
   requirement verdicts — plus the extra execution evidence. Additional UX
   verification for a native route that leaves K untouched is an Additional
   feature Contract / Verification Profile over the same Release; turning an
   additional condition into a required condition of Capsule validity is a
   new Identity K — new Contract, new CapsuleId, new Release — following the
   K/D model's critical rule. Neither is a silent weakening of the existing
   Contract.

## Signatures and the QUARANTINE boundary (macOS)

Downloaded binaries carry the Gatekeeper quarantine requirement. Policy:

- requested admission minimum: a valid Developer ID signature verified before
  launch, with the verification verdict persisted as receipt evidence;
- unsigned or ad-hoc-signed artifacts may run only through an explicit local
  override that is recorded as evidence in the receipt (never silently);
- notarization and certificate distribution are author operations, out of
  scope of this model. Windows SmartScreen shows the equivalent warnings for
  Authenticode-unsigned artifacts.

## Safe by default

The native route is the highest-risk realization path in the profile, so:

- D selection stays explicit and recorded; CLI UX prompts when more than one
  D is declared, and never silently substitutes a D (web for native or the
  reverse, as is already required for Python/OCI routes),
- no implicit planner or capability inference is added,
- containment claims in the receipt state exactly what the OS enforced,
  rather than a blanket "sandboxed" label.

## Mandatory admission policies (local and hosted alike)

Mandatory policy for a native D — read-only workspace input, Run-scoped
writable state, declared egress and lifecycle policy — is enforced **before
launch** on every execution path. The current CLI already fails local
admission when sandbox enforcement is not fully applied; the native route
inherits exactly that rule. A local or Hosted environment that cannot apply a
declared mandatory policy must refuse to launch that D: recording "was not
enforced" in the receipt is evidence of a gap, never satisfaction of a
mandatory condition.

The receipt states which mandatory conditions were enforced and, separately,
which optional additional defenses (for example OS-level containment listed
below) were present or absent. An unsigned-artifact override is an evidence
recording device only: it may record that the signature policy verdict was
"unsigned, user accepted"; it does not relax filesystem, state, egress, or
lifecycle policy.

## Isolation boundary for GUI surfaces (stated honestly)

A native GUI application needs window/display interaction, so the process and
OCI containment guarantees do not carry over wholesale. Window/display-level
containment is therefore **not** in the mandatory set of this increment; the
receipt must label those aspects "not enforced" rather than implying
sandboxing. Stronger OS containment (macOS sandbox profiles, Windows
AppContainer) is an optional defense when available and is recorded when
applied. What stays mandatory and pre-launch-enforced everywhere: the
filesystem, state, egress, and lifecycle policies above.

## Driver use case — PSP player

One K, three D routes:

- static-web D — wasm emulator core served from the profile's static surface
  (experimental; feasibility evidence expected before this increment relies
  on it),
- native-macos D — Tauri-packaged player,
- native-windows D — Tauri-packaged player.

ROM and BIOS files are user-supplied Bindings rebound at Run time; they are
never part of K, of the bundle, or of the hub ciphertext. The capsule never
bundles copyrighted game assets. Save-state placement is Instance state,
independent of D (K/D invariant 3) — but D-independence of the state
*location* is not interoperability of the state *format*; see the next
section.

## Save continuity across D

Instance state that one D writes and another D restores must actually be
readable by both. That requires, per capsule:

1. a common save schema with a declared version, shared by every D that
   declares the state slot,
2. an explicit mapping from each state slot to its storage location and
   protocol, so that "which saved resource belongs to which slot" is declared,
   not inferred, and
3. declared behavior on incompatibility: refuse the D switch with a named
   reason, or perform an explicit, user-confirmed migration — never a silent
   partial restore.

Progress tracking may therefore not over-claim: a native route that reads a
file format the web route does not write (or vice versa, e.g. localStorage
versus native files) has produced two isolated Stores, not one continuous
Instance. Acceptance for a capsule that claims cross-D progress requires a
round trip: save under one D → restore and update under another D → restore
the original again. The existing filesystem restore path is exact — it
restores resources whose slot and protocol match the declared state; it does
not translate formats — which is the floor this section builds on.

## Acceptance gate (proposal)

The PSP player with all three D routes serves as the fixture once the wasm
route has feasibility evidence. Interim gate before that: one small native
capsule (a static-web game with an added Tauri native D) with:

- byte-identical bundle consumed by CLI on macOS and Windows,
- explicit D selection for each run,
- identical verification meaning for both Ds: the **same ContractRef, the
  same requirement set, every observation Satisfied**. The native D exposes
  the loopback HTTP Port(s) the K requirements name and the common verifier
  runs the same observations; artifact integrity and launch/process evidence
  are recorded additionally and satisfy nothing by themselves,
- mandatory filesystem/state/egress/lifecycle policies enforced before
  launch on both routes, with admission failure when enforcement is
  incomplete,
- a cross-D save round trip (save on web D → restore and update on native D
  → restore on web D again) when the capsule declares save state,
- receipt naming the selected D in every case.

## Non-goals

- No GPU/graphics protocol semantics and no game-session semantic noun;
  games are ordinary portable applications.
- No marketplace, hub, or discovery integration.
- No silent cross-D fallback, no D auto-selection, no runtime download of
  native artifacts in the first increment.
- No detached/background native runtime: the Run owns cleanup, so a native
  process never outlives it.

## Undecided (needs review)

- Exact descriptor fields and whether one `ato.native@1` kind with a platform
  field is preferable to per-platform kinds.
- Signature policy default (`require-valid` vs `verify-and-record`).
- Universal binary vs per-arch artifacts for macOS; Windows ARM.
- Per-D artifact size caps and their interaction with the capsulehub object
  size and with bundle payload transport.
- The exact descriptor field for the loopback HTTP Ports a native route must
  expose whenever K requirements name HTTP observations.
