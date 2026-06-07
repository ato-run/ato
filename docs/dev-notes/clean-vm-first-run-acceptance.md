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
| When Podman is needed, **Ato presents a verified installer strategy** | ✅ (download→bundle→run); ⏳ (machine init/start) | Ordered strategies (Homebrew-if-present → Ato-managed verified download → manual) + digest-fail-closed, **atomic** installer with timeouts (#574). The Ato-managed install is now a **full macOS machine-runtime bundle**, not just the `podman` CLI: it also downloads + digest-verifies **gvproxy 0.7.5** and **vfkit 0.5.1** (the helpers Podman v5.2.3 itself bundles), installs them next to `podman`, and writes an Ato-owned `containers.conf` (`helper_binaries_dir` + `provider = "applehv"`). The real production fetcher was run on macOS arm64: download → digest match (podman + gvproxy + vfkit) → extract → `podman --version` = `podman version 5.2.3`, helpers executable, `containers.conf` written, preflight reports a complete runtime (throwaway tools dir). The host-mutating **machine** path (`machine init/start/info` + OCI Ready) is **NOT yet validated** — see "Required before merge". |
| **`podman --version` is NOT sufficient** for a macOS machine runtime | ✅ (enforced) | `podman machine init/start` needs `gvproxy` + `vfkit`; the remote-client zip ships neither. The installer validates the helpers are present + executable **before promotion** (rejecting an incomplete bundle), and a preflight self-repairs / fails with a typed `RuntimeProviderIncomplete` (`could not find "gvproxy"` → typed, not a generic mid-init failure) before `machine init` runs. |
| **Helpers must be native arch — exists + executable is NOT sufficient** | ✅ (enforced) | A Mach-O helper lacking the host's slice would run under Rosetta (hidden prerequisite). The installer parses each bundled Mach-O (minimal fat/thin header reader, no `lipo`/Xcode CLT) and rejects any podman/helper without a native slice for the host arch **before promotion** (`NotNativeArch`). The pinned gvproxy 0.7.5 / vfkit 0.5.1 are verified universal (arm64 + x86_64) by the real smoke. |
| **Ato-managed runtime must never require Rosetta** | ✅ (config) | Podman's `applehv` defaults `rosetta = true`, so `podman machine start` on Apple Silicon sets up a Rosetta guest share and prompts to install Rosetta on a clean VM (then `vfkit exited unexpectedly with exit code 1` if declined). Ato's generated `containers.conf` sets `rosetta = false`, so the machine boots natively with no host Rosetta. x86_64 Linux images are emulated in-guest, not via host Rosetta. |
| Failures are **typed actionable errors**, not `CommandNotFound(git/brew)` | ✅ | git-toolchain-missing → E203 with a "fetch is gitless; this is your app's dependency" message (#575); provider-missing → actionable Runtime-Setup error (#574). |

## Required before merge

A real clean-VM macOS smoke (no brew/git/ATO_HOME/podman) verifying
download → sha → resolve-from-`~/.ato/tools` → `podman machine init ato-podman` →
`machine start` → `podman --connection ato-podman info --format json` →
an OCI sample reaches Ready.

**Status: NOT yet validated end-to-end.** The no-brew Podman **machine** path
(`machine init`/`start`/`info` + an OCI sample reaching Ready) is a required
pre-merge manual clean-VM smoke and has NOT been run. Running
`podman machine init/start` mutates the real host, so it is deliberately left to
the clean-VM gate and was NOT executed on the development machine.

What **is** now verified (real, on macOS arm64, against a throwaway tools dir —
not the user's `~/.ato`) by the
`real_ato_managed_install_downloads_verifies_and_runs` smoke (ignored by
default; needs network + macOS arm64): the production fetcher downloads the
**full machine-runtime bundle** and digest-verifies every piece —

- podman v5.2.3 darwin archive (~25 MB), SHA256
  `1449ceb220907ca94407ca3a2a7d5d7909602657d3f5ea9cab26e4dd7c366b69`;
- **gvproxy 0.7.5** (`gvproxy-darwin`), SHA256
  `ca881d38963456bdf56b596bc2d76dfa72b565e701acf584d749a1543915f800`;
- **vfkit 0.5.1** (signed `vfkit`, carries `com.apple.security.virtualization`),
  SHA256 `6adf8ab2fb0a3b7e7d778554bdc4ae8a8d9e8f984cebffd4e0c8ff8ea5f08447`;

then extracts, confirms `podman --version` = `podman version 5.2.3`, confirms
gvproxy + vfkit are installed next to podman and executable, writes the Ato-owned
`containers.conf` (`helper_binaries_dir` + `provider = "applehv"`), and confirms
the helper preflight reports a **complete** runtime. The install is **atomic**
(extract into a temp sibling dir, validate podman runs + every helper is present,
write `containers.conf` + provenance, then `rename` into place; remove the temp
dir on any failure — an incomplete bundle is never promoted) and the fetcher has
connect/total HTTP timeouts (15s/300s).

**Why this matters:** the earlier hotfix verified only `podman --version`, which
is *not sufficient* — a clean macOS VM still failed with `could not find
"gvproxy"` at `machine init` because the remote-client zip ships no helpers. The
bundle + preflight close that gap. A second clean-VM run (PR #578) then surfaced
a *different* hidden prerequisite: Podman's `applehv` defaults `rosetta = true`,
so `podman machine start` prompted to install **Rosetta** and `vfkit exited
unexpectedly with exit code 1` on a Rosetta-less VM. Note this was **not** an
Intel-only vfkit — `lipo -archs` and the real smoke confirm the pinned vfkit /
gvproxy are universal with native arm64 slices; the trigger was the Rosetta
**guest share**, not the helper arch. Ato now sets `rosetta = false` in the
generated `containers.conf`, and additionally enforces native-arch validation on
every bundled Mach-O before promotion (defense-in-depth against a future
non-universal pin). The only unproven step left is the host-mutating
`machine init/start` itself.

## Known residual blockers (finishable; need clean-VM confirmation)

1. **Clean-VM end-to-end auto-install smoke (machine path).** The download +
   bundle half is now proven for real on macOS arm64 (download → digest-verify
   podman + gvproxy + vfkit → extract → `podman --version` → helpers executable →
   `containers.conf` written → preflight reports complete), and the installer is
   atomic with HTTP timeouts. The remaining unconfirmed half is the **machine**
   path on a fresh VM (no brew): `podman machine init ato-podman` →
   `machine start` → `podman --connection ato-podman info --format json` → an OCI
   sample reaches Ready. This was deliberately NOT run on the dev machine (it
   mutates the real host) and is the required pre-merge gate. See "Required
   before merge".
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

# 4. Ato auto-installs the full Podman machine-runtime bundle with no brew
#    (podman + native gvproxy + native vfkit + containers.conf), then:
ato internal runtime prepare --tools podman
#    Sanity-check the bundled helpers are native (no Rosetta), e.g.:
file ~/.ato/tools/podman-5.2.3/usr/bin/vfkit     # must list arm64
file ~/.ato/tools/podman-5.2.3/usr/bin/gvproxy   # must list arm64
grep rosetta ~/.ato/tools/podman-5.2.3/containers.conf   # rosetta = false
podman --connection ato-podman info --format json   # must succeed
#    then the OCI app reaches Ready. Expect NO Rosetta install prompt and NO
#    `could not find "gvproxy"`.
```

Automated per-property gates that DO pass today:
- `cargo test -p ato-cli --test gitless_github_source_install_e2e` (no-git fetch/inference)
- `cargo test -p ato-cli --lib consumer_paths_do_not_spawn_git_commands` (no git on consumer paths)
- `cargo test -p ato-cli --lib podman_install` (brew-optional strategy ordering, digest fail-closed,
  helper bundle install + `containers.conf` with `rosetta = false`, incomplete-bundle rejected before
  promotion, native-arch Mach-O validation — x86_64-only helper rejected on arm64, universal accepted)
- `cargo test -p ato-cli --lib runtime_prepare` (helper preflight → typed `RuntimeProviderIncomplete`;
  `could not find "gvproxy"` machine error mapped to the typed category, not a generic failure)
- `cargo test -p capsule-core --lib podman` (Ato-managed `containers.conf` resolved → `CONTAINERS_CONF`)
- `cargo test -p ato-cli --lib -- --ignored real_ato_managed_install` (macOS arm64 + network: real
  download + digest-verify of podman + gvproxy + vfkit, helpers executable, `containers.conf` written)

A fully-green end-to-end clean-VM gate (steps 2–4 automated) lands once the
host-mutating `podman machine init/start` path is run and confirmed on a VM.

## Provenance
v0.6 first-run-blocker hotfix, 2026-06-07: #575 (gitless install + typed git error),
#574 (Homebrew-free provider bootstrap), this acceptance doc.
Follow-up, 2026-06-07: Ato-managed macOS Podman is now a full **machine-runtime
bundle** (podman + gvproxy 0.7.5 + vfkit 0.5.1 + Ato-owned `containers.conf`),
with a helper preflight that turns the clean-VM `could not find "gvproxy"`
failure into a typed, actionable error (and self-repairs an incomplete install).
`podman --version` is explicitly documented as insufficient for a macOS machine
runtime.

Follow-up, 2026-06-08 (PR #578 review): a second clean-VM run hit a Rosetta
install prompt + `vfkit exited unexpectedly with exit code 1`. Root cause was
Podman's `applehv` default `rosetta = true` (a host-Rosetta prerequisite), **not**
an Intel-only vfkit (`lipo`/real smoke confirm the pinned helpers are universal
with native arm64). Fix: `containers.conf` now sets `rosetta = false`, and the
installer additionally enforces native-arch validation on every bundled Mach-O
before promotion (typed `NotNativeArch`, fail-closed; no `lipo`/Xcode CLT needed).
Recorded: `helper exists + executable` is still insufficient — macOS helpers must
be native for the host arch and must not require Rosetta.
