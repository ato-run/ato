use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProjectCommands {
    #[command(
        about = "List experimental projection state and detect broken projections read-only"
    )]
    Ls {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ScaffoldCommands {
    Docker {
        #[arg(long, default_value = "capsule.toml")]
        manifest: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        output_dir: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}
