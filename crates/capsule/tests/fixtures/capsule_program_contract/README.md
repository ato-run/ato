# `ato.capsule-program/v1` shared test vectors (ADR-014 §9)

Cross-language fixtures for Capsule Program Identity, indexed by
`manifest.json` and verified by
`crates/capsule/tests/capsule_program_vectors.rs`. Regenerate with the
`#[ignore]`d writer:

```sh
cargo test -p capsule --test gen_capsule_program_vectors -- --ignored --exact regenerate_shared_vectors
```

Two suites:

- `contract/` — `CapsuleProgramContractV1` JSON → canonical JCS bytes →
  `capsule_program_id` (baseline, field-order, mutations, fail-closed
  vectors, envelope verification).
- `manifest/` — `capsule.toml` text → expected `ProgramManifestIntentV1`
  JSON via the real pipeline (`load_manifest` → `program_intent_from_v03`).
  Vectors sharing one `expected_file` are equivalent authored spellings of
  the same declaration; `expect=error` vectors pin an error substring. Side
  files some vectors need (`model.gguf`) are materialized by the harness,
  not committed.

The third ADR-014 §9 suite (`source/` — fixture tree → projected file set →
source digest) is deliberately NOT committed here: committed symlink and
executable-bit fixtures are not portable across platforms/VCS checkouts, so
those scenarios live as tempdir-built unit tests in
`crates/capsule/src/contract/program_source_projection.rs`.
