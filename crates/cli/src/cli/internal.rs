//! Hidden plumbing surface for trusted shells (today: ato-desktop).
//!
//! Each subcommand here is `#[command(hide = true)]` because it is a
//! plumbing endpoint, not a user-facing command. Stability guarantees
//! are weaker than the public CLI: arguments may evolve in lockstep
//! with the calling shell.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum InternalCommands {
    /// Experimental vertical slice for Capsule Protocol State + PTY I/O.
    #[command(
        hide = true,
        about = "Capture or replay a portable Capsule Protocol bundle"
    )]
    CapsuleProtocol {
        #[command(subcommand)]
        command: CapsuleProtocolInternalCommands,
    },

    /// Detached Capsule Session Supervisor plumbing.
    #[command(hide = true, about = "Manage detached Capsule terminal sessions")]
    CapsuleSession {
        #[command(subcommand)]
        command: CapsuleSessionInternalCommands,
    },

    /// Consent-store plumbing surface. Currently only carries the
    /// approve-execution-plan endpoint that the desktop's E302 modal
    /// (and the matching `approve_execution_plan_consent` MCP tool)
    /// calls after the user approves the plan summary.
    #[command(hide = true, about = "Consent-store plumbing for trusted shells")]
    Consent {
        #[command(subcommand)]
        command: ConsentInternalCommands,
    },

    /// #117 — eager pre-launch requirement collection. Walks the
    /// orchestration target graph for `target` (a local capsule path
    /// or cached GitHub repository ref), derives an ExecutionPlan per service
    /// target without running any provisioning side effects (no
    /// `uv venv`, no `npm install`, no postgres provider startup),
    /// checks consent state per plan, and inspects each target's
    /// `required_env` (including dep-contract `{env.X}` substitutions)
    /// against the caller's SecretStore.
    ///
    /// Emits one aggregate JSON envelope on stdout listing every
    /// pending `InteractiveResolutionEnvelope` so a UI shell (today:
    /// ato-desktop) can render a single resolution modal containing
    /// all per-target consents + missing-env rows at once. The
    /// envelope reuses the shape established by #96 / #126 / #135 /
    /// #139 — no new wire format.
    ///
    /// Stability: same plumbing-tier guarantee as `ato internal
    /// consent approve-execution-plan`. The desktop calls this
    /// command before invoking `ato run` so the unified resolution
    /// modal opens once with everything visible, instead of opening
    /// repeatedly as the launch loop trips one error at a time.
    #[command(
        hide = true,
        about = "Collect aggregate launch requirements before provisioning (plumbing)"
    )]
    Preflight {
        /// Local capsule path or cached GitHub repository ref such as
        /// `github.com/owner/repo`. Unlike `ato run`, this plumbing
        /// path never fetches or installs.
        target: String,
        /// Fetch the specified community capsule.toml by ID, validate its
        /// source identity against `target`, and run preflight against the
        /// fetched manifest. When set, the offline manifest cache is
        /// bypassed and the community TOML is used instead.
        #[arg(long = "community-toml-id")]
        community_toml_id: Option<String>,
        /// Emit machine-readable JSON output on stdout. Without it
        /// the command emits a brief human-readable summary (still
        /// including every identity field a TTY user could copy-
        /// paste into `ato internal consent approve-execution-plan`).
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// #420 — host runtime-setup plumbing. Reports whether the runtime tools
    /// a recipe needs (Podman, Docker Desktop, Node, uv, Python, the bundled
    /// ato helper, nacelle) are installed and usable, and installs Ato-managed
    /// copies of the language runtimes (Node/uv/Python) on request. The
    /// desktop onboarding/settings UI shells out to these instead of probing
    /// the host directly.
    ///
    /// Replaces the earlier "host device detection" / GPU-scan path: nothing
    /// here scans CPU/GPU/hardware capabilities.
    #[command(
        hide = true,
        about = "Host runtime-setup status & managed install (plumbing)"
    )]
    Runtime {
        #[command(subcommand)]
        command: RuntimeInternalCommands,
    },

    /// Sweep stale import preview sessions left behind by a crashed
    /// Desktop/CLI owner.
    #[command(hide = true, about = "Sweep stale import preview sessions")]
    ImportPreviewSweep {
        /// Escalate directly to SIGKILL/taskkill force mode.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Emit machine-readable JSON output on stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum CapsuleSessionInternalCommands {
    #[command(hide = true)]
    Start {
        bundle: PathBuf,
        #[arg(long)]
        into: PathBuf,
        #[arg(long, default_value_t = false)]
        no_attach: bool,
    },
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        session: String,
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        into: PathBuf,
    },
    #[command(hide = true)]
    Attach {
        session: String,
        #[arg(long, default_value_t = false)]
        observe: bool,
    },
    #[command(hide = true)]
    Status { session: String },
    #[command(hide = true)]
    Kill { session: String },
    #[command(hide = true)]
    List,
    /// Lease helper. Spawned only by a Supervisor.
    #[command(hide = true)]
    Watchdog {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        lease_fd: i32,
    },
}

#[derive(Subcommand)]
pub(crate) enum CapsuleProtocolInternalCommands {
    /// Capture workspace State before running a command and record its PTY I/O.
    #[command(hide = true, trailing_var_arg = true)]
    Capture {
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(required = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },

    /// Restore, replay with observed egress, then keep the PTY interactive.
    #[command(hide = true)]
    Replay {
        bundle: PathBuf,
        #[arg(long)]
        into: PathBuf,
        #[arg(long, default_value_t = false)]
        no_continue: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum RuntimeInternalCommands {
    /// Probe the host and emit a `RuntimeSetupStatus` describing each runtime
    /// tool's readiness. Always exits `Ok`; the caller decides what to do based
    /// on the per-tool `action` field.
    #[command(hide = true, about = "Report host runtime-setup status (plumbing)")]
    SetupStatus {
        /// Emit machine-readable JSON (`RuntimeSetupStatus`) on stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Install Ato-managed copies of the requested language runtimes, streaming
    /// one `InstallProgress` event per phase. Only `node`, `uv` and `python`
    /// are installable; any other tool name is rejected before work starts.
    #[command(
        hide = true,
        about = "Install Ato-managed runtime tools (node|uv|python) (plumbing)"
    )]
    Install {
        /// Comma-separated tool list, e.g. `--tools node,uv`. Accepts the
        /// `nodejs` alias for node.
        #[arg(long = "tools", value_delimiter = ',', required = true)]
        tools: Vec<String>,
        /// Emit machine-readable JSON progress lines on stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Explicitly prepare host runtimes (Podman): install if missing, create and
    /// start the Ato-managed machine when needed, and verify with `podman info`.
    /// The only command that may mutate host/runtime container-engine state, and
    /// only after the user invokes it. Managed toolchains (node/uv/python) route
    /// to the same install path as `install`.
    #[command(
        hide = true,
        about = "Prepare host runtimes (podman) — install + machine setup (plumbing)"
    )]
    Prepare {
        /// Comma-separated tool list, e.g. `--tools podman`.
        #[arg(long = "tools", value_delimiter = ',', required = true)]
        tools: Vec<String>,
        /// Emit machine-readable JSON progress lines on stdout.
        #[arg(long = "emit-json", default_value_t = false)]
        emit_json: bool,
    },

    /// Repair the Ato-managed Podman machine: restart it and re-verify with
    /// `podman info`. Remediation for the "machine running but unhealthy" state.
    /// Only ever touches `ato-podman`. (#460)
    #[command(
        hide = true,
        about = "Repair the Ato-managed Podman machine (restart + verify) (plumbing)"
    )]
    RepairHostRuntime {
        /// Emit machine-readable JSON progress lines on stdout.
        #[arg(long = "emit-json", default_value_t = false)]
        emit_json: bool,
    },

    /// Resume Runtime Setup after a reboot: read the resume marker, re-check the
    /// (read-only) substrate status, and report the next step, clearing the
    /// marker once the substrate is ready or the marker is stale. (#460)
    #[command(hide = true, about = "Resume Runtime Setup after a reboot (plumbing)")]
    ResumeAfterReboot {
        /// Emit machine-readable JSON on stdout.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Execute a Windows substrate remediation: enable WSL / WSL2, write a
    /// reboot-resume marker, or repair the Ato Podman machine. The Desktop
    /// supplies elevation for actions that require admin. (#460)
    #[command(
        hide = true,
        about = "Execute a Windows substrate remediation action (plumbing)"
    )]
    PrepareWindowsSubstrate {
        /// One of: install-wsl | enable-wsl2 | reboot-required |
        /// open-virtualization-instructions | repair-podman-machine.
        #[arg(long)]
        action: String,
        /// Which surface initiated it (`onboarding` | `settings`).
        #[arg(long = "source-surface", default_value = "settings")]
        source_surface: String,
        /// Emit machine-readable JSON progress lines on stdout.
        #[arg(long = "emit-json", default_value_t = false)]
        emit_json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConsentInternalCommands {
    /// Append an ExecutionPlan consent record to
    /// `${ATO_HOME:-~/.ato}/consent/executionplan_v1.jsonl` using
    /// the same code path interactive prompts go through. Idempotent:
    /// if the matching record is already present, no new line is
    /// appended. The five identity fields must match exactly what
    /// shipped in the most recent `execution_plan_consent_required`
    /// envelope for the capsule.
    ///
    /// Owns: ATO_HOME resolution, parent-dir 0o700, file 0o600,
    /// JSONL append. The desktop must NOT write the consent file
    /// directly — call this command instead.
    #[command(
        hide = true,
        about = "Append an ExecutionPlan consent record (plumbing)"
    )]
    ApproveExecutionPlan {
        /// `plan.consent.key.scoped_id`
        #[arg(long)]
        scoped_id: String,
        /// `plan.consent.key.version`
        #[arg(long)]
        version: String,
        /// `plan.consent.key.target_label`
        #[arg(long)]
        target_label: String,
        /// `plan.consent.policy_segment_hash`
        #[arg(long)]
        policy_segment_hash: String,
        /// `plan.consent.provisioning_policy_hash`
        #[arg(long)]
        provisioning_policy_hash: String,
        /// Emit a single-line JSON envelope on stdout, parse-friendly
        /// for the desktop's CLI envelope reader. Mirrors the `--json`
        /// convention used by other plumbing commands.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
