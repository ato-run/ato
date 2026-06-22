use clap::{Subcommand, ValueEnum};

#[derive(Subcommand)]
pub(crate) enum AppCommands {
    #[command(about = "Resolve a capsule-aware ato-desktop handle into a launch preview")]
    Resolve {
        handle: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Fetch the latest published version of a capsule from the registry, \
                 ignoring the local cache. Used by ato-desktop to surface update prompts."
    )]
    Latest {
        handle: String,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Manage an ato-desktop guest session")]
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },

    #[command(about = "Read app-scoped bootstrap state and health")]
    Status {
        package_id: String,
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Finalize first-run personalization for an installed app")]
    Bootstrap {
        package_id: String,
        #[arg(long, default_value_t = false)]
        finalize: bool,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long = "model-tier", value_enum)]
        model_tier: Option<ModelTierArg>,
        #[arg(long = "privacy-mode", value_enum)]
        privacy_mode: Option<PrivacyModeArg>,
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Run a narrow repair action for an installed app")]
    Repair {
        package_id: String,
        #[arg(long, value_enum)]
        action: RepairActionArg,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SessionCommands {
    #[command(about = "Start an ato-desktop guest session from a capsule handle or local path")]
    Start {
        handle: String,
        #[arg(long)]
        target: Option<String>,
        /// Use a specific community capsule.toml recipe by ID (e.g. `ctoml_xxx`).
        /// The CLI fetches the recipe, validates its source matches the handle, and
        /// uses it as the launch manifest instead of running discovery.
        #[arg(
            long = "community-toml-id",
            conflicts_with = "from_materialized_record"
        )]
        community_toml_id: Option<String>,
        #[arg(
            long = "attach-state",
            value_name = "STATE:PATH",
            value_parser = validate_attach_state_arg
        )]
        attach_state: Vec<String>,
        #[arg(long = "from-materialized-record")]
        from_materialized_record: Option<String>,
        #[arg(long = "run-config-hash")]
        run_config_hash: Option<String>,
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Stop an ato-desktop guest session")]
    Stop {
        session_id: String,
        #[arg(long)]
        json: bool,
    },

    #[command(
        hide = true,
        about = "Watch an ato-desktop parent process and stop a session when it exits"
    )]
    WatchParent {
        session_id: String,
        #[arg(long = "parent-pid")]
        parent_pid: u32,
        #[arg(long = "parent-start-time-unix-ms")]
        parent_start_time_unix_ms: Option<u64>,
        #[arg(long = "poll-ms", default_value_t = 500)]
        poll_ms: u64,
    },
}

fn validate_attach_state_arg(value: &str) -> std::result::Result<String, String> {
    let (state_name, path) = value
        .split_once(':')
        .ok_or_else(|| "expected <state_name>:<path>".to_string())?;
    if state_name.trim().is_empty() || path.trim().is_empty() {
        return Err("expected <state_name>:<path>".to_string());
    }
    Ok(value.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ModelTierArg {
    Fast,
    Balanced,
    Fallback,
}

impl ModelTierArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum PrivacyModeArg {
    Standard,
    Strict,
}

impl PrivacyModeArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum RepairActionArg {
    #[value(name = "restart-services")]
    RestartServices,
    #[value(name = "rewrite-config")]
    RewriteConfig,
    #[value(name = "switch-model-tier")]
    SwitchModelTier,
}

impl RepairActionArg {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RestartServices => "restart-services",
            Self::RewriteConfig => "rewrite-config",
            Self::SwitchModelTier => "switch-model-tier",
        }
    }
}
