# Known Limitations — ato v0.5

This document lists known gaps between the declared specification and the current
v0.5 implementation. Each item includes the affected area, current behaviour,
and the planned resolution.

---

## L1 — `egress_allow` is advisory on source runtimes

**Spec intent:** `network.egress_allow` should allowlist specific hostnames and block
all other outbound connections.

**Current behaviour (v0.5):** On `source/python` and `source/node` runtimes, setting
`egress_allow` does NOT block connections to unlisted hosts. The field is recorded and
forwarded to the ato-tsnetd sidecar (SOCKS5 proxy), but the sidecar is not yet
auto-connected to source workloads.

`network.enabled = false` (deny-all) IS fully enforced via the OS sandbox
(Seatbelt via `sandbox_init` on macOS, `bwrap` + Landlock on Linux).

**Workaround:** Use `network.enabled = false` when complete network isolation is
required. Do not rely on `egress_allow` alone to restrict a source capsule to a
specific set of hosts.

**Resolution:** Auto-attach ato-tsnetd SOCKS5 enforcement to source runtimes.
Targeted for v0.5.1.

---

## L2 — `required_env` enforcement is advisory

**Spec intent:** Fields under `requirements` (e.g. `required_env`) should cause
`ato run` to abort with a clear error if declared environment variables are absent.

**Current behaviour (v0.5):** Missing `required_env` entries produce a warning but
do not block execution. The capsule launches and may fail at runtime with a less
helpful error.

**Workaround:** Document required variables in `capsule.toml` metadata and surface
them in your capsule's own startup checks.

**Resolution:** Enforce `required_env` as a pre-flight gate in `ato run`. Targeted
for v0.5.1.

---

## L3 — `--sandbox` flag not supported for `source/python`

**Spec intent:** `ato run --sandbox` should enable enhanced sandbox isolation for
all runtime types including source runtimes.

**Current behaviour (v0.5):** Passing `--sandbox` to a `source/python` capsule
returns `E301: --sandbox not yet supported for source/python`. Source runtimes
already run with basic OS-level isolation; the `--sandbox` flag targets a stricter
confinement mode that is not yet implemented for interpreted source runtimes.

**Workaround:** Source/python capsules run with standard isolation by default.
Use `network.enabled = false` and `[isolation]` to restrict access.

**Resolution:** Implement enhanced sandbox mode for source runtimes. Targeted for
v0.6.

---

## L4 — Lock file auto-generation policy is not finalized

**Spec intent:** When no `ato.lock.json` is present, `ato run` should have a
documented and consistent policy on whether it auto-generates a lock file or prompts
the user.

**Current behaviour (v0.5):** Lock file generation is triggered implicitly during
`ato run` without user confirmation. The policy for when to regenerate vs. abort is
implementation-defined and may change.

**Workaround:** Run `ato init` explicitly to generate a lock file before `ato run`
to ensure deterministic resolution.

**Resolution:** Document and stabilize the lock policy in v0.5.1 (tracked in issue #167).

---

## L5 — Multi-service sidecar topology is experimental

**Spec intent:** `[services]` in `capsule.toml` enables supervisor-mode
multi-process orchestration with dependency ordering.

**Current behaviour (v0.5):** `[services]` is parsed and dependency graphs are
validated, but runtime orchestration (start ordering, health probe enforcement,
restart policies) is incomplete. Use in production is not recommended.

**Workaround:** Use single-process capsules with an external process manager.

**Resolution:** Full orchestration runtime targeted for v0.6.

---

## L6 — `ato://` URL handler not auto-registered on Linux

**Spec intent:** `ato://` URLs should be registered as a system-level URL handler
on install, enabling `ato://run/<id>` links to launch capsules from the browser.

**Current behaviour (v0.5):** The handler is registered on macOS (Launch Services)
and Windows (registry) during install. On Linux, registration requires a manual
`xdg-mime` step documented in the install output but not automated.

**Workaround:** Run `ato setup --register-handler` manually on Linux after install.

**Resolution:** Auto-register `xdg-mime` during `install.sh` on Linux. Targeted for v0.5.1.

---

## L7 — Synthetic workspace cache is not GC'd

**Spec intent:** Managed package caches under `~/.ato/cache/` should be
automatically pruned to prevent unbounded disk growth.

**Current behaviour (v0.5):** `~/.ato/cache/synthetic/` accumulates one
directory per `(provider, package, version)` tuple and is never automatically
cleaned. Heavy usage — for example, running `npm:mintlify` daily over weeks —
can accumulate hundreds of MB to several GB.

```bash
# Inspect disk usage
du -sh ~/.ato/cache/synthetic/

# Manual cleanup (safe to delete; will be re-created on next run)
rm -rf ~/.ato/cache/synthetic/<stale-entry>
```

**Workaround:** Periodically remove stale entries from `~/.ato/cache/synthetic/`
manually when disk pressure arises.

**Resolution:** Automatic LRU-based GC with a `ato gc --synthetic` command.
Targeted for v0.5.1 (tracked in RFC `UNIFIED_EXECUTION_MODEL.md` §4.3 / §7.2).

---

## L8 — Windows / Linux Desktop are beta-quality

**Spec intent:** `ato-desktop` should provide first-class capsule
orchestration UX on macOS, Windows, and Linux equally.

**Current behaviour (v0.5):** Desktop is gold on macOS only. Windows
and Linux builds compile from the same source and pass the same unit
tests, but the underlying GPUI fork has known regressions on those
platforms — IME composition glitches, occasional WebView blank-frame
on first paint, and missing native menu integration. The bundled
`ato` CLI is gold on all three platforms; only the Desktop GUI carries
the beta label.

**Workaround:** Use `--cli-only` (or the headless install path) on
Windows / Linux for production work. Desktop installers ship for
those platforms so contributors can test the GUI, but the v0.5
release notes flag them as beta.

**Resolution:** Per-platform GPUI parity is tracked alongside
upstream Zed; structural readiness (signed/notarized bundles,
URL-scheme registration, install.sh routing) lands in v0.5 so the
flip to "gold" in v0.6 is a label change, not a build change.

---

## L9 — CCP wire shape is fixed at v1; no bidirectional streaming

**Spec intent:** The Capsule Control Protocol may eventually grow
bidirectional streaming so the desktop can push commands to a running
capsule without re-spawning the CLI.

**Current behaviour (v0.5):** Each desktop → CLI interaction is a
separate `ato`-process invocation that returns one CCP envelope on
stdout (`schema_version: "ccp/v1"`). The desktop tolerates additive
fields per `apps/ato-cli/docs/specs/CCP_SPEC.md` so v1.x CLI changes
land non-breakingly, but the request/response shape itself is fixed.

**Workaround:** None required for v0.5 use cases. Long-running
capsules are managed via repeated `session_status` polls.

**Resolution:** A `ccp/v2` schema with stdin/stdout streaming is on
the v0.6+ roadmap when a concrete UX (live logs, capsule-to-desktop
events) drives the requirement.

---

## L10 — Windows MSI is unsigned in v0.5

**Spec intent:** The Windows Desktop / CLI MSI should be signed with an
EV code-signing certificate so SmartScreen accepts it without the
"Windows protected your PC" dialog.

**Current behaviour (v0.5):** The MSI ships unsigned. On first
install, SmartScreen displays a warning dialog and requires the user
to click "More info" → "Run anyway". After install, no further
SmartScreen prompts are shown. The xtask + WiX pipeline already
includes the `signtool` invocation gated on
`WINDOWS_CODESIGN_PFX` / `WINDOWS_CODESIGN_PASSWORD`; flipping these
secrets in CI is the only delta needed once the EV cert is procured.

**Workaround:** Click through the SmartScreen dialog on first
install. Documented in the v0.5 release notes and install-win.ps1
output.

**Resolution:** EV certificate procurement and CI rollout in v0.5.x
(PR-12 placeholder in the distribution plan).

---

## L11 — macOS Desktop is ad-hoc signed (not Apple Developer ID)

**Spec intent:** Long term, the Desktop bundle should be signed with
an Apple Developer ID Application identity and notarized so Gatekeeper
accepts it without the "developer cannot be verified" dialog.

**Current behaviour (v0.5):** The bundle is ad-hoc signed
(`codesign --sign -`) with hardened-runtime entitlements. Direct
download flows hit Gatekeeper friction on first launch; `install.sh`
runs `xattr -dr com.apple.quarantine` on the staged bundle to strip
the flag, and Homebrew Cask (`auto_updates true`) gets the same
treatment for free. The xtask code-sign mode is env-resolved
(`MAC_DEVELOPER_ID_NAME`); switching to Developer ID is a secrets
flip plus the notarize step (already implemented, gated on
`APPLE_ID` / `APPLE_APP_SPECIFIC_PASSWORD` / `APPLE_TEAM_ID`).

**Workaround:** Either install via Homebrew Cask (`brew install --cask
ato`) or accept the install.sh quarantine strip. Direct double-click
from a Finder download requires right-click → Open the first time.

**Resolution:** Apple Developer ID + notarize in v0.6 once adoption
justifies the annual program fee. Same Keychain identifiers
(`run.ato.desktop`) — no migration friction expected.

---

## Foundation readiness (informational)

The following Foundation KPIs (§11.2 of the Capsule Protocol spec) are tracked for
transparency. These are not bugs but milestones toward open governance.

| KPI | Status |
|-----|--------|
| ≥1 conforming external runtime | 0 / 1 |
| Conformance suite ≥70% pass | 0% (suite in `conformance/`, not yet populated) |
| External maintainers ≥3 | 0 / 3 |
| TSC non-ato majority | 0 / required |
| ≥100 publishers | 0 / 100 |
| ≥5 adversarial security reports | 0 / 5 |

Foundation transfer is not a v0.5 milestone. This table is published for
transparency per the "Copy over Imitation" principle.
