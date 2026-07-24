---
title: "ADR-014: Capsule Program Identity — an additive declaration identity"
status: accepted       # draft | accepted | archived
date: 2026-07-24
author: "@egamikohsuke"
related:
  - "CAPSULE_V1_EXECUTION_MODEL_SPEC.md"
  - "../draft/CAPSULE_CORE_MODEL.md"
  - "../draft/ADR-012-capsule-lifecycle-column.md"
---

# ADR-014: Capsule Program Identity — an additive declaration identity

> "Target-independent" is deliberately absent from the title: the identity is
> target-**selection**-independent but target-**definition**-dependent (§0),
> and the shorter phrase misreads as "target definitions are not hashed."

**Tracking issue:** none yet — this ADR is the plan of record; open an issue
before implementation starts.

> **Revision history**
>
> **r2:** dropped slicing the identity out of the resolved, target-specific
> `ExecutionContractV1`; adopted the lock's D2/D4 identity/signature split;
> re-based all references on `origin/nightly`.
>
> **r3:** independent IR + adapter instead of a manifest denylist projection;
> dedicated source projection closing the `capsule.toml`/`ato.lock.json`
> self-reference; parent link mandatory when both envelopes exist; three
> lock-trust chokepoints; canonical vectors in Phase 0.
>
> **r4:** complete 41-field top-level classification; strict
> `ProgramManifestV03Input`; exact two-path source exclusion with
> A1-before-filter ordering; pinned-materialization-only input; association
> claim vs. derivation proof; `ProgramRelativePath`; three fixture suites.
>
> **r5 (2026-07-24):** fixes the remaining *semantics*, not
> mechanics. (1) The normative definition is corrected to what the preimage
> actually identifies: a **canonical, immutable Capsule declaration** — not a
> "program revision." The preimage is target-selection-independent but
> target-definition-dependent, and it hashes dependency *constraints*, not
> resolutions — so the same id can legitimately resolve to different concrete
> programs over time. Calling that a "sole exact identity of an immutable
> program revision" overclaimed; the claim is now scoped honestly, and the
> earlier working conclusion that execution requirements/target definitions
> sit outside a "revision" is **explicitly overturned for v1** (recorded as a
> normative decision, with the split-identity alternative documented as
> deferred). (2) Nested preimage boundary closed: non-identity nested
> exceptions and unsupported nested fields are now enumerated normatively,
> and local/out-of-tree paths are never silently hashed (`engine_path`
> present ⇒ fail closed; `model` must be in-tree relative). (3) The nested
> `capsule.toml` content-sniffing rule is replaced with an exact-path rule —
> only the selected root's two control files are special; every other path
> is ordinary source regardless of name or content. (4) One public derivation
> entrypoint bound to a single pinned root (manifest bytes and source bytes
> can no longer be supplied independently). (5) A conformance invariant ties
> the strict identity parser to the existing v0.3 normalizer, with a custom
> deserializer required for the `TargetsConfig` flatten pattern. (6)
> `ProgramSourceDigest` (sha256-only) replaces bare `ContentDigest`;
> `projection_schema` becomes a unit-like type. (7) Nested-boundary and
> parser-conformance fixture vectors added.
>
> **r6 (2026-07-24):** closes the string-normalization layer.
> (1) Rule 4's ad-hoc path list is replaced by a complete **semantic-type
> matrix** over every identity-bearing string field — the r5 list missed
> `storage.volumes[].mount_path`, `services.*.state_bindings[].target`,
> `bindings.*.mount`, `context.mount`, `snapshot.warmup_paths`/
> `content_ready_path` (HTTP request-targets, not filesystem paths),
> `build.inputs.*`, `transparency.allowed_binaries`, `pack.include/exclude`,
> `ingress` prefixes, and readiness-probe targets; it also listed a
> nonexistent "dependency file paths" row (neither `DependencySpec` nor
> `ToolDependencySpec` has one) and misclassified `working_dir`, which is
> runtime-dependent (source ⇒ source-relative; OCI ⇒ absolute guest path
> like `/app`; a blanket `ProgramRelativePath` would reject valid OCI
> manifests). (2) `ProgramRelativePath` gains a canonical **Root** form —
> the existing v0.3 normalizer legitimately produces `"."` for web static
> root entrypoints, which r5's grammar rejected. (3) The orphan parent
> claim (`execution.capsule_program_id` present, `program_identity` absent)
> is now its own state: `ParentEnvelopeMissing`, fail closed in Phase 0.
> (4) Source-referencing paths split into existence-checked
> (`SourceExistingPath`) vs. lexical-only (`SourceRelativeFuturePath`),
> with symlink containment grounded in A1v2's blanket in-tree symlink
> rejection. (5) Title no longer says "target-independent." (6) Regression
> vectors added, including execution_id byte-identity across parent-claim
> addition.
>
> **r7 (2026-07-24):** makes Rule 4's completeness claim
> true. (1) The structured targets (`targets.wasm` / `targets.source` /
> `targets.oci`) and `TargetsConfig`'s own global fields (`source_digest`,
> `health_check`, `env`, `preference`) were unclassified — they are now
> canonicalized into the same target IR as named targets and classified
> field by field, including `SourceTarget.dependencies` (a real source-path
> field — the r5/r6 claim that no dependency file path exists was true of
> `DependencySpec`/`ToolDependencySpec` but not of `SourceTarget`) and
> `OciTarget.digest`. (2) Missing named-target fields added:
> `model_repo_sha256`, `model_repo_include[]`. (3) `readiness_probe.port`
> reclassified — it is a placeholder NAME, not a host:port target — as
> `ProbePortReference`. (4) `contracts.*.ready` (the dependency-grammar
> `ReadyProbe` tagged union) classified per variant with a `Templated*`
> family, kept distinct from the target-level `ReadinessProbe`. (5)
> `execution` fields classified per runtime. (6) The nonexistent `state.*`
> mount-path row deleted (`StateRequirement` has no path field). (7)
> `OpaqueAuthoredString` is no longer a catch-all — it is a finite
> enumeration, and the IR is required to be fully newtyped so an
> unclassified `String` cannot compile. (8) The single pin type split into
> `Sha256DigestPin` / `CasContentDigest` / `GitCommitRevision` with
> per-field encodings. (9) The type hierarchy (base value types vs.
> validation policies) stated explicitly.
>
> **r8 (2026-07-24), accepted:** the review approved the architecture with
> three targeted type fixes, applied here: (1) `targets.wasm.world` is a
> WIT world reference (`wasi:cli/command`, `uarc:v1/http-handler` — `:` and
> `/` fail the Identifier grammar) → new `WitWorldRef` type with default
> expansion. (2) `targets.*.user` / `targets.oci.user` admits `uid`,
> `uid:gid`, or an image-resolvable name (`1000:1000` fails Identifier) →
> new `ContainerUserSpec` type, shared by named and structured OCI targets.
> (3) SHA-256 authoring spelling separated from canonical IR spelling: the
> existing validator accepts both bare and `sha256:`-prefixed
> `model_sha256`, while `targets.source_digest` REQUIRES the prefix — so
> the canonical IR is uniformly bare 64 lowercase hex, and each field's
> accepted authoring spellings normalize into it (both `model_sha256`
> spellings → the SAME IR, not a rejection; unprefixed `source_digest`
> stays rejected because the existing validator already rejects it).
> Status moved to `accepted`; no further review round required.
>
> **r9 (2026-07-24), amendment:** follows the `capsule.lock` rename
> amendment in `CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §5 (canonical lock file
> `ato.lock.json` → `capsule.lock`; `AtoLock` → `CapsuleLock`; module
> `ato_lock` → `capsule_lock`; old `lockfile::CapsuleLock` →
> `LegacyCapsuleLock`). The source projection's control files generalize
> from a hardcoded pair to `CapsuleControlFiles { manifest, lock:
> Option<PathBuf> }`: the canonical lock path is RESOLVED first
> (capsule.lock preferred; ato.lock.json as deprecated read alias;
> coexistence of both at the selected root rejects before derivation) and
> only that one resolved path is excluded. The lock file name never enters
> any preimage, so `capsule_program_id`, `lock_id`, `execution_id`, and
> signature payloads are identical across the rename — pinned by new
> fixtures (no-lock / capsule.lock / deprecated-alias ⇒ identical digest
> and id; coexistence ⇒ reject). Collision note: the OLDEST pre-0.3 legacy
> lock name was itself `capsule.lock`; its read path is retired in the same
> change (see the spec amendment), so no content sniffing is ever needed to
> tell canonical from ancient-legacy at the root.
>
> **r10 (2026-07-25), amendment — review round 2:** three normative gaps the
> first implementation round exposed. (1) §1 gains step 0: a root-level
> `.git` of any node type disqualifies the input as a pinned materialization
> (previously the `.git` rejection would have been implementation-chosen
> strictness, and a bare Git checkout could have pulled `.git` into the
> projection since A1v2 only rejects a NESTED `.git`). (2) §1 gains step 1b:
> a process-private staging copy immediately after the admissibility pass,
> with steps 2–6 resolving exclusively inside it. Without it an
> implementation could satisfy the six steps literally and still leave a
> TOCTOU window in which manifest intent and source digest come from
> different tree states. (3) §1 gains the lexical, fail-closed presence rule
> for control-file names, shared with the runtime canonical-lock resolver —
> two independent spellings of "exists" diverge on a dangling symlink, which
> let one path select a lock the other rejected as a split brain.
>
> Not amended, because the ADR text was already correct and the
> implementation had drifted from it: §2.2 requires a `SourceExistingPath`
> to exist **in the ProgramSourceProjection**, so a manifest naming a
> control file (the manifest itself or the resolved lock) as a
> source-existing path must be rejected — those bytes are excluded from the
> projection, and accepting them would put a reference in the identity that
> the hashed tree does not contain.
>
> **r11 (2026-07-25), amendment — review round 3:** §2.0's normative
> signature still showed `derive_capsule_program_contract(pinned_root:
> &Path)` after r10 introduced the proof-carrying boundary, so the document
> that Phase 1's second-language implementation and future producer wiring
> read as the API contract disagreed with the implementation. §2.0 now fixes
> the parameter as `&VerifiedPinnedSourceMaterialization` and, more
> importantly, states which minting paths are normative: the earned
> archive-extraction path is the only public one, a bare-path assertion is
> explicitly NOT a public contract (test scaffolding only), and the future
> materializer seam is described rather than declared as unused scaffolding.
> §2.0 also records that staging is the derivation function's own
> responsibility, so a caller cannot opt out of the isolation §1 step 1b
> requires.

## Context

An external review of the Capsule architecture converged on a critique of the
Capsule v1 spec: Execution Identity — a content-addressed identity of a
*resolved, target-specific* launch contract — is well designed, but the spec
prohibits any identity above it
(`docs/rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §3.2, §16.4).
"Same declared Capsule, N targets" is expressible today only through names
and versions.

**Implementation base**: `origin/nightly` @ `f7ee059b` (#1098–#1102 merged;
#1102 touches only `crates/snapshot` + CI). Existing structures this ADR
builds on, all verified on nightly:

- `ExecutionContractV1` / `ExecutionContractEnvelopeV1` /
  `VerifiedExecutionId`
  (`crates/capsule/src/contract/execution_contract.rs:760,1076,551`).
- The lock's D2/D4 split (`ato_lock/canonicalize.rs`):
  `CanonicalLockProjection` (feeds `lock_id`) vs.
  `CanonicalSignatureProjection` (superset).
- The lock trust boundary: one private fn (`ato_lock/mod.rs:74`) at three
  chokepoints (`load_verified_from_str:86`, `to_pretty_json:148`,
  `write_canonical_to_vec:174`).
- The frozen source-tree hash `materialized_source_tree_hash(root)`
  (`foundation/blob/source_tree.rs:189`) — A1v2 admissibility + frozen A1 v1
  tree hash (`sha256:<hex>`), exclusions being the caller's responsibility.
- Canonical-vector convention:
  `crates/capsule/tests/fixtures/execution_contract/{vectors,expected}/`.
- `select_snapshots(&VerifiedExecutionId, …)`
  (`contract/snapshot_manifest.rs:737`).
- `CapsuleManifest` (`foundation/types/manifest.rs:606-829`) — the v0.3
  authoring surface; 41 top-level fields, classified completely in §2.1.

## Decision

### 0. What this identity is — and is not (normative)

> **Capsule Program Identity is the exact identity of a canonical, immutable
> Capsule declaration: a pinned program source projection plus normalized
> authored manifest intent. It does not claim identity of a fully resolved
> program closure.** Execution Identity remains the sole exact identity of a
> resolved, runnable, target-specific launch envelope. Capsule Program
> Identity is never used as an execution compatibility, Snapshot selection,
> placement, or restore key; when present, its structural integrity and its
> parent-association claim are mandatory trusted-load checks.

The two-layer model:

```text
Capsule Program Identity   = immutable DECLARATION identity
Execution Identity         = immutable RESOLVED runnable identity
```

Three consequences, stated plainly rather than left implicit:

- **Target-selection-independent, target-definition-dependent.** The id does
  not depend on which declared target a given run selects — all Execution
  Identities resolved from one declaration share one parent id. It DOES
  change when the declaration itself changes: adding a Wasm target, widening
  `requirements`, permitting emulation, or revising `[snapshot]` intent is a
  new declaration, hence a new id. That is correct under the declaration
  definition — the author changed what is declared.
- **Unresolved dependencies are declared, not resolved.** The preimage hashes
  version *constraints* (`>=16,<17`), mutable refs as authored, and build
  commands whose network fetches are not pinned here. The same
  `capsule_program_id` therefore MAY resolve to different concrete programs
  at different times. That is by design: exact resolved identity is
  Execution Identity's job, and each resolution records its parent
  declaration id.
- **This explicitly overturns, for v1, the earlier working conclusion** (from
  the external-review discussion) that execution requirements, the target
  matrix, and snapshot policy sit outside a program "revision." Under
  declaration semantics they are authored intent like everything else, and
  including them is self-consistent. The alternative — splitting authored
  intent into three hashed facets (program semantics / resolution &
  compatibility intent / snapshot derivation intent) — is recorded in
  Alternatives as Option G and deferred: it would reintroduce per-field
  bucket-assignment judgment across the whole manifest, which is exactly the
  classification-drift failure mode r4 eliminated. If a future need arises
  to group declarations that differ only in compatibility intent, a derived
  "interface identity" can be layered on top without changing this id.

The name **Capsule Program Identity** / `capsule_program_id` is retained for
continuity (and to avoid the ADR-012 `capsule_revisions` collision); every
normative sentence in the spec uses the declaration definition above.

Derivation pipeline:

```text
pinned source materialization (selected capsule root)
  │
  │  derive_capsule_program_contract(pinned_root)     ← the ONLY public entrypoint
  │    ├─ read <pinned_root>/capsule.toml (same root — never a separate input)
  │    ├─ ordinary v0.3 load + validation (existing normalizer) MUST succeed
  │    ├─ strict ProgramManifestV03Input parse MUST succeed
  │    ├─ adapter → ProgramManifestIntentV1
  │    └─ ProgramSourceProjectionV1 over the same pinned_root → ProgramSourceContract
  ▼
CapsuleProgramContractV1 { schema, source, manifest_intent }
  │ JCS + BLAKE3("ato.capsule-program/v1" || 0x00 || …)
  ▼
CapsuleProgramEnvelopeV1 { program_contract, capsule_program_id, provenance… }
  │ authenticated association claim (when both present in a lock)
  ▼
ExecutionContractEnvelopeV1.capsule_program_id
  ▼
Snapshot / Session selection: VerifiedExecutionId only (unchanged)
```

### 1. `ProgramSourceProjectionV1`

**Input**: a pinned source materialization only (immutable archive /
`source_materialize` output, extracted and validated). Local working trees
are inadmissible in Phase 0.

**Control files — exact path rule, no content sniffing** (r5; generalized in
r9 for the `capsule.lock` rename): the pinned materialization has a
**selected capsule root**. The control files are the manifest plus the
**selected canonical lock path** — resolved first, then excluded:

```rust
struct CapsuleControlFiles {
    manifest: PathBuf,      // <selected-root>/capsule.toml
    lock: Option<PathBuf>,  // the ONE selected canonical lock path, if any
}
```

Lock-path selection at the selected root (mirrors the canonical-lock
migration rules in `CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §5 Amendment):

```text
<selected-root>/capsule.lock exists only      → lock = capsule.lock
<selected-root>/ato.lock.json exists only     → lock = ato.lock.json
                                                (deprecated alias, warn)
both exist                                    → REJECT before derivation
                                                (split-brain; never excluded
                                                both, never chose silently)
neither exists                                → lock = None
```

**Every other path is ordinary source and is hashed — regardless of its file
name or content.** A fully valid Capsule manifest at
`examples/capsule.toml` or a lock at `fixtures/capsule.lock` /
`fixtures/ato.lock.json` is test data bytes, nothing more. There is no
"manifest-shaped TOML" predicate and no nested-Capsule rejection scan:
bytes are never sniffed to guess intent, so the projection is a pure
function of (tree, selected root). A multi-Capsule repository is handled by
materializing each Capsule with its own selected root — each derivation
sees its own control files and everything else (including sibling Capsules'
manifests) as ordinary source.

**Order (normative)**:

```text
0. Reject a root that is not a pinned materialization BEFORE step 1. A
   root-level `.git` entry — of any node type (directory, gitfile, symlink)
   — is disqualifying: a Git checkout is exactly the working-tree lane
   Phase 0 forbids, its index/pack bytes are nondeterministic, and A1v2 only
   rejects a NESTED `.git` (submodule signal) plus a root `.gitmodules`, so
   this closes the remaining hole. Excluding it instead would silently widen
   the exhaustive control-file list in step 4.
1. A1v2 admissibility over the ORIGINAL tree, in full — including the
   control files. A control file that is a symlink, FIFO, or device fails
   closed here; exclusion never hides it from admissibility.
1b. IMMEDIATELY after step 1, materialize a process-private staging copy of
   the pinned root. Steps 2–6 resolve exclusively inside that copy and the
   original tree is never read again. Without this, an implementation that
   satisfies steps 1–6 literally still leaves a TOCTOU window: the manifest
   parse, the `SourceExistingPath` checks, and the projection copy would each
   re-open paths an outside process can mutate after the admissibility pass,
   so manifest intent and source digest could come from different tree
   states, and a regular file could be swapped for a symlink between the
   check and the copy.
2. Verify <selected-root>/capsule.toml exists and parses per §2.
3. Resolve CapsuleControlFiles (above); coexistence of capsule.lock and
   ato.lock.json at the selected root rejects here.
4. Exclude exactly the resolved control-file paths (manifest + the selected
   lock path, if any). Nothing else.
5. Materialize the projected tree preserving bytes AND the executable bit
   (A1 file identity includes the executable bit).
6. materialized_source_tree_hash(projected_root) — existing, frozen,
   unmodified.
```

**Lock-name presence is lexical and fail-closed** (normative): whether a
control-file name is "present" is decided WITHOUT following symlinks and
WITHOUT collapsing errors into absence — only a `NotFound` metadata error
means absent; every other error (permission, I/O) propagates. A dangling
symlink, directory, or FIFO occupying a lock name IS present, so it
participates in the coexistence check; a lock path that is selected but is
not a regular file is then rejected. The same helper MUST decide presence
for both this projection and the runtime canonical-lock resolver — two
implementations of "exists" diverge (e.g. `Path::exists()` follows a
dangling symlink to `false` while `symlink_metadata` sees the entry), which
would let one path resolve a lock the other treats as a split brain.

**Types** (Major 3 — no algorithm laundering):

```rust
/// The A1 source-tree digest: ALWAYS sha256, 32 bytes, lowercase hex.
/// A bare ContentDigest would also admit blake3 — structurally valid,
/// normatively wrong — so the narrower type enforces the A1 contract.
pub struct ProgramSourceDigest(/* sha256-only, validated on construction */);

/// v1 projection rules are fully fixed by this ADR; there is no per-Capsule
/// payload to hash, so the schema marker is a unit-like type, not a String.
pub struct ProgramSourceProjectionSchemaV1; // serializes as exactly
                                            // "ato.capsule-program-source-projection/v1"

pub struct ProgramSourceContract {
    pub digest: ProgramSourceDigest,
    pub projection_schema: ProgramSourceProjectionSchemaV1,
}
```

**Self-reference invariant (MUST + tests)**: the digest is identical across
all three of {no lock, `<selected-root>/capsule.lock` present,
`<selected-root>/ato.lock.json` present} (including a lock populated with
`program_identity`) — the canonical lock file name never reaches the
preimage, so `capsule_program_id` is identical across the rename migration.
Coexistence of both lock names at the selected root rejects before
derivation. A nested `fixtures/capsule.lock` or `fixtures/ato.lock.json`
DOES change the digest (ordinary source).

### 2. Manifest intent

#### 2.0 One entrypoint, one proof-carrying root (Major 1; signature fixed in r11)

```rust
pub fn derive_capsule_program_contract(
    pinned: &VerifiedPinnedSourceMaterialization,
) -> Result<CapsuleProgramContractV1, CapsuleProgramError>
```

The manifest is read from the pinned root's `capsule.toml` inside this
function. There is no public surface taking `raw_manifest: &str` and
`source_root: &Path` independently — a producer cannot pair source A with
manifest B.

**The parameter is a proof, not a path (normative).** A bare `&Path` would
let any caller feed a local working tree into the identity pipeline, which
§1 forbids. The proof type therefore has a private field, no public
constructor, and exactly one **earned** public minting path:

```text
Public, production:
  VerifiedPinnedSourceMaterialization::from_source_archive(archive)
    Extracts a content-addressed `.tar.zst` into a process-private directory
    the value owns. The bytes are immutable and named by their own hash and
    the destination is fresh, so the proof is earned by construction.

NOT a public contract:
  A bare-path assertion ("trust me, this directory is pinned") MUST NOT be
  reachable from outside the implementation crate. Such a constructor may
  exist only as test scaffolding (crate-internal, test-only). Exposing it
  publicly would move the compile-time guarantee one method outward and
  restore exactly the hole the proof type exists to close — a caller could
  self-attest an arbitrary directory and derive an identity whose claimed
  provenance is unverified.

Future:
  When a source resolver / CAS materializer that yields an EXTRACTED tree
  exists, it gets its own crate-internal minting seam taking that
  materializer's capability type. No such producer exists today, so none is
  declared — an unused capability type would be indistinguishable from the
  assertion it is meant to replace.
```

**Staging is this function's responsibility**, not the caller's: it runs the
§1 order (admissibility gate over the original tree, then the process-private
staging copy) and resolves the manifest, the strict adapter's
`SourceExistingPath` checks, and the projection exclusively inside that copy.
A conforming implementation therefore never re-reads the pinned root after
the gate, and callers cannot opt out of the isolation.

#### 2.0.1 Conformance with the existing v0.3 normalizer (Major 2)

The v0.3 parser is not a plain serde derive: it merges legacy
`env.required`, separates top-level and package build, normalizes runtime
selectors (`source`/`native`/`web` conversions), rejects
`entrypoint`/`cmd` conflicts with an OCI `cmd` special case, validates
`run_once`, etc. A second, independent parser would drift from the runtime's
interpretation. Therefore, normatively:

```text
Program Identity issuance requires BOTH to succeed:
  1. the existing load path — CapsuleManifest::from_toml_with_path +
     validate_for_mode(Strict) (via contract/manifest.rs::load_manifest)
  2. the strict ProgramManifestV03Input parse
```

and the adapter consumes, wherever a v0.3 normalization exists, the
**post-normalization canonical value** (from the validated `CapsuleManifest`)
rather than re-deriving its own interpretation from raw TOML. The strict
input type's job is *rejection* (unknown/duplicate/ambiguous fields fail
closed); the normalizer's job is *meaning*. A conformance fixture suite
(§9) pins that both paths agree.

`TargetsConfig` mixes known fields with arbitrary named targets via
`#[serde(flatten)]`, so `deny_unknown_fields` alone cannot police it.
`ProgramTargetsV03Input` requires a custom deserializer: reserved keys parse
as known fields; every other key parses as a named target; unknown fields
*inside* a named target are rejected.

#### 2.1 Complete v0.3 top-level classification (normative — all 41 fields)

Source of truth: `CapsuleManifest`, `foundation/types/manifest.rs:606-829`
@ `f7ee059b`.

**Identity-bearing** (31): `capsule_type`, `default_target`, `capabilities`,
`requirements`, `execution`, `storage`, `state`, `network`, `model`,
`transparency`, `build`, `pack`, `isolation`, `polymorphism`, `targets`,
`platforms`, `exports`, `services`, `dependencies`, `tool_dependencies`,
`required_env`, `contracts`, `foundation_requirements`, `host_capabilities`,
`ingress`, `snapshot`, `secrets`, `bindings`, `external`, `context`,
`generated_bindings`.

Under the declaration definition (§0), `requirements`,
`foundation_requirements`, `targets`, and `snapshot` are authored intent and
belong here — see §0's explicit-overturn note. (`generated_bindings`' own
doc comment already anticipates this: the value "is never stored in the
artifact, receipt, logs, or identity (only this spec is)" —
manifest.rs:820-828.)

**Non-identity provenance** (9; (p) = recorded in
`CapsuleProgramEnvelopeV1.provenance`): `schema_version` (p, as
`authoring_schema` — the IR is semantic; its own `schema` string is the only
schema in the hash), `name` (p — locator; mirrors
`ResolvedRefProvenanceV1`'s alias rule), `version` (p — locator; ADR-012's
concern), `metadata` (display), `distribution` (pack/publish-generated),
`state_owner_scope` and `service_binding_scope` (operational registry
handles), `routing` (placement policy — spec §2.2/§4.3), `pool`
(operational performance tuning).

**Unsupported, fail closed** (1): `workspace` — a multi-app authoring
surface is not a single Capsule declaration; fails Program Identity issuance
with an explicit error until a multi-app identity is defined.

This table is exhaustive at the top level; changing any row is a semantic
change to `ato.capsule-program/v1` requiring a new schema version.

#### 2.2 Nested preimage boundary (Blocker 3 — now normative, closed)

**Rule 1 — default**: within an identity-bearing section, every nested field
is identity-bearing.

**Rule 2 — non-identity nested exceptions (complete enumeration)**:

| Nested field | Why excluded |
|---|---|
| `build.outputs.*` (`BuildOutputsConfig`: `capsule`, `sha256`, `blake3`, `attestation`, `signature`) | pack-time digest/attestation/signature *emission* toggles — publication artifacts policy, not program behavior |
| `build.policy.*` (`BuildPolicyConfig`: `require_attestation`, `require_did_signature`) | publish-time verification policy |
| `exports.cli.<name>.description` | display-only |
| `targets.<label>.model_filename`, `targets.<label>.model_format` | informational per their own doc comments |

Every other nested field under an identity-bearing section is
identity-bearing. Adding to this exception list is a semantic change to
`ato.capsule-program/v1`.

**Rule 3 — unsupported nested fields, fail closed (complete enumeration)**:

| Nested field | Rule |
|---|---|
| `targets.<label>.engine_path` | present ⇒ Program Identity issuance fails closed — a host-local filesystem path overriding managed fetch cannot be part of a portable declaration |
| `targets.<label>.model` | admissible only as a `SourceExistingPath` (below); absolute or out-of-tree ⇒ fail closed |
| `targets.<label>.working_dir` on a Wasm target | present ⇒ fail closed (no defined semantics) |

**Rule 4 — semantic-type matrix**: no identity-bearing string field is
hashed as an uninterpreted string. Every one is assigned exactly one
semantic type below; hashing a field not yet in this matrix is a schema
violation, and adding a field to the matrix is a semantic change to
`ato.capsule-program/v1`.

The semantic types. The hierarchy is explicit: `SourceRelativePath`,
`GuestPath`, `HttpRequestTarget`, `TcpProbeTarget`, `ProbePortReference`,
`GlobPattern`, `RemoteArtifactRef`, the three pin types, `TemplatedString`,
`OpaqueCommand`, `OpaqueAuthoredString`, and `Identifier` are **base value
types** (each with one canonical grammar and serialization);
`SourceExistingPath` and `SourceRelativeFuturePath` are **validation
policies over `SourceRelativePath`**, not separate value types.

```text
Base value types:
  SourceRelativePath      source-relative path; Root ("." only) | Relative
  GuestPath               absolute in-guest path (existing type, unchanged)
  HttpRequestTarget       absolute-path HTTP request-target ("/", "/app", …)
  TcpProbeTarget          host:port / port probe target
  ProbePortReference      a placeholder NAME a probe refers to — not a target
  GlobPattern             authored glob, hashed as authored
  RemoteArtifactRef       URL / image ref / model-repo ref, hashed as authored
  Sha256DigestPin         canonical IR: exactly 64 lowercase hex — per-field
                          authoring spellings normalize INTO it (see the
                          spelling table below)
  CasContentDigest        algorithm-prefixed CAS digest ("sha256:<hex>", …)
  GitCommitRevision       exactly 40 lowercase hex (immutable commit, not a digest)
  WitWorldRef             validated WIT world reference ("wasi:cli/command",
                          "uarc:v1/http-handler") — contains ':' and '/', so it
                          is NOT an Identifier; absent ⇒ default-expanded to
                          "wasi:cli/command" before hashing
  ContainerUserSpec       container user: "uid" | "uid:gid" | image-resolvable
                          user/group name ("1000:1000" is valid) — NOT an
                          Identifier
  TemplatedString         dependency-grammar templated value ({{…}} expressions);
                          template syntax validated by the existing grammar,
                          hashed as authored
  OpaqueCommand           authored command string / argv, no path interpretation
  OpaqueAuthoredString    authored free-form value, hashed verbatim —
                          FINITE ENUMERATION below, never a catch-all
  Identifier              names, labels, keys (ASCII-identifier discipline)

Validation policies over SourceRelativePath:
  SourceExistingPath        lexical + must exist in the projection (containment below)
  SourceRelativeFuturePath  lexical only (target may be created by a build step)
```

**Structured targets are canonicalized, not separately classified**: the
adapter normalizes `targets.wasm` / `targets.source` / `targets.oci`
(`WasmTarget`/`SourceTarget`/`OciTarget`, manifest.rs:2021-2094) into the
same normalized-target IR as named targets — one IR shape, so a structured
and a named spelling of the same target intent produce the same IR (pinned
by a conformance vector). `TargetsConfig`'s own global fields
(`preference`, `source_digest`, `port`, `startup_timeout`, `env`,
`health_check`) are classified directly.

Field assignments (complete over the identity-bearing surface; verified
against `foundation/types/manifest.rs`, `foundation/types/ready_state.rs`,
and `foundation/types/dependency_grammar.rs` @ `f7ee059b`):

| Semantic type | Fields |
|---|---|
| `SourceExistingPath` | `targets.<label>.model`; `targets.source.dependencies` (the declared dependencies file — requirements.txt / package.json); `build.inputs.lockfiles[]` |
| `SourceRelativeFuturePath` | `targets.<label>.entrypoint` (incl. `targets.source.entrypoint`), `.component`; `targets.<label>.outputs[]`; `build.inputs.artifacts[]`; `exports.binaries.*` / `exports.paths.*` (relative to the materialized tool root); `execution.entrypoint` for source/web/wasm runtimes |
| `GuestPath` | `storage.volumes[].mount_path`; `services.*.state_bindings[].target`; `bindings.<name>.mount`; `context.mount`; `contracts.*.state.mount`; `targets.<label>.working_dir` on an **OCI** target (absolute container workdir, e.g. `/app`) |
| `SourceRelativePath` (Root allowed) | `targets.<label>.working_dir` on a **source/web** target |
| `HttpRequestTarget` | `snapshot.warmup_paths[]`; `snapshot.content_ready_path`; `ingress.<route>` path prefixes incl. `upstream_path_prefix`; `readiness_probe.http_get`; `targets.health_check` (global); `execution.health_check` |
| `TcpProbeTarget` | `readiness_probe.tcp_connect` |
| `ProbePortReference` | `readiness_probe.port` (a non-empty placeholder name per the existing validator — NOT a host:port) |
| `GlobPattern` | `pack.include[]` / `pack.exclude[]`; `transparency.allowed_binaries[]`; `build` exclude patterns; `targets.<label>.model_repo_include[]` (download-file allowlist) |
| `RemoteArtifactRef` | `targets.<label>.image` (incl. `targets.oci.image`), `.model_url`, `.model_repo`, `.engine`; `platforms.<os-arch>.artifact`; `dependencies.*` / `tool_dependencies.*` capsule URLs; `execution.entrypoint` for the OCI runtime (a Docker image ref) |
| `Sha256DigestPin` | `targets.<label>.model_sha256`, `.model_repo_sha256`; `platforms.<os-arch>.sha256`; `targets.source_digest` (the L1 source-archive pin) — per-field authoring spellings below |
| `CasContentDigest` | `targets.wasm.digest`; `targets.oci.digest` |
| `GitCommitRevision` | `targets.<label>.model_revision` (40-hex immutable commit — a revision, not a content digest) |
| `WitWorldRef` | `targets.wasm.world` (absent ⇒ `wasi:cli/command` default expanded before hashing) |
| `ContainerUserSpec` | `targets.<label>.user`; `targets.oci.user` — both canonicalize to the same type |
| `TemplatedString` | `contracts.*.ready` per variant — `Probe.run` (templated command), `Http.url` (templated HTTP target), `Tcp.target` (templated tcp target), `UnixSocket.path` (templated guest path), `Postgres.host`/`.port`/`.user`/`.database` (per-field templated values); `contracts.*.identity_exports.*`; `contracts.*.runtime_exports.*` — the dependency-grammar `ReadyProbe` is a different type from the target-level `ReadinessProbe` and is never conflated with it |
| `OpaqueCommand` | `targets.<label>.cmd` (incl. `targets.oci.cmd[]`), `.build_command`, `.install_command`, `.prestart_command`, `.run_command`; `targets.source.args[]`; `readiness_probe.exec`; `build.lifecycle.*` commands; `services.*.entrypoint`; `exports.cli.*.args` |
| `Identifier` | all map keys (`state`, `targets`, `services`, `dependencies`, `env` maps, …); `needs[]`; `default_target`; `targets.preference[]` entries; env variable NAMES (`required_env`, `env_allowlist`, `build_env`, `isolation.allow_env`); `state.*.producer`, `.schema_id`; `contracts.*.target`; enum-valued fields (`state.*.kind`/`.durability`/`.attach`/`.sharing`, `execution.runtime`, signal names) |
| `OpaqueAuthoredString` (finite enumeration — adding an entry is a schema decision, never a default) | env variable VALUES (`targets.<label>.env.*`, `targets.env.*`, `execution.env.*`, `targets.oci.env.*`, `targets.wasm.config.*`); `targets.<label>.runtime`, `.driver`, `.language`, `.runtime_version`, `.engine_version`, `.engine_variant`, `.source_layout`, `.package_type`; `targets.source.language`, `.version`; version constraints in `dependencies.*` / `tool_dependencies.*`; `state.*.purpose`; probe/contract timeout strings; `runtime_tools` values; `host_capabilities[].reason` |

**SHA-256 authoring spelling vs. canonical IR (normative)**: the canonical
IR spelling of every `Sha256DigestPin` is uniformly bare 64 lowercase hex.
Authoring spellings differ per field — following what the existing v0.3
validator accepts (§2.0.1's principle: the normalizer decides meaning; the
strict input rejects only what the normalizer also rejects or leaves
ambiguous) — and normalize INTO that one IR spelling:

```text
model_sha256 / model_repo_sha256
  authoring: <64hex>  OR  sha256:<64hex>   (both accepted by the existing
                                            validator — the two spellings
                                            produce the SAME IR, never a
                                            rejection)
  canonical IR: <64 lowercase hex>

platforms.<os-arch>.sha256
  authoring: bare SHA-256 as the existing validator accepts it
  canonical IR: <64 lowercase hex>

targets.source_digest
  authoring: sha256:<64hex> ONLY (the existing validator rejects the
             unprefixed spelling — "source_digest must start with
             'sha256:'" — so the strict input rejects it too)
  canonical IR: <64 lowercase hex> (prefix stripped)
```

Uppercase hex, where the existing parser tolerates it, is lowercased during
normalization — never a distinct IR value and never a Program-Identity-only
rejection.

**No raw `String` in the IR (normative)**: every `ProgramManifestIntentV1`
field is one of the semantic newtypes above — e.g.
`NormalizedSourceTargetIntent { entrypoint: SourceRelativeFuturePath,
dependencies: Option<SourceExistingPath>, args: Vec<OpaqueCommand>, … }`.
A field that would have to be a bare `String` is, by construction, an
unclassified field — it cannot compile, so the matrix cannot silently rot.
This replaces r6's "every remaining value" catch-all, which contradicted
the "hashing an unmatrixed field is a schema violation" rule (the r7
discoveries — `model_repo_sha256`, `SourceTarget.dependencies` — proved
that catch-all unsound).

`execution` note: raw `[execution]` authoring is not part of the accepted
v0.3 surface — the adapter consumes the block only as the existing
normalizer's canonical derived output (§2.0.1), classified per the rows
above (`entrypoint` is runtime-dependent: OCI ⇒ `RemoteArtifactRef`,
source/web/wasm ⇒ `SourceRelativeFuturePath`).

Corrections carried from r5/r6, restated: `DependencySpec`/
`ToolDependencySpec` have no file-path fields, but `SourceTarget.dependencies`
DOES exist and is classified above; `external.*` has no mount path;
`StateRequirement` has no mount path either (`kind`, `durability`,
`purpose`, `producer`, `attach`, `schema_id`, `sharing`, `size_mb`) — the
former `state.*` GuestPath row was wrong and is deleted; state mount paths
live on `services.*.state_bindings[].target` and `contracts.*.state.mount`;
`working_dir` is runtime-dependent, split across three rows.

**Containment and existence rules for source-referencing fields**:

```text
SourceExistingPath:
  1. lexical SourceRelativePath validation
  2. join onto the selected root
  3. the joined path MUST exist in the ProgramSourceProjection as a regular
     file or directory of the expected kind
  4. no symlink traversal — guaranteed a priori because A1v2 admissibility
     (which runs over the FULL tree before projection, §1) rejects every
     in-tree symlink outright (source_tree.rs, admissibility rule 4); the
     rule is stated here anyway so it survives any future relaxation of A1

SourceRelativeFuturePath:
  lexical validation only — the target may be produced by a later build
  step, so existence is not checked
```

**`SourceRelativePath` grammar (Blocker-2 fix — canonical Root)**:

```rust
enum SourceRelativePath {
    Root,                              // canonical spelling: exactly "."
    Relative(NormalizedRelativePath),  // "src/app", …
}
```

`"."` is the ONLY canonical spelling of Root — required because the
existing v0.3 normalizer legitimately produces `"."` for a web static root
entrypoint (e.g. `run = "index.html"` normalizes its parent directory to
`"."`), which r5's grammar wrongly rejected. Non-canonical spellings
(`""`, `"./"`, `"./x"`, `"x/."`, `"x/.."`) are rejected fail-closed, not
silently normalized — one canonical spelling per value, same discipline as
everywhere else. `Relative` keeps r5's rules: relative-only, UTF-8, NFC,
`/` separator, no `.`/`..` segments, no empty segment, no leading/trailing
`/`, no NUL/control, UTF-8 byte ordering. (`ProgramRelativePath` from r4/r5
is renamed `SourceRelativePath`; `SourceExistingPath` and
`SourceRelativeFuturePath` are validation modes over it.)

#### 2.3 Remaining normalization rules (unchanged from r4)

Absent ≡ explicit default (one canonical spelling: omitted); maps are
`BTreeMap` sorted with duplicate-key rejection; order-sensitive lists
(build lifecycle, `targets.preference`, `pack.include`/`exclude`) preserved
as authored, set-like lists (`required_env`, `network.egress_allow`) sorted
+ deduplicated; serde aliases (e.g. `ServiceSpec` `command`→`entrypoint`,
`NamedTarget` `build`→`build_command`, `install`, `prestart`, `run`,
`depends_on`→`needs`) normalize to canonical names, enumerated once in the
adapter; deprecated fields classified by the same behavior test with the
verdict in adapter doc comments.

### 3. Hash

```text
capsule_program_id =
  "blake3:" + hex(BLAKE3(UTF8("ato.capsule-program/v1") || 0x00 || JCS(program_contract)))
```

JCS + BLAKE3 + domain separator + `deny_unknown_fields` + no
self-reference, exactly as `execution_id`.

### 4. Proof-carrying id and envelope (unchanged from r4)

`VerifiedCapsuleProgramId` — private field, no public constructor, exactly
**one** sanctioned construction path in v1:
`CapsuleProgramEnvelopeV1::verified_capsule_program_id()`. Four compile-fail
doctests mirroring `VerifiedExecutionId`'s. `CapsuleProgramEnvelopeV1`
carries `program_contract`, `capsule_program_id`, `generated_at`,
`provenance` (authoring_schema, name, version), `diagnostics`; tolerant of
unknown fields; `verify()` recomputes fail-closed.

### 5. Parent link — an authenticated association claim (unchanged from r4)

```text
VerifiedCapsuleProgramId   proves: declaration contract's hash matches its stored id
VerifiedExecutionId        proves: execution contract's hash matches its stored id
verify_program_parent      proves: the lock's parent CLAIM is internally consistent
Lock signature             proves: the signer made that claim
Derivation proof           NOT provided in Phase 0 — nothing proves this
                           ExecutionContractV1 was resolved from this declaration;
                           that needs a finalization receipt / resolver
                           attestation (separate ADR)
```

`ExecutionContractEnvelopeV1` gains the additive non-identity claim field
`capsule_program_id: Option<CapsuleProgramId>`. The pairwise check lives in
the Program module (`verify_program_parent(&VerifiedCapsuleProgramId,
&ExecutionContractEnvelopeV1)` — `ParentMissing` / `ParentMismatch`
distinct); `capsule_lock/execution.rs::verify_lock_program_identity` interprets
the lock's states and mints the verified id exactly once.

**Complete lock state matrix (Major-1 fix — the orphan claim is its own
state, not folded into "legacy")**:

```text
program_identity ABSENT + execution claim ABSENT   → Ok (true legacy)
program_identity ABSENT + execution claim PRESENT  → ParentEnvelopeMissing (fail closed)
program_identity PRESENT + execution ABSENT        → program self-verification only
program_identity PRESENT + execution PRESENT       → claim mandatory AND matching
                                                     (ParentMissing / ParentMismatch)
```

A dangling claim — an execution envelope naming a parent id with no program
envelope in the lock to verify it against — is rejected in Phase 0
(`ParentEnvelopeMissing`). An "external reference" mode, where the claim
points at a program envelope stored in a registry rather than in the lock,
is a meaningful design but requires the Phase 1 registry to resolve and
verify against; it is deferred to the Phase 1 ADR, not silently permitted
now.

### 6–8. Lock integration, trust boundary, validation policy (unchanged from r3/r4)

`CapsuleLock.program_identity: Option<CapsuleProgramEnvelopeV1>` (the lock
type formerly named `AtoLock` — renamed per the `capsule.lock` amendment in
`CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §5);
`CanonicalLockProjection` untouched (`lock_id` unaffected);
`CanonicalSignatureProjection` gains the field. `verify_execution_boundary`
→ `verify_lock_trust_boundary` composing both verifications at the three
chokepoints. Fully absent (no envelope, no claim) → never blocks;
present+valid → correlation only, never runtime compatibility or selection;
present+invalid/mismatched, or an orphan claim (§5's
`ParentEnvelopeMissing`) → rejected at the trust boundary (same category as
any corrupt lock). Publication policy is Phase 1.

### 9. Fixtures — three suites, plus nested-boundary and conformance vectors (Major 4)

Under `crates/capsule/tests/fixtures/capsule_program_contract/`:

```text
contract/   CapsuleProgramContractV1 JSON → canonical JCS bytes → capsule_program_id
            (baseline, field-order, top-level mutation matrix, invalid/fail-closed)

manifest/   capsule.toml text → expected ProgramManifestIntentV1 JSON
            top-level:  excluded-field change ⇒ same IR; included-field change ⇒
                        different IR; alias ⇒ same IR; explicit-default ⇒ same IR;
                        unknown field ⇒ reject; workspace ⇒ reject
            nested:     build.lifecycle.build change ⇒ different IR
                        build.policy.require_attestation change ⇒ SAME IR (Rule 2)
                        build.outputs.attestation change ⇒ SAME IR (Rule 2)
                        exports.cli.<n>.description change ⇒ SAME IR (Rule 2)
                        targets.<l>.model_filename change ⇒ SAME IR (Rule 2)
                        targets.<l>.engine_path present ⇒ reject (Rule 3)
                        targets.<l>.model absolute ⇒ reject; relative in-tree existing ⇒ accept;
                          relative but nonexistent ⇒ reject (SourceExistingPath)
                        requirements.* change ⇒ different IR (declaration semantics, §0)
                        snapshot.* change ⇒ different IR (declaration semantics, §0)
            semantic types (Rule 4 matrix regression):
                        OCI working_dir = "/app" ⇒ accept, canonicalized as GuestPath
                        source working_dir = "packages/app" ⇒ accept, SourceRelativePath
                        wasm working_dir present ⇒ reject (Rule 3)
                        web static root entrypoint ⇒ canonical Root ("."), accept
                        storage.volumes[].mount_path = "/data" ⇒ accept, GuestPath
                        bindings.<n>.mount / context.mount ⇒ canonicalized as GuestPath
                        snapshot.warmup_paths = ["/", "/app"] ⇒ HttpRequestTarget
                        entrypoint naming a not-yet-built path ⇒ accept
                          (SourceRelativeFuturePath — lexical only)
                        structured [targets.source] vs the equivalent named target
                          ⇒ SAME normalized-target IR (canonicalization vector)
                        targets.source.dependencies = "requirements.txt" (existing)
                          ⇒ accept; nonexistent ⇒ reject (SourceExistingPath)
                        targets.oci.digest / targets.wasm.digest ⇒ CasContentDigest;
                          bare-hex spelling in those fields ⇒ reject
                        model_sha256 = "<64hex>" AND model_sha256 = "sha256:<64hex>"
                          ⇒ SAME Sha256DigestPin IR (both authoring spellings valid)
                        targets.source_digest = "sha256:<64hex>" ⇒ accept,
                          prefix-stripped Sha256DigestPin IR;
                          targets.source_digest = "<64hex>" ⇒ reject (existing
                          validator requires the prefix)
                        model_revision = 40-hex ⇒ GitCommitRevision; 64-hex ⇒ reject
                        targets.wasm.world omitted ⇒ default-expanded to
                          "wasi:cli/command", accept (WitWorldRef);
                          world = "uarc:v1/http-handler" ⇒ accept;
                          never validated as a target/map Identifier
                        targets.oci.user = "1000:1000" ⇒ accept
                          (ContainerUserSpec); named-target user and structured
                          OCI user canonicalize identically
                        model_repo_sha256 change ⇒ different IR;
                          model_repo_include[] change ⇒ different IR (GlobPattern)
                        readiness_probe.port = "web" ⇒ ProbePortReference (name,
                          not host:port); readiness_probe.tcp_connect = "db:5432"
                          ⇒ TcpProbeTarget — the two never interchange
                        contracts.<n>.ready per variant: Http.url / Tcp.target /
                          Probe.run / UnixSocket.path / Postgres.* with {{…}}
                          template expressions ⇒ TemplatedString, validated by the
                          existing dependency grammar, hashed as authored
            conformance: for each vector, ordinary-normalizer output and strict-
                        adapter output produce the SAME semantic IR
                        (§2.0.1 — catches drift between the two parsers)

source/     fixture tree → projected file set → expected source digest
            (resolved control files excluded; no-lock vs capsule.lock vs
             deprecated ato.lock.json => IDENTICAL digest; both lock names
             coexisting => reject before derivation; nested fixtures/capsule.lock,
             fixtures/ato.lock.json and
             examples/capsule.toml INCLUDED and digest-affecting — exact-path
             rule, no sniffing; control-file-shaped symlink rejected by the
             pre-filter A1 pass; executable-bit flip changes digest;
             fixed-point with/without root lock)
```

### 10. Scope

**Phase 0** (`ato` repo only): the types and single entrypoint above, the
v0.3 adapter (the future capsule.toml-v1 adapter is NOT in Phase 0), source
projection, lock field + signature projection + trust-boundary wiring, all
fixture suites. **Phase 1** (separate ADR): `ato-api` registry
column/index/query, publication-time enforcement, duplicate/collision
policy, second-language implementation consuming the Phase 0 fixtures.

### 11. Naming

**Capsule Program Identity** / `capsule_program_id`, defined normatively as
the *declaration* identity (§0). Retained for continuity and to avoid
ADR-012's `capsule_revisions` collision. The legacy inert
`capsule_manifest_hash` (`snapshot_manifest.rs`, capture provenance) is the
informal predecessor; never equal-by-construction, never conflated in
migration. "Program Source **Projection**," not "Closure" (A1v2 rejects
submodules/LFS rather than resolving them).

## Alternatives Considered

- **A: Do nothing** — cross-target declaration grouping stays inexpressible.
- **B: External review's full proposal** — reopens non-load-bearing
  machinery, duplicates terminology, colliding name. Rejected (r1 review).
- **C: Slice from resolved `ExecutionContractV1`** — target-specific facets
  leak in. Rejected (r1 review).
- **D: Project `CapsuleManifest` minus a denylist** — cannot pin a preimage
  (`execution` is skip_serializing; `distribution` is generated). Rejected
  (r2 review).
- **E: IR + adapter with classification finished "during implementation"** —
  an unfinished classification is not a preimage. Superseded (r3→r4).
- **F: r4's boundary with "program revision" semantics** — the preimage
  includes target definitions, execution requirements, snapshot intent, and
  unresolved dependency constraints, so "immutable program revision" and
  "target-independent" overclaimed what the hash identifies. Superseded
  (r4→r5) by the declaration definition.
- **G: Split authored intent into three identities** (program semantics /
  resolution & compatibility intent / snapshot derivation intent) —
  honest about the semantic layers, but reintroduces per-field
  bucket-assignment judgment across the entire manifest (which bucket does
  `isolation.allow_env` belong to? `transparency`? every future field?) —
  precisely the drift failure mode the complete classification exists to
  kill. **Deferred**, not rejected: a derived interface-level identity can
  be layered later without changing `ato.capsule-program/v1`.
- **H — chosen (r5, refined r6/r7)**: r4's mechanics with the declaration definition,
  complete nested exception/unsupported enumerations, the exact-path
  control-file rule, one entrypoint, normalizer conformance, and narrowed
  digest types.

## Consequences

- Good: the definition now matches the preimage — no gap between what the
  name promises and what the hash identifies; the declared-vs-resolved
  two-layer model is explicit and each layer has exactly one identity.
- Good: nested boundary closed by complete enumeration; local paths can no
  longer leak host-specific bytes into (or nondeterminism out of) the hash;
  the projection is a pure function of (tree, selected root) with no
  content sniffing; manifest and source can't be mixed across roots.
- Good: `lock_id` untouched; parent claim mandatory when both envelopes
  exist; honest claim scoping (association, not derivation).
- Bad (accepted): a declaration id changes on target/requirements/snapshot
  edits even when program code is untouched — correct under §0's
  definition, but consumers wanting "same code, any declaration" grouping
  need the deferred Option G layer.
- Bad (accepted): the same id can resolve to different concrete programs
  over time (unresolved constraints) — by design; Execution Identity is the
  resolved layer.
- Bad (accepted): Phase 0 refuses dirty working trees, `workspace`
  manifests, `engine_path`, and out-of-tree `model` paths — all fail
  closed rather than hashed ambiguously.
- Bad (accepted): no derivation proof in Phase 0; nested/multi-manifest
  trees are handled by per-Capsule selected roots, not by a single
  multi-manifest identity.

## Follow-up

### Spec edits — `docs/rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md`

As r3/r4 listed (§1 governing sentence + six-element model, §2.1, §3.1
cardinalities, §3.2 carve-out, new §3.4, §5, §7.5, §13, §15, §16.4,
terminology/migration, §17.6), with §3.4 carrying r5 content: the
declaration definition and its three stated consequences (§0), the complete
top-level table (§2.1), nested Rules 1–4 (§2.2), the exact-path control-file
rule and projection order (§1), the guarantee-scope table (§5), and the
validation-policy wording (§8). §13 additionally records: (a) the explicit
overturn of the earlier "requirements are outside the revision" framing,
and (b) that revisiting the semantic-IR decision (same id across authoring
schema versions) or any classification row requires a new
`ato.capsule-program/v1` version.

### Implementation — Rust (branch fresh from `origin/nightly` @ `f7ee059b`)

- **New** `crates/capsule/src/contract/capsule_program_contract.rs`:
  `CAPSULE_PROGRAM_V1_SCHEMA`, `CapsuleProgramId`,
  `VerifiedCapsuleProgramId` (+4 compile-fail doctests),
  `CapsuleProgramContractV1`, `ProgramSourceContract`,
  `ProgramSourceDigest` (sha256-only), `ProgramSourceProjectionSchemaV1`,
  the Rule-4 semantic types (`SourceRelativePath` with its `Root` variant,
  `SourceExistingPath`/`SourceRelativeFuturePath` validation modes,
  `HttpRequestTarget`, `TcpProbeTarget`, `ProbePortReference`,
  `GlobPattern`, `RemoteArtifactRef`, `Sha256DigestPin` (with per-field
  authoring-spelling normalization), `CasContentDigest`,
  `GitCommitRevision`, `WitWorldRef`, `ContainerUserSpec`,
  `TemplatedString` (reusing the existing dependency-grammar type),
  `OpaqueCommand`, `OpaqueAuthoredString` as a closed set),
  `ProgramManifestIntentV1` + `Normalized*Intent` facets,
  `CapsuleProgramEnvelopeV1`, `compute_capsule_program_id`,
  `CapsuleProgramLinkError` (incl. `ParentEnvelopeMissing`),
  `verify_program_parent(verified, execution)`, and the single public
  entrypoint `derive_capsule_program_contract`.
- **New** `crates/capsule/src/contract/program_source_projection.rs`: the
  five-step projection of §1; calls, never modifies,
  `materialized_source_tree_hash`.
- **New** `crates/capsule/src/contract/program_manifest_input.rs`:
  `ProgramManifestV03Input` (strict; `deny_unknown_fields` everywhere;
  custom deserializer for `ProgramTargetsV03Input`'s flatten pattern) +
  `program_intent_from_v03` adapter consuming post-normalization values per
  §2.0.1.
- `execution_contract.rs`: additive non-identity
  `capsule_program_id: Option<CapsuleProgramId>` on
  `ExecutionContractEnvelopeV1`.
- `capsule_lock/schema.rs` (renamed from `ato_lock/`): `program_identity:
  Option<CapsuleProgramEnvelopeV1>` on `CapsuleLock`.
- `capsule_lock/canonicalize.rs`: `program_identity` in
  `CanonicalSignatureProjection` only.
- `capsule_lock/execution.rs`: `verify_lock_program_identity` (the four-state
  matrix of §5, incl. the orphan-claim rejection; verified id minted once).
- `capsule_lock/mod.rs`: rename `verify_execution_boundary` →
  `verify_lock_trust_boundary`; compose both; call sites unchanged.
- **Fixtures**: the suites of §9, including the nested-boundary and
  conformance vectors.
- **Not touched**: `snapshot_manifest.rs` (`select_snapshots` — add the
  intentional-exclusion doc-comment), `foundation/blob/source_tree.rs`
  (frozen), `engine/execution_graph/*`, `engine/execution_plan/*`,
  `cli/commands/inspect.rs`.

### Verification

- `cargo check -p capsule -p cli -p snapshot-builder`; `cargo clippy … -D
  warnings`; `cargo fmt --all -- --check`.
- New module tests: contract mutation matrix (every §2.1 row), nested
  boundary vectors (every §2.2 Rule 2/Rule 3 row), parser-conformance
  suite, source-projection suite (fixed-point, nested-lock-included,
  examples/capsule.toml-included, executable-bit, symlink control file),
  compile-fail doctests.
- `capsule_lock` tests: the tamper × chokepoint matrix, now four scenarios ×
  three chokepoints (tampered id, `ParentMissing`, `ParentMismatch`,
  `ParentEnvelopeMissing` — the orphan claim); `lock_id` byte-identity
  with/without `program_identity`; signature changes when it is added;
  tampered `program_identity` fails signature verification; **and
  `execution_id` byte-identity before/after adding the
  `capsule_program_id` claim to an execution envelope** — the direct proof
  that the claim is a non-identity envelope field.
- `cargo test -p capsule --lib snapshot_manifest` — `select_snapshots`
  unchanged, `&VerifiedExecutionId`-only.
- Cross-check before merge: every §2.1 row and every §2.2 exception/
  unsupported row has a corresponding fixture vector; flag any gap.
