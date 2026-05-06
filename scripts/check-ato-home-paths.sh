#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PATTERN='dirs::home_dir\(|std::env::var\("HOME"\)'
ALLOWLIST='^(crates/capsule-core/src/foundation/common/paths.rs|crates/ato-desktop/src/settings.rs|crates/ato-desktop/src/cli_install.rs|crates/ato-cli/src/application/auth/storage.rs|crates/ato-cli/src/utils/local_input.rs|crates/ato-cli/src/app_control/session.rs|crates/capsule-core/src/adapters/capsule/cas_store.rs|crates/ato-cli/src/application/engine/install/support.rs|crates/ato-cli/src/adapters/registry/serve/registry_storage.rs|crates/ato-cli/src/adapters/registry/binding/proxy.rs|crates/ato-cli/src/cli/commands/uninstall.rs|crates/ato-cli/src/adapters/output/progressive/mod.rs|crates/ato-cli/src/application/engine/build/native_delivery/projection.rs|crates/ato-cli/src/adapters/registry/publish/upload_strategy/presigned.rs):'

MATCHES=$(rg -n "$PATTERN" crates/ato-cli/src crates/ato-desktop/src crates/capsule-core/src crates/ato-session-core/src || true)
MATCHES=$(printf '%s\n' "$MATCHES" | rg -v "$ALLOWLIST" || true)

if [ -n "$MATCHES" ]; then
    echo "Found HOME accessors outside the explicit allow-list:"
    echo "$MATCHES"
    exit 1
fi

echo "No unexpected HOME accessors found in product source."
