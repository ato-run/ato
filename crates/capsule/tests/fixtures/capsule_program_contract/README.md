# `ato.capsule-program/v1` shared test vectors (ADR-014 §9)

Cross-language fixtures for Capsule Program Identity, indexed by
`manifest.json` and verified by
`crates/capsule/tests/capsule_program_vectors.rs`. Regenerate with the
`#[ignore]`d writer:

```sh
cargo test -p capsule --test gen_capsule_program_vectors -- --ignored --exact regenerate_shared_vectors
```

Three suites:

- `contract/` — `CapsuleProgramContractV1` JSON → canonical JCS bytes →
  `capsule_program_id` (baseline, field-order, mutations, fail-closed
  vectors, envelope verification).
- `manifest/` — `capsule.toml` text → expected `ProgramManifestIntentV1`
  JSON via the real pipeline (`load_manifest` → `program_intent_from_v03`).
  Vectors sharing one `expected_file` are equivalent authored spellings of
  the same declaration; `expect=error` vectors pin an error substring. Side
  files some vectors need (`model.gguf`) are materialized by the harness,
  not committed.
- `source/` — committed fixture tree → projected file set → expected
  `ProgramSourceDigest`. Each `source/vectors/<name>/` directory is a real
  tree; its index entry records `projected_files` (the sorted paths a second
  implementation must hash — not only the digest) and `source_digest`.
  `relation` is measured against `source_baseline`.

## `source/` vectors

| vector | pins |
|---|---|
| `baseline` | manifest + two source files, no lock: the root `capsule.toml` is excluded, everything else is hashed |
| `with-canonical-lock` | baseline's source bytes + a root `capsule.lock` ⇒ the SAME digest (the fixed point: the resolved lock never reaches the preimage) |
| `with-deprecated-alias-lock` | baseline's source bytes + a root `ato.lock.json` ⇒ the SAME digest (the id survives the lock-file rename) |
| `nested-control-names` | baseline's source bytes + `examples/capsule.toml` + `fixtures/capsule.lock` ⇒ a DIFFERENT digest, with both nested files IN the projected set — exclusion is by exact path at the selected root, never by file name or content sniffing |
| `nested-dir-tree` | recursion and the A1 child-ordering rule, with sibling names that sort across the `/` boundary (`a.txt`, `a/`, `ab/`) |

Portability rules these fixtures follow, so a fresh checkout on any platform
reproduces the recorded digests:

- **Regular files only** — no symlinks, no directories that would need to be
  empty (git does not track empty directories, so every fixture directory
  holds at least one file).
- **No meaningful executable bit** — the A1 file digest includes a mode byte
  (`1` iff the executable bit is set), so every fixture file is committed
  non-executable.
- **No end-of-line translation** — `source/.gitattributes` marks the whole
  suite `-text`; the digest is over the bytes, and a CRLF checkout would
  change it.

### Deliberately NOT committed here

Two source-projection scenarios from ADR-014 §9 stay out of this tree because
they cannot be expressed as portable committed files — a symlink does not
survive a checkout on every platform, and the executable bit is exactly what
those vectors must control. They are covered by tempdir-built unit tests in
`crates/capsule/src/contract/program_source_projection.rs`:

- control-file-shaped symlink rejected by the pre-filter A1 pass —
  `symlink_named_capsule_lock_rejected_by_admissibility_pass`
- executable-bit flip changes the digest —
  `executable_bit_flip_changes_projection_digest`, and the staging copy that
  must preserve it — `staged_copy_preserves_executable_bit`

The same module also covers the fail-closed cases that are the ABSENCE of a
tree rather than a tree (`rejects_coexisting_lock_names_at_root`,
`directory_under_the_lock_name_is_rejected`, `missing_root_manifest_is_rejected`,
`root_level_git_is_not_a_pinned_materialization`). This is a documented
boundary, not a gap: everything a second implementation can check from
committed bytes alone is in `source/`.
