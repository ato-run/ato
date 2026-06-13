use clap::Subcommand;

/// `ato runner` — Connected Runner agent (control plane v0).
///
/// Enrolls this host as an execution device under the operator's Ato
/// account and keeps it visible (online) via heartbeats. Run leasing /
/// ready-URL reporting are later slices and are not part of these
/// commands.
#[derive(Debug, Subcommand)]
pub(crate) enum RunnerCommands {
    /// Sign in (device flow), register this host as a Connected Runner,
    /// and store its runner token locally. The user session obtained for
    /// registration is discarded — only the runner token is persisted.
    #[command(name = "login")]
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
    },

    /// Serve as a Connected Runner: send liveness heartbeats and poll for
    /// run leases, executing dispatched source runs sandboxed
    /// (`ato run <source> --sandbox`). Honest readiness only: a run is
    /// reported ready solely on the local probe-confirmed signal.
    #[command(name = "serve")]
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
}
