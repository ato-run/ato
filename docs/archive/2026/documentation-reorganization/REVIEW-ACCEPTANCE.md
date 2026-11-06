# New-developer acceptance review

Date: 2026-10-26. Reviewed base: dev
`0c93d81a7f86140868fbe84efb63538360a9cba1`; initial PR head:
`e5ebda3a605c775b650066457246fa8d617bc1e0`. Follow-up changes remain in the
independent [documentation PR #1591](https://github.com/ato-run/ato/pull/1591).

## Acceptance criterion and findings

A developer encountering Ato for the first time must be able to start at the
root README, understand the concepts, distinguish normative specifications
from implemented drafts, and reach an executable first example and current
authoring instructions. File counts alone do not establish this outcome.

The second review found two actual onboarding defects in the initial PR:

1. The first `pack` command used an `ato.capsule/1` source, while the current
   public frontend accepts `ato.capsule/2`. Executing the documented command
   reproduced the parser failure. The corrected first example runs the
   supplied static bundle. A separate authoring step packages the current
   v2 process example and explains its runtime and explicit-D prerequisites.
2. Git-valid relative paths were incorrectly routed by Docsify's disabled
   relative-path setting. Following the concept page's “Computation model”
   produced a real 404; the “project overview” link returned the docs index
   instead of the root README. Relative routing is now enabled, shared sidebar
   routes are absolute within the site, out-of-docs links go to repository
   files, and JSON/TSV artifacts are served as files. Table contrast was also
   corrected for the dark theme.

The specification index now warns at the accepted-spec entry that unqualified
identity/lifecycle language conflicts with later application-profile code.
Application implementation contracts remain explicitly Draft. None of the
eight accepted RFCs was rewritten to remove that conflict. Contributing no
longer establishes legacy `dev` as the universal default while the workspace
is introducing the `stg` integration flow.

## Paths exercised

| Reading path | Verification |
|---|---|
| README → Concepts / Architecture / Specifications / Getting Started / Formation / Runner / Contributing / Operations | repository links checked; current targets exist |
| docs index → K/D/Source → Computation | browser navigation; correct concept content, no 404 |
| Specifications → Computation Architecture | browser navigation; `Status: Accepted` visible |
| Getting Started → Formation → Authoring v2 | browser navigation; `Status: draft implementation contract` visible |
| nested page → main sidebar destinations | site-root URLs remain independent of the current page |
| receipt implementation and sample manifest | rendered links resolve to the corresponding repository source |
| issue-linked old ops URL → archived original | browser navigation through the compatibility page to preserved content |

The CLI was built in this worktree with `cargo build -p ato-cli --bin ato
--locked`. In a fresh isolated test environment, the corrected README command
ran `samples/interop-static.capsule`: two observations were satisfied and the
receipt reported `fully_satisfied=true`. Packaging
`samples/portable-multi-process-authored` also succeeded. This is macOS local
evidence; no process-route/GPU/OCI or hosted acceptance is inferred from it.

The site's renderer has eight regression cases plus shared-sidebar and README
checks in `scripts/docs-site-check.mjs`. Documentation CI separately runs
those checks and rejects all current broken links and any historical failure
not present in the reviewed 33-occurrence baseline. Existing Rust/release
workflows are unchanged; the documentation check does not turn those failures
green or waive them.

## 1. Compatibility with references outside the repository

GitHub search was paginated through all 129 matches for `docs/ops/` in this
repository's indexed issue/PR bodies and conversation comments. In total,
174 bodies/comments were inspected. Of 152 path-reference occurrences, 104
point to moved documents: 85 distinct moved paths across 95 discussion/comment
sources. Every one retains its old path. The matched moved references did not
contain heading or line fragments. Sanitized per-reference evidence is in
[external-references.json](external-references.json).

Examples include [issue #1023](https://github.com/ato-run/ato/issues/1023),
which names the former staging-builder runbook, and
[PR #1546](https://github.com/ato-run/ato/pull/1546), which names the patching
ledger. The public Pages staging-builder URL was checked in a browser before
deployment; the same path was checked against the proposed docs preview and
its archive link was followed successfully.

All 215 former Markdown resources returned HTTP 200 from the docs-only preview
and matched the local compatibility-page bytes. All 1,195 original
GitHub-style heading/explicit anchors remain present. Additional aliases cover
Docsify heading forms such as numeric prefixes and collapsed hyphens.

This is bounded evidence, not an assertion that every external website has
been indexed or every private link can be discovered. Existing commit-pinned
URLs retain their immutable content. Branch URLs acquire these compatibility
pages when that branch integrates the PR. Arbitrary line-number links and
third-party mirrors cannot be guaranteed by retaining Markdown headings. No
issue/comment content or third-party site was rewritten.

## 2. Remaining historical references

The 33 remaining occurrences are the same reviewed failures, in 13 historical
documents. No current reading-path document has one. Each affected page now
has an explicit historical notice and links back to current specification
status. The full destinations remain in [link-audit.json](link-audit.json);
the following table identifies the owning documents.

| Historical collection/document | Occurrences |
|---|---:|
| Desktop tab design | 3 |
| legacy secrets architecture ADR | 1 |
| legacy Nacelle terminal spec | 1 |
| legacy run/install semantics | 2 |
| v0.5 readiness dashboard | 5 |
| monorepo consolidation plan | 1 |
| runtime-boundary research | 6 |
| Vikunja/Memos/LibreTranslate research | 7 |
| sandbox best-practices research | 1 |
| Ready-State backend matrix | 1 |
| archived Desky spec | 1 |
| archived Store Web spec | 1 |
| historical Simple TODO sample README | 3 |

Readers can deliberately inspect history, but it is not a prerequisite for the
current concept → specification → implementation paths. The CI allow-list is
the exact reviewed failures, not an exemption for arbitrary future archive
links.

## 3. Authority

The accepted-spec table, application Draft table, per-RFC status, glossary
qualifications and the separate Detected mismatch draft were checked together.
The current first example is application-profile behavior, and does not claim
that the eight computation-root RFCs formally adopt that profile. K equality
is not described as proof of state continuity, effect safety or security.

## 4. Change safety

For all 215 relocated bodies, comparison with the baseline found no unexpected
text differences after removing the added status/language banners, ignoring
Markdown target relocation and normalizing whitespace. The original pure-move
commit remains available. Other intentional changes are navigation/concept
explanations, corrected examples, historical notices, site routing and
documentation validation. No Rust implementation, accepted RFC or runtime
fixture was changed. All 314 other non-Markdown docs files remain byte-identical;
`docs/index.html` is the one intentional non-Markdown correction.

Sensitive-pattern review was repeated for the follow-up. No credential values
are included in this report or the new reference evidence. Historical demo
defaults and attribution/host metadata remain subject to the limitations in
the [retention assessment](EVIDENCE-RETENTION.md). A pattern scan does not
certify image pixels or arbitrary personal data as clean.

## CI and release gate

The initial PR's failing Rust CI was inspected by job and step. It is not a
billing-only failure. `cargo check -p ato-runtime-attempt --tests --locked`
failed on both the documentation head and an isolated checkout of the dev
base, with the same missing `StateAttachmentV1.backing` fields at
`ephemeral.rs:993` and `state_bindings.rs:210`.

The Ubuntu CLI scenario receipt-search failure and Windows Formation test
abnormal termination are already tracked in
[issue #1578](https://github.com/ato-run/ato/issues/1578). The macOS Activity MCP
test also failed in this PR's CI; its cause remains unresolved. Passing Linux
transformation/scenario-specific steps does not replace the failed workspace
or CLI jobs. See [CI evidence](ci-review.json) for job links and exact scope.

The requested merge is conditional on review and CI. A successful documentation
check alone does not satisfy the failing code gates. No merge/admin bypass or
production deployment is performed by this review.

[Release #1588](https://github.com/ato-run/ato/pull/1588) was already merged at
`5df2f36d2f1d672b89fceb0e1fbb368a9835a12b`; new commits cannot be appended to
that completed release. A subsequent candidate needs a new release PR under
the current integration policy, with exact code/contract/migration and staging
evidence. Post-merge dev validation remains pending until #1591 can merge.
