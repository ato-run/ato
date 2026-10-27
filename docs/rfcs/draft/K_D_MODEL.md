# K/D Model — normative relations (A4 spec)

Status: **draft** (for review; no implementation change)
Owner: Ato core model
Applies to: Capsule / Contract identity, Derivation, Release, Receipt, Instance,
SCE, Projection, Managed Secrets authorization.

This document fixes RELATIONS and SCOPE only. It does **not** change any
existing hash, identifier or public Contract meaning.

## 1. Definitions

| Term | Relation |
|---|---|
| **Application** | A continuing identity for listing/authoring/usage statistics. Not the Contract, not a Run. A portable Application is identified by `application_ref`. |
| **Release** | A specific published content snapshot of an Application. A Release names the Contract and the approved set of Derivations D. |
| **Identity K** | The immutable Contract that determines Capsule identity (the acceptance condition for reproduction). K equality is necessary but **not sufficient** for Application identity, permission or State continuity. |
| **D (Derivation)** | A candidate realization path of Identity K. D is not part of K. A Contract may have several D; the runtime selects one explicitly and records it. |
| **Additional feature Contract** | A contract that additionally verifies the capability of a **specific D/Release** (e.g. an embedding relay authority, a network policy). It is evaluated per D/Release and does not by itself change Capsule identity. |
| **Effective K** | The existing composed-Contract meaning: the Contract set actually exercised by a Run. It is a composition of Identity K plus the verification contracts the Run is evaluated against. |
| **Verification Profile** | A named bundle of additional verification (Additional feature Contracts + their fixtures/targets) applied to a D/Release **without changing Identity K**. Selected per Release/Run and recorded in the Receipt. Not a Contract and not an identity. |
| **Receipt** | Evidence bound to (Contract set, D, execution attempt/conditions, Verifier version). Not a Run; does not define identity. |
| **Instance** | The user's data + execution continuity. Independent of D. |
| **SCE** | Evidence that pre/post-update State is the same user state, bound to the snapshot generation and Receipt. |
| **Projection** | A regenerable representation for search/recommendation/category inference. Not identity. |
| **Managed Secrets Permit** | An independent **authorization policy** (Application + approved Bundle/D + principal + purpose/environment scope + budget). Not a Contract and not a feature Contract: it answers "may this Run spend operator cost/credentials?", not "does the feature work?". |

## 2. Critical rule: when does an additional condition become a new K?

- If an additional feature Contract only **verifies** a capability of a specific
  D/Release, Identity K is unchanged; the result is recorded as an Additional
  feature Contract / Verification Profile result in the Receipt.
- If an additional condition is made a **required condition of Capsule validity**
  (a Capsule that lacks it is not the same Capsule), it MUST be treated as a
  **new Identity K**, with a new Contract, a new CapsuleId and a new Release.
  There is no "same Identity K with a mandatory extra condition".
- Effective K is the composition actually exercised; it does not blur the
  Identity K boundary. A Receipt must state which Contract set (Identity K +
  Verification Profile + Additional feature Contracts) it was evaluated under.

## 3. Root profile scope

- **computation-root v2**: wire v2; `index.root` is a `ComputationRef`; it is
  not re-read as a ContractRef.
- **contract-root v3/v4**: `profile = "ato.portable-application/1"`;
  `CapsuleId = root_contract_ref`; the root is recomputed from canonical
  `BoundContract` bytes. v3/v4 select D explicitly. `bundle_sha256` is
  byte-identical transport evidence, never semantic identity.

## 4. Base K and Effective K

- **Base K**: the Contract as sealed by the author/Release. Never rewritten at
  Run time; the runtime does not re-capture K.
- **Effective K**: Base K + the Additional feature Contracts and Verification
  Profile the Run is evaluated against. A verification composition, not a new
  identity and not a hash change.
- Rule: an additional feature contract may REFUSE a Run; it may not silently
  weaken Base K. Receipts must state the contract set they were evaluated under.

## 5. Release-approved D set

- A Release records the **approved set** of D (sorted `DerivationRef`s) it
  permits. The set is not part of Identity K; D order/success does not change K.
- A Run selects a D from the approved set explicitly; no implicit
  planner/fallback. A new D is added by a new Release (or by the #1573
  candidate-generation/adoption flow), never by rewriting K.

## 6. Receipt environment applicability and expiry

- A Receipt is valid only for the environment/conditions it was produced under
  (target, Runner/adapter, platform, Verifier version, execution conditions).
- A Receipt becomes **inapplicable** (not "false") when the target/conditions
  differ, the Verifier version is unsupported, the D or Contract set differs, or
  its validity window ends. Expiry never rewrites the Receipt.
- A Run acceptance requires a Receipt bound to the current attempt.

## 7. Invariants

1. Identity K is immutable; K equality is necessary but not sufficient for
   Application identity, permission or State continuity.
2. D is a realization path; changing D does not change Identity K.
3. Instance identity and State continuity are independent of D.
4. Existing hashes/identifiers/public Contract meanings are unchanged.
5. Ambiguous authority (e.g. a K-only operator credential) fails closed.
6. A mandatory additional condition is a new Identity K, never a silent
   Effective-K extension.
7. A Managed Secrets Permit is authorization, not evidence of function; it
   never changes Identity K or a Verification Profile.

## 8. Non-goals

- No change to any existing hash, identifier or public Contract meaning.
- No new Verifier Registry or composite-application support in this document.
- No production migration in this document.

## 9. Undecided (needs review)

- The concrete representation of a Verification Profile (named registry entry
  vs inline set) and whether it is pinned by Release or by Run.
- Whether Effective K is represented as a derived digest or only as a
  receipt-side contract-set descriptor (no identity change either way).
- Whether Release-approved D sets live on the Release row or in a separate
  `release_derivations` table.
- The exact expiry policy for long-lived Receipts (time window vs
  environment/verifier invalidation only).
- The generation identity for Projection (publicationVersion vs input digest).
