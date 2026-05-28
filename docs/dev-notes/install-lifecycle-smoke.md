# Install Lifecycle P3 Acceptance — CLI Smoke

Verify the install lifecycle end-to-end on a real CLI path.

## Prerequisites

- Ato dev branch checkout (PRs #222, #226, #227, #229, #231 merged)
- `cargo build` completed
- A sample capsule installed (any capsule works; use `ato install <handle>`)

## Flow

### 1. Clean ATO_HOME

```bash
export ATO_HOME=$(mktemp -d)
echo "Using ATO_HOME=$ATO_HOME"
```

### 2. Install a capsule

```bash
cargo run -- install <publisher>/<slug> -y --json
# Capture the install_profile_key from JSON output (ipk_<32hex>)
```

### 3. Launch the capsule

```bash
IPK="ipk_<value_from_step2>"
cargo run -- launch "$IPK" -y --verbose
```

### 4. Check session records

```bash
cargo run -- ps --json
# Verify that session records contain install_lifecycle fields:
#   install_profile_key, install_revision_id, installed_app_id, install_profile_id
```

### 5. List revisions

```bash
cargo run -- revisions "$IPK" --json
# Should show * marker on current revision
# After initial install: 1 revision, is_current=true
```

### 6. Update the capsule

```bash
cargo run -- update "$IPK" -y --json
# After update: revision count should increase (or "already up to date")
```

### 7. Verify revisions after update

```bash
cargo run -- revisions "$IPK" --json
# Should show multiple revisions, current marked with *
```

### 8. Rollback

```bash
cargo run -- rollback "$IPK"
# Should print "Rolled back from <old> → <new>"
```

### 9. Launch after rollback

```bash
cargo run -- launch "$IPK" -y --verbose
# Verify it uses the rollback target revision
```

### 10. GC dry run

```bash
cargo run -- gc --dry-run --json
# Verify reclaimable/protected/deleted lists
# Current revision and keep-last revisions must be in protected
```

### 11. GC with active session protection

```bash
# Before stopping: launch any capsule, then run GC
cargo run -- gc --dry-run --json
# Active session's install_revision_id must be in protected
```

### 12. GC execution

```bash
cargo run -- gc --json
# Verify deleted revisions match dry-run reclaimable
# Current, session-referenced, pinned, keep-last must survive
```

### 13. Cleanup

```bash
cargo run -- stop --all
rm -rf "$ATO_HOME"
```

## Error Boundary Tests

### Corrupt revision_log.json → GC aborts

```bash
# Locate the revision_log.json for a profile
LOG_PATH="<ATO_HOME>/instances/<app_id>/profiles/<profile_id>/revision_log.json"
echo "garbage" > "$LOG_PATH"
cargo run -- gc 2>&1
# Expected: error with "parse revision log" context
# Expected: no revisions deleted
```

### Corrupt session record → GC aborts

```bash
# Locate session root
SESSION_ROOT="<ATO_HOME>/ato-desktop/sessions"
mkdir -p "$SESSION_ROOT"
echo "garbage" > "$SESSION_ROOT/broken.json"
cargo run -- gc 2>&1
# Expected: error with "parse session record" context
# Expected: no revisions deleted
```

### Current revision deletion refused

```bash
# GC never deletes current_revision. Verify:
cargo run -- gc --keep-last 0 --retention-days 0 --dry-run --json
# current_revision must be in protected list, never in reclaimable
```

## Test Coverage

The integration test file `crates/ato-cli/tests/install_lifecycle_acceptance.rs` covers:

| Test | What it verifies |
|------|-----------------|
| `full_lifecycle` | install → rollback → GC → verify surviving revisions |
| `gc_current_always_protected` | current revision survives keep_last=0 |
| `gc_keep_last_protection` | keep-last N protects last N per profile |
| `gc_respects_pin` | .pinned marker prevents collection |
| `gc_respects_retention_window` | finalized_at within retention protects |
| `gc_protects_active_session_revision` | live process PID protects its revision |
| `gc_errs_on_corrupt_revision_log` | corrupt log JSON aborts GC |
| `gc_errs_on_corrupt_artifact_manifest` | corrupt manifest JSON aborts GC |
| `delete_revision_removes_revision` | non-current revision deleted from disk |
| `delete_revision_refuses_current` | current revision delete returns Err |
| `gc_protects_all_profile_currents` | all profiles' current_revs protected |
| `gc_keep_last_scoped_per_profile` | keep-last is per profile, not global |
| `rollback_auto_picks_predecessor` | rollback without explicit rev picks prev |
| `rollback_noop_same_revision` | set_current to same rev is no-op |
| `pin_unpin_cycle` | pin → is_pinned → unpin → not pinned |
| `ipk_stable_across_recreates` | IPK is deterministic |
| `ipk_stable_across_revisions` | IPK unchanged across revision additions |
| `gc_nothing_to_collect_single_rev` | single revision is never reclaimable |
| `gc_does_not_delete_protected` | explicitly protected revs never returned |
| `gc_protects_current_even_when_not_in_log` | ghost current rev still protected |

Run: `cargo test -p ato-cli --test install_lifecycle_acceptance -- --test-threads=1`
