# Portable Native Derivations (macOS / Windows)

Status: Draft (discussion outcome, 2026-11-09; no implementation yet)
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
   present, the declared signature policy must verify, and the containment
   features the platform can offer must be available. An unavailable platform
   must not invalidate another valid D.
3. **Execution** materializes the workspace input read-only, launches the
   declared executable with explicit user confirmation, establishes a logical
   Port, and owns cleanup on stop, timeout, and crash.
4. **Verification** observes artifact integrity (digest comparison) and
   launch/process evidence for the Run attempt. In this first increment a
   native D has **no HTTP Contract observations**: the UI experience is not
   part of K and is not verified. A fully-satisfied native receipt therefore
   means "the artifact is intact and it launched", not "the UX works".
   If a native route later authors real observations (HTTP loopback or
   another observation protocol), that is a new descriptor contract, never a
   silent weakening of this rule.

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

## Isolation (stated honestly)

A native GUI application needs window/display interaction, so the process and
OCI containment guarantees do not carry over wholesale. In this increment the
enforceable claims are: read-only workspace input, Run-scoped writable state,
declared egress policy, and process cleanup. Stronger containment (macOS
sandbox, Windows AppContainer) is desirable but presupposes ongoing OS policy
work; until then, the receipt must record what was and was not enforced. A
Hosted Runner that cannot enforce a native D's declared policy must reject
admission for that D.

## Driver use case — PSP player

One K, three D routes:

- static-web D — wasm emulator core served from the profile's static surface
  (experimental; feasibility evidence expected before this increment relies
  on it),
- native-macos D — Tauri-packaged player,
- native-windows D — Tauri-packaged player.

ROM and BIOS files are user-supplied Bindings rebound at Run time; they are
never part of K, of the bundle, or of the hub ciphertext. Save state is
Instance state, independent of D (K/D invariant 3), so progress follows the
user across D switches. The capsule never bundles copyrighted game assets.

## Acceptance gate (proposal)

The PSP player with all three D routes serves as the fixture once the wasm
route has feasibility evidence. Interim gate before that: one small native
capsule (a static-web game with an added Tauri native D) with:

- byte-identical bundle consumed by CLI on macOS and Windows,
- explicit D selection for each run,
- `fully_satisfied = true` receipts for the static-web D (HTTP observations)
  and the native D (integrity + launch evidence),
- receipt naming the selected D in every case.

## Non-goals

- No GPU/graphics protocol semantics and no game-session semantic noun;
  games are ordinary portable applications.
- No marketplace, hub, or discovery integration.
- No silent cross-D fallback, no D auto-selection, no runtime download of
  native artifacts in the first increment.

## Undecided (needs review)

- Exact descriptor fields and whether one `ato.native@1` kind with a platform
  field is preferable to per-platform kinds.
- Signature policy default (`require-valid` vs `verify-and-record`).
- Universal binary vs per-arch artifacts for macOS; Windows ARM.
- Per-D artifact size caps and their interaction with the capsulehub object
  size and with bundle payload transport.
- Whether native D may declare a loopback HTTP Port for observation reuse in
  a later increment.
- Background lifetime for a native launch: today the Run owns cleanup, so a
  native process must not outlive it; a detached/background mode is a future
  increment if ever wanted.
