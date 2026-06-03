# Pre-existing test failures (not caused by this branch)

Running `cargo test --bin ato-desktop settings::` shows 4 failures that exist on
`dev` **before** this branch's changes. Verified by `git stash` + re-run on base:

- base `dev`:  `21 passed; 4 failed`
- this branch: `22 passed; 4 failed`  (the +1 is the new passing
  `snapshot_exposes_runtime_podman_enabled`)

Failing tests (all pre-existing):

- `settings::tests::snapshot_from_config_includes_desktop_section`
  — asserts `desktop.get("focusViewEnabled").is_some()`, but commit
  `c39851bf refactor(desktop): remove legacy DesktopShell and
  focus_view_enabled flag` (already in dev) removed that field without updating
  the assertion. Unrelated to runtime setup.
- `settings::tests::secrets_snapshot_storage_metadata_present`
- `settings::tests::secrets_snapshot_keys_have_metadata`
- `settings::tests::secrets_snapshot_grants_normalized`
  — secrets-snapshot tests, untouched by this branch.

This branch's only `settings.rs` change is additive (`resolved.runtime.podmanEnabled`
plus its test), which cannot affect the desktop-section or secrets snapshots.
