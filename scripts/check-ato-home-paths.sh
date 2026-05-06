#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PATTERN='(?:dirs::home_dir|home_dir|std::env::var\("HOME"\))\([^\n]*\)(?s:.{0,120})(join\("\.ato"\)|join\("\.ato/"\))'

MATCHES=$(rg -nUP "$PATTERN" crates/ato-cli/src crates/ato-desktop/src crates/capsule-core/src crates/ato-session-core/src \
    --glob '!crates/capsule-core/src/foundation/common/paths.rs' || true)

if [ -n "$MATCHES" ]; then
    echo "Found direct HOME -> .ato path derivations that bypass canonical helpers:"
    echo "$MATCHES"
    exit 1
fi

echo "No direct HOME -> .ato path derivations found in product source."
