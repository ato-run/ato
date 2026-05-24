# Desktop Install Lifecycle Detail — P4b Acceptance Smoke

Verify the read-only detail panel for installed apps in the Desktop shell.

## Prerequisites

- P4b branch (`feat/desktop-install-lifecycle-detail`) built
- `cargo build -p ato-desktop` completed
- `ato` CLI binary available (can be `cargo run -p ato-cli`)

## Hermetic run

```bash
export ATO_HOME=$(mktemp -d)
echo "ATO_HOME=$ATO_HOME"
```

## Flow

### 1. Install sample capsule

```bash
# tutorial-step1 is the simplest capsule (type=job, Python, no deps)
cargo run -p ato-cli -- install samples/tutorial/apps/tutorial-step1 -y --json
```

Expected: outputs `install_profile_key` (IPK). Save for later.

### 2. Launch Desktop

```bash
cargo run -p ato-desktop
```

### 3. Verify Installed Apps list

```
- Launcher panel shows "Installed Apps (1)" section
- App card shows publisher/slug "tutorial/tutorial-step1"
- App card shows version
- Install profile key (abbreviated IPK) visible on app card
- "Launch" button visible
- Running badge not shown (no session yet)
```

### 4. Click app card → detail panel

```
- Click the app card in the left column
- Right column shows detail panel with:
  - Publisher: tutorial
  - Slug: tutorial-step1
  - Capsule handle: tutorial/tutorial-step1
  - Installed app ID
  - Version: 1.0.0
  - Installed at timestamp
  - Updated at timestamp
  - Profile section with profile_id "default"
  - Install profile key (full)
  - Current revision ID
  - Revisions count: ≥1
```

### 5. Revision list markers

```
- Revision list shows at least one revision entry
- The current revision has a "Current" marker
- Non-current revisions (if any) do NOT have a "Current" marker
```

### 6. Pinned revision marker

```
- If any revision is pinned: "Pinned" marker visible
- Pinned marker distinct from "Current" marker
```

### 7. Profile switch

```
- If multiple profiles exist, clicking a different profile:
  - Detail panel updates to show that profile's info
  - Revision list updates to show that profile's revisions
  - IPK / current revision / revisions count all reflect the switched profile
```

### 8. Copy actions

```
- "Copy IPK" button copies install_profile_key to clipboard
- "Copy revision ID" button copies current_revision_id to clipboard
- "Copy output dir" button copies current_output_dir to clipboard
- Clipboard items are plain-text (ClipboardItem::new_string)
```

### 9. Refresh

```
- Click "Refresh" button in the detail panel or launcher
- DashboardCache::refresh() runs I/O on background
- app.refresh_windows() re-renders the UI
- If data changed externally (e.g. new install via CLI):
  - List updates to include new items
  - Detail panel reflects fresh data
```

### 10. Launch from detail / list

```
- Click "Launch" on the app
- ato launch <ipk> -y runs in background
- After launch completes + refresh:
  - Running badge (green dot) appears on the app card
```

### 11. Stop and verify badge removal

```bash
cargo run -p ato-cli -- stop --all
```

```
- After stop, click Refresh
- Running badge disappears
- Detail panel reflects stopped state
```

### 12. Error states

#### Corrupt revision_log.json

```bash
# Install an app, then corrupt its revision log
ATO_STORE="$ATO_HOME/instances"
# Find the revision_log and corrupt it
find "$ATO_STORE" -name "revision_log.json" -exec sh -c 'echo "corrupt" > "$1"' _ {} \;
```

```
- Open launcher / detail panel
- Error state shown instead of empty detail
- Message indicates "Unable to read <app>: parse revision log"
```

#### Corrupt artifact_manifest.json

```
- Similar corruption → error state
- "Unable to read revision data" message
```

### 13. Destructive UI verification

```
- No "Rollback" button visible
- No "Update" button visible
- No "GC" button visible
- No "Pin" / "Unpin" button visible
- No "Delete" / "Uninstall" button visible
- All copy actions are read-only (ClipboardItem::new_string)
```

### 14. Empty state (clean ATO_HOME)

```bash
export ATO_HOME=$(mktemp -d)
# Launch Desktop with fresh ATO_HOME, no app installed
```

```
- Launcher shows "No installed apps yet."
- Hints: "ato install <publisher>/<slug>"
- No error, no detail panel (no selection possible)
```

## Cleanup

```bash
ato_pid=$(lsof -ti :0..65535 | grep ato-desktop || true)
[ -n "$ato_pid" ] && kill "$ato_pid" 2>/dev/null || true
rm -rf "$ATO_HOME"
```

## Test Coverage

Run:
```bash
cargo test --lib install_lifecycle_dashboard -- --test-threads=1
cargo test -p ato-desktop installed_app_detail -- --nocapture
```

| Test | What it verifies |
|------|-----------------|
| installed_apps_ui_select_profile_when_app_id_is_none | Profile selection works without prior app |
| installed_apps_ui_select_profile_ignores_mismatched_app | Different app_id rejected |
| installed_apps_ui_select_profile_matching_app_succeeds | Same app_id updates profile |
| installed_apps_ui_select_app_clears_error | detail_error reset on new selection |
| resolve_selected_app_returns_selected | Selected app lookup by ID |
| resolve_selected_app_falls_back_to_first | No selection → first item |
| resolve_selected_app_returns_none_for_missing | Missing selected ID → None (no fallback) |
| resolve_selected_app_empty_returns_none | Empty items list → None |
| resolve_selected_profile_prefers_default | Default profile preference |
| resolve_selected_profile_falls_back_to_first_no_default | No default → first item |
| resolve_selected_profile_empty_returns_none | Empty profiles → None (no panic) |
