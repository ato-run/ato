# Ato documentation

Start with the [project overview](../README.md), then follow the path matching
your task. English is the default for current documentation;
[README.ja.md](../README.ja.md) is the Japanese overview. Historical Japanese
or mixed-language sources are labelled in the archive catalogue.

## Documentation ownership

Two surfaces own different kinds of documentation:

| Surface | Owns |
|---|---|
| [ato.run documentation](https://ato.run/docs/) | General user documentation: [Overview](https://ato.run/docs/), [What is Ato](https://ato.run/docs/what-is-ato/), [Using apps](https://ato.run/docs/using-apps/), [Library](https://ato.run/docs/library/), [CLI quickstart](https://ato.run/docs/cli-quickstart/), [Core concepts](https://ato.run/docs/core-concepts/), [Capsule](https://ato.run/docs/capsule/) and [Use with AI](https://ato.run/docs/use-with-ai/). |
| This repository (GitHub) | Architecture, RFC/ADR, protocol and implementation specifications, technical design rationale, operations and the archive. |

Pages that exist on both surfaces are split by purpose. [run.md](run.md),
[capsule.md](capsule.md) and the [concepts](concepts/computation.md) pages here
are technical references; the general explanation is owned by the ato.run page
each of them links to. Minimal build, test and contribution steps stay in
[README](../README.md) and [CONTRIBUTING](../CONTRIBUTING.md).

Roadmap and work tracking live in [GitHub Issues](https://github.com/ato-run/ato/issues)
(and GitHub Projects when a board is used), not in a Markdown roadmap file.

## Document status

Status is carried by a `Status:` line in the file; where a page has none, the
directory decides.

| Status | Meaning | Where |
|---|---|---|
| Current | Written against the repository today. It is not a guarantee of implementation, deployment or verification on any environment. | `docs/` root technical pages, `concepts/`, `guides/`, `operations/`, `architecture/`, `specifications/`, `rfcs/accepted/` |
| Draft | Non-normative proposal or work in progress; not an implementation or deployment promise. | `rfcs/draft/` |
| Archived | Historical evidence: past reports, plans, experiments or retired procedures. It does not certify current state. | `archive/`, plus the retained collections listed in [archive/2026/README.md](archive/2026/README.md) |

`Current` means "written against the current tree". Deployment state, feature
flags and measurements are separate claims, and accepted RFCs remain the
authority for normative behavior.

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

The [2026 repository cleanup record](archive/2026/repository-cleanup-20261028/REPORT.md)
documents the 2026-10-28 ownership and status pass. Compatibility redirect pages
under `ops/`, `specs/` and the `docs/` root, and the historical collections in
[archive/2026/README.md](archive/2026/README.md), are intentionally retained:
they keep external issue/PR references (including `docs/...` links quoted in
old issues and PRs) resolving, and scripts and tests still read some of them.

GitHub Pages hosting is retired: the repository-side workflow and site assets
are removed, and the GitHub-side disable, 133-deployment purge and
`github-pages` environment deletion were completed on 2026-10-28
([result record](archive/2026/repository-cleanup-20261028/GITHUB-SETTINGS.md)).
The former `ato-run.github.io/ato/` URLs now return 404; that external link
loss is accepted and cannot be redirected from this repository. A few
historical links remain unresolved by design, while in-repository links stay
valid and are checked by docs CI.

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
