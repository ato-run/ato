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
(`sandbox-exec` on macOS, `bwrap` on Linux).

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
