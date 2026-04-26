use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    Engine {
        #[command(subcommand)]
        command: ConfigEngineCommands,
    },
    Registry {
        #[command(subcommand)]
        command: ConfigRegistryCommands,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigEngineCommands {
    Features,
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        default: bool,
    },
    #[command(about = "Download and install an engine")]
    Install {
        /// Engine name to install
        #[arg(long, default_value = "nacelle")]
        engine: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = false)]
        skip_verify: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigRegistryCommands {
    Resolve {
        domain: String,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    ClearCache,
}

#[derive(Subcommand)]
pub(crate) enum EngineCommands {
    Features,
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        default: bool,
    },
}
