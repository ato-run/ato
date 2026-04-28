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
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Stop an ato-desktop guest session")]
    Stop {
        session_id: String,
        #[arg(long)]
        json: bool,
    },
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
