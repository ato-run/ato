# Evidence retention assessment

Audit date: 2026-10-26. No raw evidence was removed, rewritten, uploaded to a
new service, or removed from Git history by this change.

## Consolidation evidence

The directory `docs/ops/dev-consolidation-20261026/` is retained at its original
path. It is an Archive collection, including its Japanese/mixed-language prose.

| File | Bytes | SHA-256 |
|---|---:|---|
| `inventory-evidence.json` | 3,038,308 | `c9f97b0ce58b6deb2e68c041547fb058c58ff2a1270e4c7e88f33d8832ee51df` |
| `validation-evidence.json` | 102,467 | `ca84161907fa2137fb8f44046a06b64889c492edce6429c838019971acf5fe0b` |

The inventory is a 337-entry PR snapshot with repository, number, head/base
SHAs, branch names, author metadata, changed paths, ancestry/classification and
decisions. It is not an executable fixture or normative schema. The validation
object has 20 sections for rollout, cleanup, source replay, portable/native/state
observations and unresolved issues. These are dated observations, not current
deployment truth.

Git history records introduction and follow-ups in `5e4721b07`, `d4873f0f0`,
`a1b5a96fe`, `5aeb940a5` and `132a6c115`. The neighbouring
[PR inventory](../../../ops/dev-consolidation-20261026/PR-INVENTORY.md),
[validation report](../../../ops/dev-consolidation-20261026/VALIDATION.md) and
[changed-files report](../../../ops/dev-consolidation-20261026/CHANGED-FILES.md)
provide the human-readable context. No existing runtime/test dependency on
these two JSON paths was found by repository reference search. Their historical
filenames and embedded code/document paths still have audit value; the move
catalogue preserves how renamed documents map to them.

## Evaluation

Three megabytes is manageable as a retained snapshot. Repeated full inventories
and large binary/raw test logs will grow clone/history cost without improving
normal documentation. There is no verified durable external location in this
task. Keeping these exact bytes in Git now preserves availability and audit
links; deleting them or rewriting history would not meet that requirement.

For future runs, retain in Git a small English summary and manifest naming
each artifact's SHA-256, size, media type, producing repository/commit,
attempt or workflow identifiers, collection time, redaction method and storage
location. Store raw evidence in an access-controlled immutable object store or
a release artifact with explicit retention and backups. Ephemeral CI artifact
URLs alone are insufficient unless expiry and durable mirroring are addressed.

## Before externalizing an existing artifact

1. Select the owner, approved storage and retention/access policy. Preserve
   original bytes and provenance; use a separate redacted derivative for wider
   sharing when necessary.
2. Upload by content digest; verify download SHA-256/size and a restoration
   drill from a clean environment. Record version/immutability and backup
   information. Signed expiring URLs must not be the permanent reference.
3. Inventory inbound repository and known external links; keep the current
   path as a manifest/index explaining retrieval. Keep small receipts and
   failure summaries in Git so the claim can be reviewed without raw logs.
4. Review personal data, capability URLs and credential exposure before
   changing access. Encryption and access restriction do not replace redaction
   of a public derivative.
5. Only after owner review and verified recovery consider removing redundant
   working-tree copies. Preserve history by default; any history rewrite or
   migration to LFS needs a separate reviewed change and coordination.

The [audit report](REPORT.md) records sensitive-pattern inspection and its
limits. Existing screenshots and archives are evidence, not proof that every
private datum has been removed.
