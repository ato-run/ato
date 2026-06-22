#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

PATTERN='dirs::home_dir\(|std::env::var\("HOME"\)'
ALLOWLIST='^(crates/capsule/src/foundation/common/paths.rs|crates/desktop/src/settings.rs|crates/desktop/src/cli_install.rs|crates/cli/src/application/auth/storage.rs|crates/cli/src/utils/local_input.rs|crates/cli/src/app_control/session.rs|crates/capsule/src/adapters/capsule/cas_store.rs|crates/cli/src/application/engine/install/support.rs|crates/cli/src/adapters/registry/serve/registry_storage.rs|crates/cli/src/adapters/registry/binding/proxy.rs|crates/cli/src/cli/commands/uninstall.rs|crates/cli/src/adapters/output/progressive/mod.rs|crates/cli/src/application/engine/build/native_delivery/projection.rs|crates/cli/src/adapters/registry/publish/upload_strategy/presigned.rs):'

MATCHES=$(rg -n "$PATTERN" crates/cli/src crates/desktop/src crates/capsule/src || true)
MATCHES=$(printf '%s\n' "$MATCHES" | rg -v "$ALLOWLIST" || true)

if [ -n "$MATCHES" ]; then
    echo "Found HOME accessors outside the explicit allow-list:"
    echo "$MATCHES"
    exit 1
fi

echo "No unexpected HOME accessors found in product source."
