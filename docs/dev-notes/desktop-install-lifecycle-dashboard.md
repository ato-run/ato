# Desktop Install Lifecycle Dashboard — P4 Smoke

Verify the installed apps dashboard in the Desktop shell.

## Prerequisites

- P4 branch (`feat/desktop-install-lifecycle-dashboard`) built
- `cargo build` for ato-desktop completed
- At least one capsule installed via CLI

## Flow

### 1. Clean ATO_HOME

```bash
export ATO_HOME=$(mktemp -d)
echo $ATO_HOME
```

### 2. Install a capsule

```bash
cargo run -- install <publisher>/<slug> -y --json
# Note the install_profile_key from JSON output
IPK="<ipk_value>"
```

### 3. Launch Desktop

```bash
cargo run --bin ato-desktop
```

### 4. Verify Installed Apps list

```
- Launcher panel shows "Installed Apps (1)" section
- App card shows publisher/slug, version, profile info
- Install profile key (short) visible on app card
- Launch button visible per app
- Running badge invisible (no session yet)
```

### 5. Launch from Desktop

```
- Click "Launch" on the app card
- The Launch button triggers `ato launch <ipk> -y`
- After launch, refresh or re-open launcher
- Running badge (green dot) appears next to the app card
```

### 6. Verify session attachment

```
- In the installed apps list, running sessions show as green indicator
- Session info links to existing Open Windows / Card Switcher
```

### 7. Verify CLI integration

```bash
cargo run -- ps --json
# Verify session records contain install lifecycle fields
```

### 8. Empty state

```
- Fresh ATO_HOME with no installed apps
- Launcher shows "No installed apps yet." message
- Shows install hint: "ato install <publisher>/<slug>"
```

### 9. Cleanup

```bash
cargo run -- stop --all
rm -rf "$ATO_HOME"
```

## Error states

### Store unreadable

```
- If ATO_HOME/instances/ doesn't exist or is corrupted
- Launcher shows "Unable to read installed apps: <error>"
```

### Corrupt revision_log.json

```
- Install an app, corrupt its revision_log.json
- Re-open Desktop launcher
- Installed apps section shows error message instead of empty state
```

## Test Coverage

Register tests at `crates/ato-desktop/src/install_lifecycle_dashboard.rs`:

Run:
```bash
cargo test --lib install_lifecycle_dashboard -- --test-threads=1
```

| Test | What it verifies |
|------|-----------------|
| empty_store_returns_empty_vec | Fresh install returns empty list |
| single_app_returns_correct_item | App card with publisher/slug/version/ipk |
| get_app_detail_returns_full_detail | Detail query returns full profile info |
| list_revisions_returns_correct_count_and_current_marker | Revision list with current flag |
| current_revision_none_when_link_missing | Profile without symlink returns None |
| corrupt_revision_log_returns_err | Corrupt log → Err, not silent empty |
