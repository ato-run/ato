# Documentation reorganization report

Date: 2026-10-26. Baseline: dev
`0c93d81a7f86140868fbe84efb63538360a9cba1`.

The [new-developer acceptance review](REVIEW-ACCEPTANCE.md) records the
follow-up browser and executable-example checks, fixes to the initial PR,
external reference coverage, and the still-failing code CI gates. Read it
with this initial organization report.

## Inventory and disposition

[inventory.tsv](inventory.tsv) classifies 997 baseline files: all tracked docs
attachments plus repository-wide Markdown/reStructuredText/README prose.
It records Concepts, Specifications, Guides, Operations or Archive, status,
language, size, baseline digest, final destination and action. Architecture
explanations belong to Concepts. Draft specifications remain non-normative.
New navigation and audit artifacts are listed in [file-changes.tsv](file-changes.tsv).

The [plan](PLAN.md) was committed before relocation. Its refinement archives
five host runbooks after verifying that their `ato runner` commands are retired.
The final [move list](moves.tsv) contains 215 files:

- 5 current operating procedures to `docs/operations/`;
- 2 implementation explanations to `docs/architecture/`;
- 208 historical reports, plans and legacy guides to `docs/archive/2026/`.

An intermediate commit records all 215 original moves at 100% similarity before
link/content edits. Git history is preserved. Former Markdown paths retain
compatibility pages and the original heading fragments. Raw JSON, logs, images,
compressed evidence and the whole dev-consolidation evidence directory remain
at their original locations. No history was rewritten and no evidence was deleted.

## Canonical owners and duplication

| Information | Canonical owner after this change |
|---|---|
| vocabulary | [existing glossary](../../../glossary-reference.md) |
| explanation and profile boundaries | [Concepts](../../../concepts/application-contracts.md) |
| normative contracts | [eight accepted specifications](../../../specifications/README.md#accepted-specifications) |
| application wire/authoring/receipt implementation | same Rust code and draft contracts, indexed without promotion |
| local use and contribution | [Guides](../../../guides/README.md), root Contributing |
| operating procedures | [Operations](../../../operations/README.md) |
| past implementation/validation | [Archive](../README.md) and retained evidence collections |

The baseline SHA-256 duplicate scan found two intentional groups, listed in
[duplicates.json](duplicates.json): the two empty `.nojekyll` files support
separate docs entry trees, and two identical screenshots belong to different
validation runs. Both groups are retained; byte equality does not make their
provenance interchangeable. Overlapping concept definitions now point to the
glossary and profile explanation. Older Ready-State/recipe/manifest models stay
labelled as historical rather than being merged into the current model.

## Authority and unresolved questions

All eight accepted RFCs remain byte-identical to baseline. Their complete list
is in [Specifications](../../../specifications/README.md#accepted-specifications).
The separate [Detected mismatch draft](../../../rfcs/draft/DOCUMENTATION_AUTHORITY_MISMATCHES.md)
records six groups: profile identity, the public packaging lifecycle, initial
v3 scope versus extensions, draft/receipt authority, stale snapshot status and
retired/unverified operational instructions. Adoption and implementation fixes
need owner decisions outside this documentation PR.

Compatibility is documented as K preservation plus verified re-realization.
The text explicitly does not infer state continuity, safe effects, security
patches or rollout safety from K equality. “State Continuity Evidence” is an
explanatory evidence category, not a newly invented schema. Implemented code,
accepted specifications, design direction and deployment evidence are labelled
separately.

## Link verification

The reproducible [local auditor](../../../../scripts/docs-audit.py) checks
repository-wide Markdown inline/reference paths, directories, heading fragments
and explicit compatibility anchors while excluding fenced examples. Run from
the repository root:

```sh
python3 scripts/docs-audit.py --ref 0c93d81a7f86140868fbe84efb63538360a9cba1 \
  --output .tmp/docs-links-baseline.json
python3 scripts/docs-audit.py --output .tmp/docs-links-current.json
```

The tool exits nonzero when historical references are unresolved; this result
must not be reported as an all-repository clean pass. Baseline and final totals,
remaining references and validation limits are recorded in
[VALIDATION.json](VALIDATION.json) and [link-audit.json](link-audit.json).
The baseline contained 126 unresolved local references; the final audit has 33,
a reduction of 93. All current entry-point/guide/specification links resolve. Historical references
without an established replacement remain explicitly inventoried. Code paths
are not redirected to unrelated current files merely because basenames match.
The initial audit counted external URLs without fetching them. The follow-up
review inspected GitHub references and exercised the rendered Docsify paths.

The ECC docs-governance read-only audit was also run. It reports real legacy
Markdown failures plus heuristic warnings for inline examples, schema strings,
runtime-created paths and absent optional governance files. Those warnings do
not establish missing production behavior. No empty governance spine was added
just to satisfy default filenames.

## Sensitive data and evidence

The initial bounded text/archive pattern scan inspected 3,522 text files/members and
five tar.gz archives without extracting or executing them. Sixteen binary
files/members were outside text inspection. Seventeen pattern matches were
reviewed as historical example/default values or input-policy labels; no live
credential was established by this scan. No credential was tested against a
service. The OpenAI-shaped matches occur in a document that expressly labels
the values demo placeholders, rather than in the consolidation JSON.

[Sanitized scan metadata](security-review.json) records scope and archive limits.
Personal-data indicators are present in historical attribution/author metadata,
email examples, host addresses and local user paths. The 337-entry consolidation
inventory includes GitHub author metadata. These are not duplicated into this
report as values. The URL-token match is descriptive prose about scrubbing
query parameters, not a captured signed URL. Screenshots/PDF pixels were not
OCR-scanned; a pattern scan is not proof of absence of secrets or private data.
The archive should not be broadly republished without an access/redaction review.

The two consolidation JSON files remain byte-identical; sizes, digests, history,
inbound references and a recoverable storage proposal are in
[EVIDENCE-RETENTION.md](EVIDENCE-RETENTION.md). External storage migration and
history rewrite are not performed by this change.

## Delivery and integration

This is an independent documentation PR targeting dev. Review is required
before integration. Main promotion of this documentation is a separate release
action. Main already contained release #1588 at the recorded baseline and dev
contained its back-sync #1589; neither proves this new documentation is on main.

No application behavior, wire/schema, accepted RFC, deployment, migration or
feature flag is changed. The initial organization pass did not run application builds or smokes. The
acceptance follow-up built the CLI and verified the corrected first example;
its scope and the code CI failures are recorded in the linked review.
