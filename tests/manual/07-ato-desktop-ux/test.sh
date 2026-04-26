#!/bin/bash
# =============================================================================
# §7 ato-desktop の実 UX — GUI は全項目が人手確認
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_07_ato_desktop_ux.log"
: > "$RESULT_FILE"

SUITE="§7 ato-desktop UX"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"

# ---------------------------------------------------------------------------
# Automated: ato-desktop binary exists and starts (smoke test)
# ---------------------------------------------------------------------------
test_desktop_binary_exists() {
    if command -v ato-desktop &>/dev/null; then
        pass "ato-desktop binary found in PATH"
    else
        local build_path="$SCRIPT_DIR/../../../apps/ato-desktop/target/debug/ato-desktop"
        if [ -x "$build_path" ]; then
            pass "ato-desktop binary found at $build_path"
        else
            skip "ato-desktop binary not found — build first: cd apps/ato-desktop && cargo build"
        fi
    fi
}

# ---------------------------------------------------------------------------
# Automated: ato-desktop --version returns something
# ---------------------------------------------------------------------------
test_desktop_version() {
    local binary="ato-desktop"
    command -v ato-desktop &>/dev/null || \
        binary="$SCRIPT_DIR/../../../apps/ato-desktop/target/debug/ato-desktop"
    if [ ! -x "$binary" ] && ! command -v ato-desktop &>/dev/null; then
        skip "ato-desktop binary not available"
        return
    fi
    local out
    out=$(timeout 5 "$binary" --version 2>&1 || true)
    if echo "$out" | grep -qE '[0-9]+\.[0-9]+'; then
        pass "ato-desktop --version: $out"
    else
        fail "ato-desktop --version" "Unexpected output: $out"
    fi
}

# ---------------------------------------------------------------------------
# Human checks
# ---------------------------------------------------------------------------
test_first_launch_onboarding() {
    checklist "First launch onboarding experience" \
        "On a clean machine (no previous ato-desktop config), launch ato-desktop" \
        "Onboarding screen appears and explains what ato-desktop does" \
        "All action items in onboarding are clickable and functional" \
        "Onboarding can be dismissed and re-opened" \
        "After onboarding, the main window shows a usable default state"
}

test_macos_gatekeeper() {
    if [ "$(uname)" != "Darwin" ]; then skip "macOS Gatekeeper check (macOS-only)"; return; fi
    checklist "macOS Gatekeeper and notarization" \
        "Download the ato-desktop .dmg from the release page" \
        "Open the .dmg — no 'unidentified developer' warning (must be notarized)" \
        "Launch ato-desktop from Applications — no quarantine prompt" \
        "System Preferences → Privacy & Security shows ato-desktop as 'App Store and identified developers'"
}

test_windows_smartscreen() {
    checklist "Windows SmartScreen" \
        "Download ato-desktop installer from the release page on Windows" \
        "Run installer — SmartScreen does NOT block (requires valid Authenticode signature)" \
        "If SmartScreen appears, 'Run anyway' option is visible (for unsigned builds)" \
        "After install, launching ato-desktop from Start Menu works without warnings"
}

test_dock_tray() {
    checklist "Dock / Taskbar / System Tray behavior" \
        "Launch ato-desktop — icon appears in macOS Dock / Windows Taskbar" \
        "Click X to close main window — app remains in system tray (if designed to)" \
        "Click tray icon → window reappears" \
        "Right-click tray icon → Quit actually terminates the process"
}

test_dark_light_mode() {
    checklist "Dark/Light mode switch" \
        "Launch ato-desktop in light mode" \
        "Switch macOS/Windows to dark mode via system settings" \
        "ato-desktop GPUI chrome switches to dark theme immediately" \
        "WebView panels inside capsules also switch to dark mode" \
        "Switch back to light mode — both chrome and WebViews follow"
}

test_display_management() {
    checklist "External display plug/unplug" \
        "Open ato-desktop with a capsule on an external monitor" \
        "Unplug the external monitor — window moves to primary display" \
        "Re-plug external monitor — window can be moved back" \
        "Test with different DPI monitors: Retina (2x) and standard (1x)" \
        "On mixed-DPI setup, capsule WebView renders at correct DPI (no blur)"
}

test_webview_crash() {
    checklist "WebView crash recovery" \
        "Open a capsule with a WebView panel" \
        "In ato-desktop DevTools (if available) or via JS: window.close() or throw a fatal error" \
        "Alternatively, kill the capsule WebView renderer process from Activity Monitor" \
        "ato-desktop shows an error state in the panel, NOT a full application crash" \
        "The parent GPUI shell remains responsive" \
        "Option to reload the crashed WebView is offered"
}

test_memory_leak_24h() {
    checklist "Memory leak — 24-hour run" \
        "Launch ato-desktop with one idle capsule" \
        "Record memory usage (Activity Monitor / top): initial RSS" \
        "Leave running for 24 hours without interaction" \
        "Check memory again — RSS growth is < 50MB (no significant leak)" \
        "No UI degradation (animations still smooth, no frozen frames)"
}

test_multi_capsule_concurrent() {
    checklist "Multiple capsules running simultaneously" \
        "Launch capsule A (e.g. a Python server)" \
        "Launch capsule B (e.g. a Node.js server) at the same time" \
        "Both capsule windows are responsive" \
        "Switching between capsule panels is instant (<100ms)" \
        "Stopping capsule A does not affect capsule B" \
        "CPU/memory usage scales linearly (no combinatorial explosion)"
}

test_self_update() {
    checklist "ato-desktop self-update" \
        "While a capsule is running in ato-desktop, trigger a self-update of ato-desktop" \
        "Confirm ato-desktop informs the user before restarting (not a silent kill)" \
        "Running capsule is paused or cleanly stopped before ato-desktop restarts" \
        "After restart, capsule can be re-launched" \
        "No capsule data or state is lost due to the update"
}

test_desktop_binary_exists
test_desktop_version
test_first_launch_onboarding
test_macos_gatekeeper
test_windows_smartscreen
test_dock_tray
test_dark_light_mode
test_display_management
test_webview_crash
test_memory_leak_24h
test_multi_capsule_concurrent
test_self_update

print_suite_summary "$SUITE"
