use clap::Subcommand;

/// `ato runner` — Connected Runner agent and host provisioning.
///
/// Enrolls this host as an execution device under the operator's Ato
/// account, keeps it visible (online) via heartbeats, and provides
/// GPU host provisioning (`provision`) and health checking (`doctor`)
/// for Ubuntu + NVIDIA systems.
#[derive(Debug, Subcommand)]
pub(crate) enum RunnerCommands {
    /// Sign in (device flow), register this host as a Connected Runner,
    /// and store its runner token locally. The user session obtained for
    /// registration is discarded — only the runner token is persisted.
    #[command(name = "login", hide = true)]
    Login {
        /// Store API base URL (default: ATO_STORE_API_URL or https://api.ato.run)
        #[arg(long, value_name = "URL")]
        api_base: Option<String>,

        /// Store site base URL used for the browser sign-in page
        /// (default: ATO_STORE_SITE_URL or https://ato.run)
        #[arg(long, value_name = "URL")]
        site_base: Option<String>,

        /// Display name shown in the PWA runner list (default: hostname)
        #[arg(long, value_name = "NAME")]
        display_name: Option<String>,

        /// Absolute, non-loopback base URL where this runner will expose
        /// ready apps (reverse proxy / tunnel). Optional in this slice.
        #[arg(long, value_name = "URL")]
        public_base_url: Option<String>,

        /// Print the sign-in URL instead of opening a browser
        #[arg(long, default_value_t = false)]
        headless: bool,

        /// Headless hosted-runner enrollment: exchange this single-use
        /// enrollment token (`ato_enr_…`, minted by the control plane and
        /// injected via cloud-init) for a runner token, skipping the operator
        /// device-flow sign-in. Used by Managed Cloud VMs. The token is never
        /// printed or persisted — only the returned runner token is saved.
        /// Falls back to the `ATO_RUNNER_ENROLLMENT_TOKEN` env var when this
        /// flag is absent (explicit flag wins); see run_login.
        #[arg(long, value_name = "TOKEN")]
        enrollment_token: Option<String>,
    },

    /// Serve as a Connected Runner: send liveness heartbeats and poll for
    /// run leases, executing dispatched source runs sandboxed
    /// (`ato run <source> --sandbox`). Honest readiness only: a run is
    /// reported ready solely on the local probe-confirmed signal.
    #[command(name = "serve", hide = true)]
    Serve {
        /// Override the API base stored at login
        #[arg(long, value_name = "URL")]
        api_base: Option<String>,

        /// NOTE: display name is fixed at registration; passing a different
        /// value prints a notice (re-run `ato runner login` to rename).
        #[arg(long, value_name = "NAME")]
        display_name: Option<String>,

        /// Advertise this public base URL on every heartbeat
        #[arg(long, value_name = "URL")]
        public_base_url: Option<String>,

        /// Local address the BASE root proxy listens on. Slot `i` listens on
        /// `base_port + i`, so concurrent runs never collide on a port (the
        /// operator's tunnel/LB forwards public_base_url to the base port).
        /// Default: 127.0.0.1:8420
        #[arg(long, value_name = "ADDR:PORT")]
        proxy_listen: Option<String>,

        /// Max concurrent run slots this device serves (default 1; also
        /// `ATO_RUNNER_MAX_SLOTS`). Each slot runs one app and owns proxy port
        /// `base_port + slot_index`. Values are clamped to [1, 64].
        #[arg(long, value_name = "N")]
        max_slots: Option<usize>,

        /// Template for each slot's public URL, with `{port}` (the slot's proxy
        /// port) and/or `{slot}` (its index) placeholders — e.g.
        /// "https://{slot}.runner.example.com/" or
        /// "https://runner.example.com:{port}/". Set this only when your ingress
        /// maps each slot's proxy port to that URL; without it, only slot 0 gets
        /// a public URL (from --public-base-url) and other slots report ready
        /// without one. Also `ATO_RUNNER_PUBLIC_URL_TEMPLATE`.
        #[arg(long, value_name = "TEMPLATE")]
        public_url_template: Option<String>,
    },

    /// Check GPU host readiness for LLM workloads. Read-only — never
    /// mutates host state. Probes OS, NVIDIA driver, Docker, and the
    /// NVIDIA Container Toolkit, then prints a diagnostic table with
    /// recommended next steps.
    #[command(name = "doctor", about = "Check GPU host readiness for LLM workloads")]
    Doctor {
        /// Emit machine-readable JSON on stdout instead of a human table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Install the NVIDIA driver, Docker Engine, and NVIDIA Container
    /// Toolkit on this Ubuntu host so it can run GPU LLM capsules.
    /// Requires root (sudo). Idempotent: skips components already
    /// installed unless `--force` is given.
    #[command(
        name = "provision",
        about = "Install NVIDIA driver + Docker + nvidia-container-toolkit (Ubuntu)"
    )]
    Provision {
        /// GPU provisioning profile (v0: `nvidia-ubuntu` only).
        #[arg(long, value_name = "PROFILE", default_value = "nvidia-ubuntu")]
        profile: String,

        /// Reinstall even if a component is already present.
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Resume after a reboot — re-check state and continue from the
        /// last completed phase (reads the provision marker).
        #[arg(long, default_value_t = false)]
        resume: bool,

        /// Enroll as a Connected Runner after successful provision by
        /// delegating to `ato runner login`. Pass an optional display
        /// name; if absent the hostname is used.
        #[arg(long, value_name = "NAME")]
        enroll: Option<Option<String>>,

        /// Emit JSON progress events on stdout (one per phase).
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Show the commands that would be executed without running
        /// anything. Useful for review before applying changes.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}
