# Skip Audit — v0.5 Release Candidate

**Total: 100 SKIP out of 154 tests (65%)**

Last updated: 2026-04-22

---

## Summary

| Category | Count | Release Blocker? |
|---|---|---|
| A — Environment not available | 13 | No (needs appropriate hardware/OS) |
| B — Requires external tester | 65 | **Yes if >10 uncovered** (see beta-program.md) |
| C — Infra-dependent (linked to FAIL 4) | 9 | Unblocks when §08/§11/§14 deploy |
| D — Not yet implemented | 13 | **Yes — needs decision or implementation** |
| E — Explicitly out of scope | 0 | — |

---

## Category A: Environment not available (13 SKIP)

Tests skipped because this machine lacks required hardware or OS.
**Not release blockers** — run on appropriate machines before release.

| Suite | Tests | Condition |
|---|---|---|
| §2 GPU Accelerator | 7 | No GPU (M-series Metal, CUDA, ROCm) |
| §4 Sandbox Boundary | 1 | `bubblewrap` (Linux-only) |
| §12 Toolchain Interference | 2 | `pyenv`, `conda` not installed |
| §13 Longtail Envs | 3 | Not NixOS / not Windows / ARM Linux (needs real Linux ARM) |

**Action**: These must be covered by external testers on appropriate hardware.
See `docs/release-blockers/beta-program.md` for assignment plan.

---

## Category B: Requires external tester (65 SKIP)

Tests skipped because they require human interaction, fresh machines, or diverse environments.
**These are the primary motivation for the beta program.**

| Suite | Count | What needs testing |
|---|---|---|
| §1 Install/Upgrade | 5 | Fresh install, upgrade, uninstall, offline, multi-version |
| §3 First-run Download | 5 | 5GB cold start UX, network resume, disk full, progressive launch |
| §4 Sandbox Boundary | 2 | Child process capability inheritance; interactive enforcement check |
| §5 Cross-OS | 4 | Same capsule.toml on Mac/Linux/Windows; locale, TZ, permissions |
| §7 Desktop UX | 10 | Onboarding, Gatekeeper, SmartScreen, dark/light mode, DPI, multi-window |
| §8 Trust UX | 6 | TOFU prompt UX, petname assignment, key rotation, offline trust expiry |
| §9 Network Isolation | 5 | `tcpdump`/Wireshark/Little Snitch packet-level verification |
| §10 Error Messages | 5 | GPU insufficient, disk full, permission denied, signature failure |
| §11 ato-api | 6 | CDN propagation, large capsule publish, concurrent publish |
| §12 Toolchain Interference | 4 | nvm Node leak, antivirus scan, corporate firewall |
| §13 Longtail Envs | 3 | MDM-managed Mac, encrypted home dir (FileVault/LUKS) |
| §14 Doc Alignment | 4 | Getting Started copy-paste, blog post commands |
| §15 Dogfooding | 6 | Team share-URL workflow, 1-week sprint, demo recording |

**Action**: Requires beta program. See `docs/release-blockers/beta-program.md`.

---

## Category C: Infra-dependent (9 SKIP)

Tests that are SKIP because dependent infrastructure isn't live yet.
**Automatically unblock when deployment checklist items complete.**

| Suite | Count | Blocked by |
|---|---|---|
| §6 Share URL | 8 | `ato login` / `ato publish` cascade — publish skipped, downstream tests skip |
| §8 Trust UX | 1 | `~/.ato/trust/` not created until first trust event (lazy init) |

**Action**: §6 cascade will resolve once `ato publish` auth is testable.
§8 trust dir skip is benign — the dir gets created on first `ato run` with a signed capsule.

---

## Category D: Not yet implemented (13 SKIP)

Tests that skip because the feature or command doesn't exist yet.
**These are potential release blockers depending on what's in the v0.5 spec.**

| Suite | Count | Missing feature |
|---|---|---|
| §4 Sandbox Boundary | 3 | `--sandbox` flag for source/python not yet supported (E301) |
| §6 Share URL | 1 | `ato login` / authenticated publish not implemented |
| §9 Network Isolation | 4 | Network enforcement for source/python (linked to D1 decision) |
| §10 Error Messages | 1 | Network enforcement advisory skip (linked to D1 decision) |
| §11 ato-api | 1 | `ato login` for publish auth |
| §14 Doc Alignment | 3 | `ato open`, `ato trust`, `ato version` subcommands not yet present |

**Action per item**:
- `--sandbox` for source/python (§4): linked to D1 decision in `decisions-needed.md`
- `ato login` / publish auth (§6, §11): needs scoping — is this in v0.5?
- Network enforcement (§9, §10): linked to D1 decision
- `ato open`, `ato trust`, `ato version` (§14): need to check if these are v0.5 features;
  if not, tests should be moved to Category E or the `--help` docs updated

---

## Category E: Explicitly out of scope (0 SKIP)

No tests have been deliberately scoped out. All current SKIPs are environmental or implementation gaps.

---

## Action Items

| Priority | Item | Owner | Deadline |
|---|---|---|---|
| P1 | Assign Category A tests to beta testers with correct hardware | ??? | T-28d |
| P1 | Decide on Category D (D1: sandbox enforcement, `ato login` scope) | ??? | T-21d |
| P2 | Run beta program for Category B (65 tests) | ??? | T-7d |
| P3 | §6 cascade unblocks when `ato publish` auth is implemented | dev | T-21d |
