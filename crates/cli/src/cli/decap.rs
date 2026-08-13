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

        /// Internal detached worker marker.
        #[arg(long, hide = true, default_value_t = false)]
        worker: bool,
    },

    /// List local Capsule Sessions.
    List {
        /// Emit machine-readable JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Read output from a local Capsule run.
    Attach {
        /// Session name.
        #[arg(value_name = "NAME")]
        name: String,
    },

    /// Stop a local Capsule run.
    Stop {
        /// Session name.
        #[arg(value_name = "NAME")]
        name: String,
    },
}
