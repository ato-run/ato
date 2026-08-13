use std::path::PathBuf;

use clap::Subcommand;

/// Open and manage portable Capsule Sessions.
#[derive(Subcommand)]
pub(crate) enum DecapCommands {
    /// Open a portable Capsule as a Session.
    Start {
        /// Portable `.capsule` file to open.
        #[arg(value_name = "CAPSULE")]
        capsule: PathBuf,

        /// Keep the Session in the background even when it is interactive.
        #[arg(long, default_value_t = false)]
        detach: bool,

        /// Friendly local name. Defaults to the Capsule filename.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },

    /// List local Capsule Sessions.
    List {
        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Attach to an interactive Capsule Session.
    Attach {
        /// Session name.
        #[arg(value_name = "NAME")]
        name: String,
    },

    /// Stop a Capsule Session.
    Stop {
        /// Session name.
        #[arg(value_name = "NAME")]
        name: String,
    },
}
