# Ato documentation

Start with the [project overview](../README.md), then follow the path matching
your task. English is the default for current documentation;
[README.ja.md](../README.ja.md) is the Japanese overview. Historical Japanese
or mixed-language sources are labelled in the archive catalogue.

## Reading paths

| Task | Read in order |
|---|---|
| Understand Ato | [Computation](concepts/computation.md) → [Capsule and Run](concepts/capsule.md) → [K, D and Source](concepts/application-contracts.md) → [glossary](glossary-reference.md) |
| Build and verify an app | [Getting started](guides/getting-started.md) → [Formation](guides/formation.md) → [Runner / Hosting](guides/runner-hosting.md) |
| Change the runtime | [Architecture](architecture/README.md) → [Specifications](specifications/README.md) → [Contributing](../CONTRIBUTING.md) |
| Operate a service | [Operations](operations/README.md) → the relevant update/rollback/provisioning runbook |

## Concepts

- [Computation](concepts/computation.md): residual work, Evolution, composition,
  Ports and projections.
- [Capsule and Run](concepts/capsule.md): immutable continuation and active evaluation.
- [Contract K, Derivation D and Source](concepts/application-contracts.md):
  profile-specific identity and explicit realization.
- [Formation, Verification and Receipt](concepts/formation-verification.md):
  frozen expectations, observations and evidence.
- [Instance, Run, Lease and State Continuity](concepts/state-continuity.md).
- [Compatibility, Continuous Patching and Derivation Transformation](concepts/compatibility.md).
- [Materialization](concepts/materialization.md), [execution identity](execution-identity.md),
  and the [canonical glossary](glossary-reference.md).

## Specifications

[Specifications](specifications/README.md) lists all eight accepted RFCs and
locates Protocol, wire, schema, Contract and Verification Receipt contracts.
[RFCs](rfcs/README.md) separates accepted, draft and historical documents.
An accepted RFC owns normative behavior; code describes implementation.
A conflict is a [Detected mismatch](rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md),
not permission to rewrite either side during documentation maintenance.

## Guides and architecture

- [Developer guides](guides/README.md): CLI, formation, transformation, Runner,
  hosting, contribution and component instructions.
- [Architecture](architecture/README.md): library boundaries, shared portable
  execution, Hosted Formation and Scenario verification.
- [Capsule bundle overview](capsule.md) and [computation lifecycle](run.md).
- [Snapshot materialization](snapshot.md) and [COOP](coop.md).
- [Theory draft](theory/capsule-process-model.md): research background, not an
  implementation or deployment promise.

## Operations

[Operations](operations/README.md) is the entry for current binary identity,
worker updates, Runner rollout/rollback and compiler provisioning. API/PWA
deployment and remote migrations stay with their owning service repositories.

## Archive and complete inventory

[Archive](archive/README.md) maps past models, reports, experiments and plans.
[2026](archive/2026/README.md) includes moved ops reports and retained evidence
trees. Old Markdown paths and heading anchors remain compatibility entry points.

The [inventory](archive/2026/documentation-reorganization/inventory.tsv) covers
all baseline docs attachments and repository-wide prose. The
[move list](archive/2026/documentation-reorganization/moves.tsv),
[retention assessment](archive/2026/documentation-reorganization/EVIDENCE-RETENTION.md)
and [audit report](archive/2026/documentation-reorganization/REPORT.md)
record classifications, duplicates, link checks and unresolved limitations.
The [acceptance review](archive/2026/documentation-reorganization/REVIEW-ACCEPTANCE.md)
checks the newcomer reading paths, actual first example, external references
and code CI separately.

<!-- Compatibility anchors from the previous documentation entry. -->
<a id="60-second-model"></a>
<a id="pick-a-reading-path"></a>
<a id="full-index"></a>
<a id="architecture"></a>
<a id="theory"></a>
<a id="implementation-and-protocol-details"></a>
<a id="historical-material"></a>

<!-- Additional Docsify slug aliases (including numeric prefixes). -->
<a id="_60-second-model"></a>
