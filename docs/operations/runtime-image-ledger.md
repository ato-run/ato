# Runtime image ledger and patch-candidate validator

## Status

Phase-2 foundation. This change adds a read-only tool
(`tools/runtime-image-ledger`) and a versioned inventory format
(`ato.runtime-image-inventory/1`). It does **not** change the execution path,
`lib/formation`, or `ato-api`; it adds no CI wiring and writes nothing to D1.

> **This tool validates DECLARED evidence. It does not scan, sign, or verify an
> image.** Every `sbom`, `scan`, `provenance`, and `receipt` value in an
> inventory file is an assertion written by an operator. The tool checks that
> those assertions are internally consistent and that a status which claims
> verified execution actually carries a receipt summary. It never pulls an
> image, reads a layer, runs a scanner, checks a signature, or accepts a
> Contract receipt.

## Why this exists

Runtime execution images are pinned in several places, but no reviewed
inventory of them exists:

| Source | Pin location today |
|---|---|
| Hosted run OCI route declared by an app author | `capsule.toml` `runtimes["oci.image"]` → `BoundDerivation.runtimes` → `DerivationRef` |
| Formation source-to-OCI base images | `SourceOciRecipe.base_images[].pinned_digest` (`lib/formation/src/source_oci_plan.rs`) |
| Generated `ato-source/…@sha256:…` image | embedded in the bundle; named in the execution receipt |
| Hosted runner host/VM image | runner provisioner configuration (ato-api) |
| Python/Node runtime versions used by Formation | constant catalog in `lib/formation/src/intent.rs` |

At the time of writing, neither this repository nor ato-api generates SBOMs,
runs image vulnerability scans, signs images, or records SLSA provenance for
them. The closest prior art is the single-observation file
`docs/ops/formation-oci-candidates-registry-2026-09-30.json`: it records digests
and platform manifests, is never read at runtime, and is not updated
automatically. The same gap is recorded in
`docs/ops/formation-source-oci-d2.md`: images are digest-pinned, but "the bytes
behind this digest were audited" is not recorded anywhere.

This ledger is the reviewed half of the two-layer design:

| Layer | Where | What |
|---|---|---|
| Policy / allowlist (human-reviewed) | this repository, a versioned `*.json` file | digest, base digests, expected runtime versions, declared SBOM/scan/provenance refs, K/D refs, status, waiver |
| Observed facts (append-only) | ato-api D1 (planned) | which attempt verified which digest with which receipt, when |

The ato repository is where reviewers can see and approve a policy change. D1
is where runtime observations accumulate. This PR adds only the first layer and
the validator for it; see [Planned D1 index mapping](#planned-d1-index-mapping).

## Non-goals

- **No scanning.** The tool does not invoke trivy, grype, osv-scanner, or any
  other scanner. A `scan` record is only as trustworthy as the operator who
  wrote it.
- **No SBOM generation or reading.** `sbom.digest` is a declared content digest,
  not a file the tool opens.
- **No signing or signature verification.** `provenance` records what the
  operator says exists (sigstore bundle, Ed25519, registry, or `none`). Nothing
  here checks a certificate or a Rekor entry.
- **No image pull or registry check.** The tool never confirms that
  `image_digest` is still resolvable or that its bytes match.
- **No Contract K evaluation.** It never parses observations, never calls the
  verifier, and never decides whether a route satisfies K. That remains
  `accept_receipt` / `accept_verified_route`.
- **No automatic rebuild, rollout, or quarantine.** `decision: auto_build` is a
  declaration for a later phase; this PR has no executor.
- **No D1 write and no CI wiring in this PR.** Running the validator is a
  manual/review step for now.

## Inventory schema

Schema id: `ato.runtime-image-inventory/1`.

All structs use `deny_unknown_fields`; unknown fields, unknown enum variants,
and missing required fields fail parsing (exit 1). A full example is
[`tools/runtime-image-ledger/fixtures/valid-inventory.json`](../../tools/runtime-image-ledger/fixtures/valid-inventory.json).

```json
{
  "schema": "ato.runtime-image-inventory/1",
  "images": [
    {
      "image_digest": "sha256:1111…64 lowercase hex…",
      "base_digests": ["sha256:2222…"],
      "runtime_versions": {"node": "22.23.4", "python": "3.12.7"},
      "created_from": "formation",
      "sbom": {"format": "spdx-json", "digest": "sha256:3333…"},
      "scan": {
        "tool": "trivy 0.60.0",
        "status": "findings",
        "checked_at": "2026-10-20T03:12:04Z",
        "advisory_refs": ["GHSA-aaaa-bbbb-cccc"]
      },
      "provenance": {"kind": "sigstore", "ref": "bundle:…"},
      "derivation_ref": "sha256:4444…",
      "contract_ref": "sha256:5555…",
      "status": "active",
      "receipt": {
        "receipt_ref": "sha256:6666…",
        "contract_ref": "sha256:5555…",
        "derivation_ref": "sha256:4444…",
        "fully_satisfied": true,
        "observed_at": "2026-10-21T09:00:00Z"
      },
      "findings_waiver": {
        "reason": "advisory not reachable from the declared route",
        "approved_by": "koh0920",
        "expires_at": "2099-12-31T00:00:00Z"
      }
    }
  ],
  "patch_candidates": [
    {
      "from_digest": "sha256:1111…",
      "to_digest": "sha256:7777…",
      "advisories": ["GHSA-aaaa-bbbb-cccc"],
      "rebuild": {
        "derivation_ref": "sha256:8888…",
        "contract_ref": "sha256:5555…",
        "receipt_ref": "sha256:9999…",
        "fully_satisfied": true
      },
      "decision": "auto_build"
    }
  ]
}
```

### `images[]`

| Field | Required | Meaning |
|---|---|---|
| `image_digest` | yes | `sha256:` + 64 lowercase hex; unique within the file |
| `base_digests` | yes | Declared parent/base image digests; they do not have to be listed in `images[]` |
| `runtime_versions` | yes | Name → exact `x.y.z`. Ranges (`^`, `~`, `>=`), wildcards, tags, and `latest` are rejected |
| `created_from` | yes | `formation` (source-to-OCI lane), `author` (declared in `capsule.toml`), `runner_host` (host/VM image) |
| `sbom` | no | `{format: spdx-json \| cyclonedx-json, digest}` |
| `scan` | no | `{tool, status: passed \| findings \| not_run, checked_at, advisory_refs[]}` |
| `provenance` | no | `{kind: sigstore \| ed25519 \| registry \| none, ref}` |
| `derivation_ref` | yes | The D this image and receipt belong to |
| `contract_ref` | yes | The K this image is expected to prove |
| `status` | yes | `candidate`, `verified`, `active`, `retired`, `quarantined` |
| `receipt` | no | Summary of the latest accepted `ContractVerificationReceipt` |
| `findings_waiver` | no | Accepted risk for `active` + `findings`; `expires_at` is checked against the clock |

### `patch_candidates[]`

| Field | Required | Meaning |
|---|---|---|
| `from_digest` | yes | Existing affected image; must appear in `images[]` |
| `to_digest` | yes | Rebuilt image; must appear in `images[]` and differ from `from_digest` |
| `advisories` | yes | Non-empty list of advisories motivating the rebuild |
| `rebuild` | yes | `{derivation_ref, contract_ref, receipt_ref, fully_satisfied}` for the rebuilt image |
| `new_contract` | no (default `false`) | Set `true` only when the rebuild intentionally moves to a new Contract K |
| `decision` | yes | `auto_build`, `manual`, or `blocked` |

### Timestamps

Declared instants are UTC RFC 3339 with a `Z` designator:
`YYYY-MM-DDTHH:MM:SSZ`, optionally with fractional seconds (`.123456`, 1–9
digits). `T`/`Z` may be lowercase. Offsets such as `+00:00` are rejected so
ledger files have one canonical form. The parser is hand-written in
`src/rfc3339.rs`; the crate deliberately has no `chrono` dependency. Leap
seconds are not accepted.

## Validation rules and stable codes

The validator collects **all** findings, not just the first. Each finding names
the code, the subject (`<image digest>`, `patch_candidates[i]`, or `inventory`),
and the JSON field. Exit status is 1 when any finding exists.

| Rule | Code(s) |
|---|---|
| Schema must be `ato.runtime-image-inventory/1` | `schema_unsupported` |
| Digests are `sha256:` + 64 lowercase hex | `image_digest_invalid`, `base_digest_invalid`, `sbom_digest_invalid`, `patch_digest_invalid` |
| Image digests are unique | `image_digest_duplicate` |
| Runtime versions are exact `x.y.z` | `runtime_version_not_exact` |
| Declared timestamps parse | `scan_checked_at_invalid`, `receipt_observed_at_invalid`, `waiver_expires_at_invalid` |
| `verified` / `active` carry a receipt with `fully_satisfied: true` | `receipt_required`, `receipt_not_fully_satisfied` |
| Receipt `derivation_ref` matches the image record (for `verified` / `active`) | `receipt_derivation_mismatch` |
| Receipt `contract_ref` matches the image record (at any status) | `receipt_contract_mismatch` |
| `active` + `scan.status = findings` carries a non-expired waiver | `waiver_required`, `waiver_expired` |
| Patch references exist and are not self-referential | `patch_reference_unknown`, `patch_self_reference` |
| Patch advisories are non-empty | `patch_advisories_empty`, `patch_advisory_empty` |
| Patch rebuild keeps the from-image K unless `new_contract: true` | `patch_contract_mismatch` |
| `auto_build` requires `rebuild.fully_satisfied: true` | `patch_auto_build_not_satisfied` |
| Parsing fails closed (unknown fields/variants, malformed JSON) | `inventory_json_invalid` |

Notes on intent:

- An expired waiver is only invalid where a waiver is required: `active` with
  `scan.status = findings`. Historical `retired` records may keep an expired
  waiver.
- A `receipt_contract_mismatch` is invalid at every status. An image record
  names exactly one K; a rebuild that moves to a new K is a new patch candidate
  with `new_contract: true`, not a receipt attached to the old image's K.
- `new_contract: true` skips only the from-image K equality check. It does not
  prove the new K differs, and it does not waive the rules for the `to_digest`
  image record itself.

## CLI

```text
ato-runtime-image-ledger validate <inventory.json> [--quiet]
ato-runtime-image-ledger report   <inventory.json> [--json] [--quiet]

0  inventory is valid
1  findings are present, or the inventory cannot be parsed
2  usage error, or the inventory file cannot be read
```

- `validate` prints each finding to stderr and (unless `--quiet`) a one-line
  summary to stdout.
- `report` prints a table with the declared status, scan/SBOM/provenance
  presence, and receipt `fully_satisfied` per image, then each patch candidate
  with its decision. Absent evidence is printed as `missing`, never as a
  passing value. With `--json` it emits the declarations, the findings, and the
  SHA-256 of the inventory file (file identity only, not image verification).
  `report` also exits 1 when findings exist.
- `--quiet` suppresses stdout; findings always go to stderr.
- `--json` is valid only for `report` and cannot be combined with `--quiet`.

Example:

```console
$ cargo run -p ato-runtime-image-ledger -- \
    validate tools/runtime-image-ledger/fixtures/valid-inventory.json
valid: 2 image(s), 1 patch candidate(s) [ato.runtime-image-inventory/1]

$ cargo run -p ato-runtime-image-ledger -- \
    validate tools/runtime-image-ledger/fixtures/waiver-missing.json
waiver_required: sha256:259e…: images[0].findings_waiver: status active with scan.status=findings requires a findings_waiver
```

## Adding an image

1. Pick the image digest (`sha256:…`) actually used by the declared route or
   runner. Do not write a tag.
2. Record the K and D it is expected to prove. Take them from the existing
   runtime/route data (`verified_routes`, `formation_results`, or the bundle
   metadata); do not compute or "fix up" K here.
3. Record the receipt summary from the latest accepted receipt: `receipt_ref`,
   `contract_ref`, `derivation_ref`, `fully_satisfied`, `observed_at`. If no
   receipt exists, leave `receipt` out and use status `candidate`.
4. Add declared SBOM/scan/provenance references only if those artifacts really
   exist. If they do not, omit the fields; `report` will show `missing`.
5. Set `status`. `verified`/`active` require a fully satisfied matching
   receipt. `active` with `scan.status = findings` requires a waiver with a
   future `expires_at`.
6. Run the validator and commit the file change for review. The review is the
   approval step: the tool cannot tell whether the operator actually ran the
   scanner.

## Adding a patch candidate

1. Add the rebuilt image as its own `images[]` record first (`to_digest`), with
   the D/K evidence produced by the rebuild.
2. Add `patch_candidates[]` with `from_digest`, `to_digest`, the advisories that
   motivate the rebuild, and the `rebuild` summary.
3. Keep `rebuild.contract_ref` equal to the from-image's `contract_ref`. Only
   set `new_contract: true` when the rebuild deliberately changes K; then the
   `to_digest` record carries the new K and its own receipt evidence.
4. Choose `decision`:
   - `auto_build` — safe to queue automatically, but only with
     `rebuild.fully_satisfied: true`;
   - `manual` — rebuild/review by hand;
   - `blocked` — do not proceed (for example, unresolved K change or missing
     evidence).
5. Run `validate` and commit. This PR does not start the rebuild; a later phase
   connects the decision to the existing source-patch → Formation path.

## Contract K authority stays in Rust

The inventory's `receipt` is a **summary**. It exists so a reviewer can see
which K/D the declared evidence names and whether it was fully satisfied. It
is not a second receipt schema and not a second judge.

- The only acceptance functions are `accept_receipt` and
  `accept_verified_route` in `lib/formation/src/receipt.rs` (with
  `ContractVerificationReceipt` defined in `lib/formation/src/verify.rs`).
- ato-api calls that same Rust authority through WASM
  (`apps/ato-api/src/services/runtime_network/receipt_authority.ts` in the
  ato-api repository). TypeScript routes and stores results; it must not
  re-evaluate K.
- This tool only compares declared strings (`contract_ref`, `derivation_ref`,
  `fully_satisfied`) against the image record. It does not read observations,
  does not verify attempt ids, and does not certify the receipt.
- If a future change wants "is this receipt still valid for this image", call
  the existing authority; do not add a third implementation here or in
  TypeScript.

## Evidence limits and residual risks

- **Self-declared evidence.** A wrong or bad-faith operator can write
  `scan.status: passed` without running a scanner. The validator cannot detect
  it. This is the reason the file is reviewed in version control and the reason
  the docs and report say "declared" everywhere.
- **No image pull.** Digest availability, tag movement, and byte equality are
  not checked.
- **No signature/provenance verification.** `provenance.ref` is opaque; `none`
  is allowed and remains visible in the report.
- **Receipt summary drift.** `observed_at`, `receipt_ref`, and `attempt_id`
  (not recorded here) are not cross-checked against D1 in this PR. A later
  read-only API can compare the summary with the stored receipt.
- **No CI wiring.** Nothing runs the validator automatically yet, so a stale or
  inconsistent inventory is only caught when someone runs it.
- **Timestamps are UTC-only** and omit leap seconds; existing records with
  `+00:00` offsets (for example the 2026-09-30 candidate registry, a different
  schema) are not accepted in this schema.
- **Refs are opaque.** `contract_ref`, `derivation_ref`, and `receipt_ref` are
  not format-validated to avoid creating a second ref-syntax validator; the
  Contract/formation authority owns ref construction.
- **`new_contract: true` is a declaration**, not proof that K changed.
- **The `to_digest` image record is validated independently**, so a patch
  candidate cannot hide a missing receipt on the rebuilt image by declaring
  `auto_build`.

## Planned D1 index mapping

Not part of this PR. The two layers stay separate because ato-api Workers
cannot read this repository:

- The reviewed file (this repository) remains the policy/allowlist source of
  truth.
- ato-api D1 records observed facts and the latest receipt per digest. Only one
  of the following shapes should be added later, not both:
  - `runtime_images(image_digest PK, base_digests_json, runtime_versions_json,
    sbom_artifact_id, scan_artifact_id, provenance_json, derivation_ref,
    contract_ref, status, updated_at)`, or
  - `runtime_image_verifications(id, image_digest, contract_ref,
    derivation_ref, attempt_id, fully_satisfied, receipt_ref, observed_at)`
    plus an index on `image_digest` (the existing `receipts` table has no
    image-level index and `verified_routes` is indexed by route).

Mapping from this schema:

| Inventory field | D1 destination (planned) |
|---|---|
| `image_digest` | `runtime_images.image_digest` / `runtime_image_verifications.image_digest` |
| `base_digests`, `runtime_versions` | JSON columns |
| `sbom.digest`, `scan`, `provenance` | artifact ids / observation JSON once producers exist |
| `derivation_ref`, `contract_ref` | verifications row |
| `receipt.receipt_ref`, `fully_satisfied`, `observed_at` | verifications row joined to `formation_results` / `verified_routes` |
| `status`, `findings_waiver` | policy columns, but approval stays in the reviewed file until a migration/route exists |

Any future write path must append observations and receipts from the existing
runtime paths. It must not copy this repository's file into D1 as if the tool
had verified anything, and it must not add a TS Contract judge.

## Fixtures

`tools/runtime-image-ledger/fixtures/` contains one valid inventory (an old
affected image with findings and a waiver, plus a verified patched image and an
`auto_build` candidate) and one negative fixture per rule. See
[the fixture README](../../tools/runtime-image-ledger/fixtures/README.md) for the
fixture → code table. The CLI integration test runs every fixture as the exact
binary, so the documented codes cannot drift silently.
