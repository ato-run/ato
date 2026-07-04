# RFC: Podman as a Desktop Runner cold-OCI substrate

- Status: **draft**
- Supersedes / extends: `docs/ready-state/desktop-runner.md`, `docs/ready-state/backend-matrix.md`
- Related: #949 (structured placement diagnostics), #951 (`ato://run` spawns `ato run` on the Desktop Runner)
- Affected crates: `crates/cli/src/application/desktop_runner/`, `crates/cli/src/adapters/runtime/oci_provider.rs`, `crates/cli/src/adapters/runtime/podman_machine.rs`

## Problem

The Desktop Runner placement gate advertises **only** Apple Containerization as a
local cold-OCI substrate. On macOS < 26 (the majority of today's Macs) or on an
Intel Mac, the gate returns `SuggestManagedRunner` with `LocalBackendBlocker::MacOsTooOld`
or `NotAppleSilicon`, even when a perfectly capable Podman install is present.

Users see: *"install Apple `container` / upgrade to macOS 26 / use a managed runner"* —
while `ato run <oci-capsule>` (the default, non-Desktop-Runner path) already runs the
same OCI image locally through `PodmanProvider` on the very same host. The two paths
disagree on what "local OCI" means, and the Desktop Runner path is the stricter one for
no correctness reason — only for a substrate-naming reason.

This RFC proposes adding **Podman** as a second Desktop Runner cold-OCI substrate on
macOS, so the placement gate can honestly say "you have a local cold-OCI backend" when
Podman is installed and its machine is ready — independent of macOS version or Apple
silicon. Linux is explicitly out of scope (see Open questions).

## Placement contract (invariant)

**The placement gate must never return `local_cold_oci_candidate` for a substrate
whose execution is not wired.** A decision that says "this can run locally" must be
backed by a real executor branch that can actually start the container. Returning a
candidate for a substrate with no executor is not a diagnostic improvement — it is a
contract violation that surfaces as "placement said yes, executor said not wired" at
run time, which is a regression for the user.

This invariant drives the phase split: Podman only becomes a
`local_cold_oci_candidate` once the `cold_oci::podman_run` executor branch exists
in the same PR. No "candidate without executor" intermediate state is allowed.

## Non-goals

- **Not** adding Ready-State restore on Podman. M0–M3 stays `cold_oci` only; CRIU
  inside the Podman VM is a future, separately-specced mechanism (the existing
  `criu-container-spike.md` track).
- **Not** removing Apple Containerization. The two substrates coexist; a host with
  both gets Apple Containerization (lighter-weight, per-session VM) as the preferred
  cold-OCI backend, with Podman as the fallback that broadens host coverage.
- **Not** changing the default `ato run` path. `PodmanProvider` continues to own the
  non-Desktop-Runner OCI execution; the Desktop Runner only **advertises** a Podman
  backend in its capability facts and routes through the existing `cold_oci` executor.
- **Not** changing the single-flight / log-isolation contract from #951. A Podman-backed
  run uses the same `spawn_desktop_runner_run` plumbing and per-run log paths.
- **Not** supporting Linux native Podman in this RFC. Linux Podman runs without a VM
  wrapper (`isolation_boundary = container`, not `vm_wrapped_container`), which changes
  the isolation taxonomy and needs its own security review. A separate follow-up RFC
  will cover Linux.

## Design

### Substrate identity

| Field | Apple Containerization (existing) | Podman (new) |
|---|---|---|
| `substrate` | `apple_containerization` | `podman` |
| `isolation_boundary` | `vm_wrapped_container` | `vm_wrapped_container` |
| `substrate_scope` | `per_session_vm` | `shared_machine` |
| `ready_state_kind` | `cold_oci` | `cold_oci` |
| `accelerator` | `apple_vz` | `none` (Podman machine uses its own virtualization; no Ato-managed accelerator label) |
| `maturity` | `Experimental` | `Experimental` |
| `supports_*` | all `false` (M0) | all `false` (M0) |

Both substrates produce a `BackendCapability` with `guest_os = "linux"`; the guest arch
matches the host arch (no cross-arch / Rosetta in M0 for either substrate).

A new `substrate_scope` field distinguishes the VM scope without overloading
`isolation_boundary`. Both substrates use `vm_wrapped_container` (a container inside a
Linux VM), but the VM scope differs — and that difference matters for cleanup, state
reuse, and what a session receipt honestly claims.

### Podman isolation semantics (shared machine, not per-session VM)

Apple Containerization starts a **per-session** lightweight VM for each container;
when the container exits, the VM is torn down. Podman, on macOS, runs containers
inside a **shared** `ato-podman` machine that persists across sessions. This is not
the same isolation shape, even though both are `vm_wrapped_container` at the boundary
level. The Podman branch must enforce:

- **Unique container name per run** — already guaranteed by #951's
  `unique_container_name`. No name reuse across runs.
- **No host bind mounts except declared safe mounts** — the Desktop Runner cold-OCI
  path does not bind-mount host directories into the container; it runs an OCI image
  as-is. This matches the Apple Containerization branch.
- **No persistent volumes by default** — the Podman branch does not create or reuse
  named volumes. State reuse between runs is limited to the immutable image / artifact
  cache (the OCI image itself), never container filesystem state.
- **No state reuse between runs** — each `ato://run` starts a fresh container from the
  image. A previous run's container filesystem, stopped container, or volumes must not
  be reused. The waiter's cleanup (`container rm -f` after the session) is mandatory.
- **Explicit stop / cleanup** — the `DesktopRunnerRunChild`'s process-group kill on
  shutdown (from #951) must also `podman rm -f <name>` the container, not just kill
  the `ato run` wrapper. The container outlives the wrapper process if the Podman
  machine keeps it running.

### Probe: `crates/cli/src/application/desktop_runner/macos.rs`

Add a Podman probe alongside the Apple Containerization probe. The macOS facts
builder reports **two** substrates in `substrates` and produces a `BackendCapability`
for each that is honestly available:

```
apple_containerization: available = is_apple_silicon && macos>=26 && container_present
podman:                  available = podman_binary_present && ato_podman_machine_running
```

The Podman probe reuses the existing, shared infrastructure rather than duplicating
detection:

- **Binary resolution**: `capsule::foundation::podman::resolve_podman()` — the same
  resolver the OCI provider uses (handles `ATO_PODMAN_BIN`, `PATH`, Homebrew known
  locations, Ato-managed installs under `~/.ato/tools`).
- **Machine state**: `crate::adapters::runtime::podman_machine::PodmanMachineStatus`
  via `podman machine list --format json` — the same parser
  `oci_provider::PodmanProvider::ensure_ready` uses. The substrate is `available`
  only when the **`ato-podman`** machine specifically is `Running` (matches the
  connection the OCI provider pins); a stopped machine yields a blocker, not an
  unavailable substrate with a silent degrade.

  - Why require the `ato-podman` machine specifically, not any running Podman machine?
    Because the existing OCI provider's connection-pinning always pins `ato-podman` on
    macOS. Accepting any running machine would diverge from the OCI path and could
    surprise users who have their own machine configured with different networking /
    arch / resource settings / volume policy. **Resolved: `ato-podman` only.**
  - Why require a running machine and not just an installed binary? Because the
    Desktop Runner substrate contract is "the host can honestly serve a cold-OCI
    session **right now**". A stopped machine means a multi-second start delay (or
    an interactive repair flow) before the first container can run — that is a
    blocker the user can act on, not an invisible degrade. The diagnostic names
    "start the `ato-podman` machine" as the next action.
  - **Auto-start is out of scope** for the probe: the Desktop Runner probe stays
    read-only and side-effect free, mirroring the Apple Containerization probe
    (which detects but never runs `container system start`). Starting the machine
    is the user's explicit action; a future `ato://runner/repair-podman` privileged
    intent (Phase C) can be added separately.

### Structured blockers (extends PR 1's `LocalBackendBlocker`)

Add new blocker kinds so a Podman-only host gets actionable diagnostics, not the
Apple-Containerization-specific `macos_too_old` / `apple_container_missing`:

```rust
pub(crate) enum LocalBackendBlocker {
    // existing
    NotAppleSilicon,
    MacOsTooOld { found: Option<String>, required: u32 },
    AppleContainerMissing,
    NonMacOsHost { host_os: String },
    // new — Podman substrate
    PodmanBinaryMissing,
    PodmanMachineStopped,           // ato-podman configured but not running
    PodmanMachineNotConfigured,     // no ato-podman machine
    PodmanMachineStatusUnavailable { reason: String }, // podman machine list failed / unparseable / permission error
}
```

`PodmanMachineStatusUnavailable` covers the cases where `podman machine list` itself
cannot run or its JSON output cannot be parsed — mapped from
`PodmanMachineStatus::Unavailable { reason }` and `PodmanMachineStatus::Unknown { reason }`.
The `reason` string is a short, safe summary (never raw stderr); the existing
`PodmanMachineStatus::display_status()` already produces this.

### Blocker rendering policy

**Placement blockers are rendered only when no local cold-OCI backend is available.**
If Podman is available, Apple Containerization blockers must NOT appear in the
placement failure path — they may appear in `ato doctor desktop-runner` substrate
details, but a successful placement (either substrate) carries no blockers.

This means:
- macOS 15 + Podman running → placement is `local_cold_oci_candidate` (Podman
  backend), `local_backend_blockers: []`. The Apple `macos_too_old` /
  `apple_container_missing` blockers are NOT emitted, because a backend IS available.
- macOS 15 + no `container` + Podman stopped → placement is
  `suggest_managed_runner`, blockers include both Apple and Podman blockers (no
  backend is available, so all applicable blockers surface).
- macOS 26 + `container` installed + Podman stopped → placement is
  `local_cold_oci_candidate` (Apple Containerization), `local_backend_blockers: []`.
  The Podman `podman_machine_stopped` blocker is NOT emitted, because a backend IS
  available.

The facts builder emits blockers only for substrates that are unavailable; a
substrate that is available contributes no blockers. This keeps the placement
failure path honest: it lists what is missing, never what is merely not-preferred.

### Selection: `matching.rs`

`local_cold_backend()` today returns the first backend matching the host os/arch.
With two substrates this needs a **preference order**:

1. `apple_containerization` (lighter-weight, per-session VM, Apple-silicon-only)
2. `podman` (broader host coverage, heavier shared VM)

The preference is encoded in a new `preferred_local_cold_backend()` method on
`DesktopRunnerFacts` (keeping `local_backend()` unopinionated for other callers).
`matching::cold_or_managed` picks the preferred available backend; if none is
available, it surfaces the substrate blockers (which, per the rendering policy
above, are exactly the blockers for unavailable substrates).

### Execution: `cold_oci.rs`

`ExecutionClass::from_backend` already derives the guest class from the chosen
`BackendCapability`. The `cold_oci::run` path invokes Apple `container` today; a
Podman-backed run needs a **parallel executor branch** selected on
`backend.substrate`:

```
substrate = apple_containerization → existing `container` run path
substrate = podman                 → cold_oci::podman_run branch
```

**Podman execution must reuse the existing Podman invocation layer for:**
- binary resolution (`PodmanInvocation` / `resolve_podman`)
- connection pinning to `ato-podman`
- container name generation (reuse #951's `unique_container_name`)
- port publishing
- readiness observation
- stop / cleanup (including `podman rm -f <name>` — see isolation semantics)
- OCI policy enforcement
- network / egress policy hooks where already supported

The Desktop Runner branch may be a thin wrapper, but it must **not** hand-roll a
separate Podman semantics path that diverges from the default OCI provider.
`podman run -d --name <unique> --publish <port>:<port> <image>` is the shape of the
command, but the implementation goes through `PodmanInvocation` so binary resolution,
`CONTAINERS_CONF`, and `PATH` prepending stay consistent with every other Podman
spawn in the codebase.

Both branches produce the same `DesktopColdOciSession` receipt (PR 2's
`desktop_run_agent::parse_session_receipt` already reads it generically). The
container-name uniqueness and process-group ownership from #951 apply unchanged to
the Podman branch; the Podman branch additionally must `podman rm -f` the container
on cleanup, because a container inside the shared `ato-podman` machine outlives the
`ato run` wrapper process.

### What stays fail-closed

- No automatic substrate selection that crosses a trust boundary: a host with
  Podman but no Apple Containerization gets a Podman backend advertised, never a
  fabricated Apple Containerization backend.
- No automatic `podman machine start` from the probe. The probe is read-only.
- No cross-arch. A macOS aarch64 host advertising Podman still reports
  `guest_arch = aarch64`; Podman's own cross-arch (qemu user) is not a Ready-State
  path and is not advertised.
- No Ready-State restore on Podman in M0. `supports_ready_state_restore` stays
  `false`; a Ready-State-enabled run still routes to `SuggestManagedRunner` exactly
  as today.
- No `local_cold_oci_candidate` for Podman until the executor branch is wired
  (the placement contract invariant).

## Test plan

### Unit tests (`macos.rs`)

Extends the 4-pattern matrix from PR 1 to cover Podman availability independently:

| Host | Apple Containerization | Podman | Expected backend | Expected blockers |
|---|---|---|---|---|
| Apple Silicon + macOS 26 + `container` + podman running | available | available | `apple_containerization` (preferred) | none |
| Apple Silicon + macOS 26 + `container` + no podman | available | unavailable | `apple_containerization` | none (Apple available → no blockers rendered) |
| Apple Silicon + macOS 15 + no `container` + podman running | unavailable | available | `podman` | none (the win) |
| Apple Silicon + macOS 15 + no `container` + podman stopped | unavailable | unavailable | none | `macos_too_old`, `apple_container_missing`, `podman_machine_stopped` |
| Intel Mac + macOS 26 + podman running | unavailable | available | `podman` | none |
| Intel Mac + no podman | unavailable | unavailable | none | `not_apple_silicon`, `podman_binary_missing` (or `podman_machine_*`) |
| macOS 15 + podman `machine list` unparseable | unavailable | unavailable | none | `macos_too_old`, `apple_container_missing`, `podman_machine_status_unavailable` |
| No substrate at all | unavailable | unavailable | none | all applicable blockers |

The probe is split into `MacosProbeInputs` (raw host facts) +
`build_macos_facts` (pure), so these are all pure-function tests with no real
Podman / `container` calls — mirroring the existing Apple Containerization test
strategy.

### Unit tests (`matching.rs`)

- `preferred_local_cold_backend_picks_apple_containerization_when_both_available`
- `preferred_local_cold_backend_falls_back_to_podman_when_apple_unavailable`
- `cold_or_managed_surfaces_all_blockers_when_neither_available`
- `placement_success_suppresses_unavailable_substrate_blockers` — when Podman is
  available, Apple Containerization blockers do NOT appear in
  `local_backend_blockers` (the rendering policy).

### Unit tests (`execute.rs` / `render_placement_failure`)

- `placement_failure_on_podman_only_host_names_podman_actions` — macOS 15 host
  with a stopped `ato-podman` machine gets `podman_machine_stopped` + "start the
  `ato-podman` machine" in the next-action line.
- `placement_failure_with_both_unavailable_lists_both_substrate_reasons` — the
  rendered error includes both Apple and Podman blocker tags.

### Unit tests (`cold_oci.rs` — Podman branch)

- `podman_run_reuses_podman_invocation_for_binary_resolution` — the branch does
  not spawn a bare `"podman"` string; it goes through `PodmanInvocation`.
- `podman_run_cleanup_removes_container` — after the session, `podman rm -f
  <name>` is invoked (not just the wrapper kill).

### Integration / smoke (manual, gated)

- macOS 15 + Apple silicon + Podman + `ato-podman` running → `ato doctor
  desktop-runner` reports a `podman` cold-OCI backend; `ato://run` launches the
  container through `podman run` and the activity log shows the session receipt.
- macOS 15 + Apple silicon + Podman installed but `ato-podman` stopped →
  `ato doctor desktop-runner` reports no backend and a `podman_machine_stopped`
  blocker with a "start the `ato-podman` machine" next action.
- macOS 26 + Apple silicon + `container` installed → unchanged from today
  (Apple Containerization preferred).
- Double-click `ato://run` on a Podman-backed host → single-flight guard from
  #951 holds; no second container.

## Open questions (resolved)

1. **`ato-podman` machine only, or any running Podman machine?** — **Resolved:
   `ato-podman` only.** Matches the OCI provider's connection-pinning. Accepting
   any machine would diverge and inherit a user's own machine networking / arch /
   volume policy.

2. **Auto-start the `ato-podman` machine when stopped?** — **Resolved: no, not in
   the probe.** The probe is read-only. A future `ato://runner/repair-podman`
   privileged intent (Phase C) can handle the start flow with explicit user
   consent, mirroring `ato://runner/register`.

3. **Reuse `PodmanProvider` directly or a thinner helper?** — **Resolved: thin
   wrapper over `PodmanInvocation` + shared `podman_machine` parser, not the full
   `PodmanProvider` state machine.** Keeps the substrate probe and executor
   decoupled from the provider's readiness state. The wrapper MUST reuse
   `PodmanInvocation` for binary resolution / connection pinning / `CONTAINERS_CONF`
   — it must not hand-roll a divergent `podman run` path.

4. **Linux host scope.** — **Resolved: out of scope for this RFC.** Linux native
   Podman is `isolation_boundary = container` (not `vm_wrapped_container`), which
   changes the isolation taxonomy and needs its own security review. A separate
   follow-up RFC will cover Linux.

## Implementation phases

### Phase A — facts-only / doctor-only (placement decision unchanged)

- Add the Podman probe to `macos.rs` (binary resolution + `ato-podman` machine
  state).
- Extend `LocalBackendBlocker` with the four Podman blocker kinds.
- Add `substrate_scope` to `SubstrateCapability` / `BackendCapability`.
- `ato doctor desktop-runner` reports Podman substrate availability and
  diagnostics, including when Podman is the only available substrate.
- **Placement is NOT changed**: `matching::cold_or_managed` still only considers
  Apple Containerization for `local_cold_oci_candidate`. Podman blockers appear in
  the placement failure path when Apple Containerization is unavailable (improving
  the diagnostic), but Podman does NOT itself become a candidate.
- `doctor` shows "Podman detected, Desktop Runner execution not wired yet" when
  Podman is available but the executor branch is absent.
- The run path is untouched.

This phase is independently shippable: it improves diagnostics on a Podman-equipped
macOS 15 host (the user sees `podman_machine_stopped` or "detected, not wired yet"
instead of a bare `macos_too_old`), without ever claiming a placement the executor
cannot honor.

### Phase B — executable Podman substrate (placement contract extended)

- Add `cold_oci::podman_run` branch, selected on `backend.substrate == "podman"`.
- The branch reuses `PodmanInvocation` for binary resolution / connection pinning,
  reuses #951's `unique_container_name`, and adds `podman rm -f` cleanup.
- **Only now** does `matching::cold_or_managed` consider Podman as a
  `local_cold_oci_candidate` (preferred order: Apple Containerization, then Podman).
- Ship the `ato://run` smoke from #951 against a Podman backend.
- Blocker rendering policy (suppress unavailable-substrate blockers when a backend
  is available) is enforced here, since placement now succeeds on a Podman-only host.

Phase A and Phase B MAY be combined into a single PR if the executor branch is
ready at the same time. The invariant is: **Podman becomes a candidate only when
the executor is wired**. If they are combined, Phase A's "detected, not wired yet"
doctor message is unnecessary.

### Phase C — repair UX

- Stopped `ato-podman` machine start flow.
- `ato://runner/repair-podman` privileged intent (mirrors `ato://runner/register`).
- UI / PWA-side "Start local runner substrate" CTA when a run is requested and the
  machine is stopped.
- Preference / fallback policy tuning based on Phase B feedback.

## Documentation updates

- `docs/ready-state/desktop-runner.md`: add the Podman substrate rows to the
  capability table; note that the probe is read-only and never auto-starts the
  machine; document the `substrate_scope` distinction (per-session VM vs shared
  machine) and the cleanup / no-state-reuse contract.
- `docs/ready-state/backend-matrix.md`: add the macOS + Podman row with
  `isolation_boundary = vm_wrapped_container`, `substrate_scope = shared_machine`,
  `ready_state_kind = cold_oi`, `accelerator = none`, `maturity = experimental`.
- `docs/rfcs/accepted/` (on merge): move this RFC to `accepted/` and update the
  spec cross-references in the two ready-state docs.
