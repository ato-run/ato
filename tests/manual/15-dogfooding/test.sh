#!/bin/bash
# =============================================================================
# §15 リリース前 dogfooding
# All items are human checklists — no automated tests
# =============================================================================
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../config.sh"
RESULT_FILE="$RESULTS_DIR/result_15_dogfooding.log"
: > "$RESULT_FILE"

SUITE="§15 Pre-Release Dogfooding"
echo "══════════════════════════════════"
echo " $SUITE"
echo "══════════════════════════════════"
check_ato

# ---------------------------------------------------------------------------
# Human: internal team encap + share URL cross-machine
# ---------------------------------------------------------------------------
test_team_encap_share() {
    checklist "Team encap → share URL → colleague run" \
        "Each team member takes their own real project and runs: ato encap ." \
        "After publish, copy the share URL (https://ato.run/s/<id>)" \
        "Send the URL to a colleague who has ato installed on a DIFFERENT machine" \
        "Colleague runs: ato run <url> — confirm it works without extra steps" \
        "Repeat with at least one cross-OS pair (e.g., Mac sender → Linux receiver)" \
        "Note and file any friction points encountered"
}

# ---------------------------------------------------------------------------
# Human: 1-week dogfooding sprint
# ---------------------------------------------------------------------------
test_one_week_sprint() {
    checklist "1-week 'ato run only' development sprint" \
        "Each engineer runs their daily development workflow using only 'ato run'" \
        "No direct invocation of python/node/docker/cargo outside of ato" \
        "Track and file every friction point (small or large) as a GitHub issue" \
        "At end of week, hold a retro: what worked, what was painful, what blocked progress" \
        "Sprint outcome: list of P0/P1 issues that must be fixed before release"
}

# ---------------------------------------------------------------------------
# Human: Llama 3.1 8B demo recording
# ---------------------------------------------------------------------------
test_llama_demo_recording() {
    checklist "Record Llama 3.1 8B local chat demo from zero (marketing-quality)" \
        "Start with a CLEAN machine — no ato installed, no model cached" \
        "Install ato using the official install script (curl ato.run/install.sh | sh)" \
        "Run the documented 'Llama 3.1 8B local chat' capsule — no undocumented steps" \
        "Measure and record actual elapsed time from install command to model first response" \
        "Target: ≤5 minutes total on M2 Mac with good internet" \
        "Record screen video: this becomes a marketing asset for the blog post" \
        "If time goal not met, identify the bottleneck and file a performance issue"
}

# ---------------------------------------------------------------------------
# Human: all sample capsules run by a real developer
# ---------------------------------------------------------------------------
test_sample_capsules_dogfood() {
    checklist "All sample capsules run by a fresh-eyes developer" \
        "Hand the samples/ directory to a developer who did NOT write them" \
        "They should be able to run every sample using only: ato run <dir>" \
        "No tribal knowledge, no Slack pings, no README corrections allowed" \
        "File a bug for every sample that requires outside assistance to run" \
        "Goal: every sample works 'out of the box' for a new developer"
}

# ---------------------------------------------------------------------------
# Human: T-30 internal metrics
# ---------------------------------------------------------------------------
test_t30_metrics() {
    checklist "T-30 internal dogfooding — metrics to capture" \
        "Time-to-first-success (install → first capsule run) per developer" \
        "Number of support questions / Slack pings in the first 24 hours" \
        "Number of bugs filed per day during the sprint" \
        "Coverage: at least one run on Mac Intel, Mac ARM, Ubuntu, Windows" \
        "Coverage: at least one run with GPU capsule, one without" \
        "Coverage: at least one share-URL cross-machine flow" \
        "All P0 bugs resolved before T-14"
}

# ---------------------------------------------------------------------------
# Human: external alpha tester coordination
# ---------------------------------------------------------------------------
test_external_alpha_testers() {
    checklist "External alpha tester program (10 testers, T-14 window)" \
        "Identify 10 external testers — diverse OS/GPU/network backgrounds" \
        "Include at least 2 testers on corporate networks with proxies" \
        "Include at least 2 testers on slow/mobile internet" \
        "Include at least 1 tester on Windows 10 and 1 on Ubuntu LTS" \
        "Testers run §1 (install), §2 (GPU if available), §6 (share URL end-to-end)" \
        "Testers run §10 (error messages) — intentionally break manifests and report quality" \
        "Collect structured feedback: what confused them, what was missing, what failed" \
        "File all issues before T-7"
}

test_team_encap_share
test_one_week_sprint
test_llama_demo_recording
test_sample_capsules_dogfood
test_t30_metrics
test_external_alpha_testers

print_suite_summary "$SUITE"
