use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum InspectCommands {
    #[command(about = "Inspect runtime requirements from capsule.toml")]
    Requirements {
        /// Local capsule path or scoped package reference such as publisher/slug
        target: String,
        /// Registry URL override
        #[arg(long)]
        registry: Option<String>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Inspect lock-first fields, provenance, and unresolved markers")]
    Lock {
        /// Project path, ato.lock.json path, or capsule.toml path
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Preview durable and ephemeral lock materialization without writing files")]
    Preview {
        /// Project path, ato.lock.json path, or capsule.toml path
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Show lock-path diagnostics with inspect/preview follow-up references")]
    Diagnostics {
        /// Project path, ato.lock.json path, or capsule.toml path
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Suggest lock-path remediation actions with source mapping")]
    Remediation {
        /// Project path, ato.lock.json path, or capsule.toml path
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}
