# Clean-VM first-run acceptance gate (no Homebrew, no git)

_Status as of 2026-06-07. Tracks the v0.6 first-run blocker: Ato must work on a
clean VM **without the user pre-installing Homebrew or git**. Ato's promise is to
detect and prepare missing tools — never to tell the user "install Homebrew/git
and come back."_

## Acceptance criteria → status

| Criterion | Status | Evidence / where |
|---|---|---|
| Desktop/CLI run with **Homebrew not installed** | ✅ | Running Ato never needed brew; provider *install* no longer hard-requires it (#574). |
| **git not installed** → public GitHub source **fetch + manifest inference** works | ✅ | Already tarball-based (`download_github_repository_at_ref`, `ATO_GITHUB_API_BASE_URL`); `source_tree_hash` excludes `.git`. Verified empirically with `git` scrubbed from PATH (#575 `gitless_github_source_install_e2e`) and locked by the extended `consumer_paths_do_not_spawn_git_commands` guard. |
| OCI target with **no provider** → routed to Runtime Setup, **not** "install Homebrew" | ✅ | `install_podman` now yields a typed actionable error presenting the Ato-managed installer, never a brew instruction (#574). |
| When Podman is needed, **Ato presents a verified installer strategy** | ⚠️ framework | Ordered strategies (Homebrew-if-present → Ato-managed verified download → manual) + digest-fail-closed installer landed (#574). **Remaining:** the pinned macOS Podman SHA256 digests are placeholders, so the real auto-install fails closed → manual instructions until the digests are filled from Podman's official `shasums` and the archive layout is confirmed on a real VM. |
| Failures are **typed actionable errors**, not `CommandNotFound(git/brew)` | ✅ | git-toolchain-missing → E203 with a "fetch is gitless; this is your app's dependency" message (#575); provider-missing → actionable Runtime-Setup error (#574). |

## Known residual blockers (finishable; need clean-VM confirmation)

1. **Podman real digests (#574 remaining step).** Fill the pinned macOS (arm64/x86_64)
   Podman release SHA256 digests from the official `shasums`, confirm the archive's
   internal `binary_rel_path` / `strip_prefix` on a fresh VM, then the Ato-managed
   installer actually installs Podman with no Homebrew.
2. **App dependencies that use `git+https://…`.** These genuinely need `git` (or a
   future gitless dependency resolver). Ato's *own* fetch is gitless; #575 makes
   this a clear typed error (E203) instead of an opaque E999, but auto-resolving
   git-URL app deps without git is a separate follow-up, not part of this hotfix.

## Manual clean-VM smoke recipe (the gate)

Run on a fresh VM with **neither Homebrew nor git** installed, fresh `ATO_HOME`:

```bash
# 1. Ato Desktop/CLI must start with no brew, no git
ato --version

# 2. public GitHub source app (no git+https deps) installs without git
ato install github.com/<a public source sample without git-URL deps>
# expect: success (tarball fetch + inference), no "install git" message

# 3. an OCI app with no provider must route to Runtime Setup (not brew)
ato install github.com/<an OCI sample>
# expect: a typed Runtime-Setup / "Ato can install a local container runtime"
#         prompt — NOT "install Homebrew (brew.sh) and re-run"

# 4. once #574 digests are filled: Ato auto-installs Podman with no brew, then the
#    OCI app reaches Ready
```

Automated per-property gates that DO pass today:
- `cargo test -p ato-cli --test gitless_github_source_install_e2e` (no-git fetch/inference)
- `cargo test -p ato-cli --lib consumer_paths_do_not_spawn_git_commands` (no git on consumer paths)
- `cargo test -p ato-cli --lib podman_install` (brew-optional strategy ordering, digest fail-closed, actionable error)

A fully-green end-to-end clean-VM gate (steps 2–4 automated) lands once the #574
podman digests are filled and verified on a VM.

## Provenance
v0.6 first-run-blocker hotfix, 2026-06-07: #575 (gitless install + typed git error),
#574 (Homebrew-free provider bootstrap), this acceptance doc.
