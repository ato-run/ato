# Windows Desktop AODD — OCI cleanup follow-up (2026-06-04)

Follow-up to the `#369` Windows Desktop recipe-launch AODD. The original matrix
(`docs/aodd/desktop_recipe_launch_matrix.csv`, run `20260530`) recorded blockers
`#372` (state_binding_unix_path), `#373` (podman_dns_failure), and `#377`
(Desktop GitHub source build needs `/bin/sh`).

A later `#369` rerun (PR `#452`) surfaced two **cross-cutting OCI-runtime**
defects that are not per-recipe launch results: `#444` (Windows Podman writable
state mounts) and `#450` (session stop leaves orphaned Podman networks). This
note records how those — plus the related `#445` readiness diagnostics — changed
after the cleanup PRs merged. It does **not** re-grade the per-recipe Desktop GUI
matrix.

## Status of cross-cutting issues

| Issue | Topic | Fixed by | Status | Verification |
|-------|-------|----------|--------|--------------|
| #444 | Windows + Podman: stateful recipes can't write bind-mounted state | #456 | **code merged** | Unit-tested (engine-volume strategy, capsule + ato-cli). **App-level Desktop GUI verification still pending.** |
| #445 | multi-service `exited-before-ready` collapsed to a generic error | #453 | **code merged** | `blinko`-style exit-before-ready now preserves the typed `oci_container_exited_before_ready` diagnostic instead of collapsing to E999. |
| #450 | session stop leaves orphaned `ato-*` Podman networks | #458 | **resolved / verified** | Real Podman 5.8.2 / WSL rootless, via `ato app session start/stop` (the same `stop_session` path the Desktop uses). See below. |
| #460 | Runtime Setup not fully Desktop-driven (WSL/Podman substrate) | — | **open** | UX gap; WSL/virtualization/health-error detection + remediation still to do. |

## #450 verification detail (2026-06-04)

Environment: Windows 11 · Podman **5.8.2** · machine `podman-machine-default`
(VM type **wsl**, rootless, running). Builds: `dev`/#456 = `target/debug/ato.exe`
(without #458); #458 = branch `fix/issue-450-session-network-cleanup` head
`69a73c1f` (base `dev` `d6fa8b19`).

Method: `ato app session start "capsule://github.com/sosedoff/pgweb"` (launches
via in-process orchestration → creates an `ato-pgweb-<hash>-<pid>` network), then
`ato app session stop`.

| scenario | dev (#456) | #458 |
|----------|------------|------|
| normal start → stop | clean (network removed) | clean (no regression) |
| start → rogue endpoint attached to session network → stop | **LEAK** — stop returned `network_cleanup_warning: "... network is being used; run podman network rm ... manually"`; network remained | **network removed** (no warning, no residue) |

Podman-level mechanism (the exact dev→#458 code change): plain `podman network rm`
on an in-use network fails (`network is being used`); `podman network rm --force`
(what #458 emits) removes it.

Post-run: `podman ps -a`, `podman network ls`, `podman volume ls` all free of
`ato-*` residue.

## Scope / honesty caveats

- #450 was verified at the **CLI session layer**, which is the same
  `stop_session` / `remove_network_if_present` code the Desktop invokes — but the
  **literal Desktop GUI omnibar (`NavigateToUrl`) flow and WebView render were
  not re-driven** in this follow-up.
- The full Desktop GUI app matrix (memos / uptime-kuma / pgweb / node-red /
  blinko: Ready + WebView render) remains a **separate, pending AODD pass**.
  No per-recipe row is promoted to PASS on the strength of this follow-up.
- `#444`/`#456` (state mounts) is merged but its app-level Desktop GUI result is
  **pending**; `node-red` / `blinko` are not graded PASS here.
