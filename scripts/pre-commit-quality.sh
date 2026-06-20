#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

echo "==> [root] cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> [root] cargo check --workspace --all-targets"
cargo check --workspace --all-targets

echo "==> [root] cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings \
    -A clippy::await_holding_lock \
    -A clippy::collapsible_if

# ato-desktop is an independent workspace (it carries its own empty
# `[workspace]` table), so the `--workspace` runs above do NOT cover it.
# Gate it separately. Window/WebView code lives under the bin target, so we
# lint `--bin ato-desktop` (this also compiles the lib it depends on).
#
# clippy runs with `-D warnings`. The allow list below is deliberate:
#
#   dead_code                       — unreliable for a cross-platform binary
#                                     analyzed only on the host OS: code behind
#                                     `#[cfg(not(target_os = ...))]` and code
#                                     exercised only by `#[cfg(test)]` reads as
#                                     dead here but is live elsewhere. Genuinely
#                                     dead modules/functions were already deleted.
#   await_holding_lock / collapsible_if
#                                   — same allowances as the root workspace.
#                                     (unused_unsafe is intentionally NOT
#                                     allowed, so the gate catches nested or
#                                     redundant `unsafe` blocks.)
#   result_large_err / large_enum_variant
#                                   — fixing means boxing error/enum payloads
#                                     (a behavioral/perf change), out of scope.
#   too_many_arguments / type_complexity / enum_variant_names
#                                   — would require signature/API refactors.
#   arc_with_non_send_sync          — GPUI entity handles are single-threaded
#                                     by design; the Arc usage is intentional.
#   collapsible_match               — collapsing would change match arm
#                                     exhaustiveness; left as-is.
#   doc_lazy_continuation / doc_overindented_list_items
#                                   — cosmetic rustdoc list formatting only.
echo "==> [ato-desktop] cargo fmt --all -- --check"
( cd crates/desktop && cargo fmt --all -- --check )

echo "==> [ato-desktop] cargo clippy --bin ato-desktop -- -D warnings"
( cd crates/desktop && cargo clippy --bin ato-desktop -- -D warnings \
    -A dead_code \
    -A clippy::await_holding_lock \
    -A clippy::collapsible_if \
    -A clippy::result_large_err \
    -A clippy::large_enum_variant \
    -A clippy::too_many_arguments \
    -A clippy::type_complexity \
    -A clippy::enum_variant_names \
    -A clippy::arc_with_non_send_sync \
    -A clippy::collapsible_match \
    -A clippy::doc_lazy_continuation \
    -A clippy::doc_overindented_list_items )

echo "pre-commit quality checks passed"
