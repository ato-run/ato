use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ProfileCommands {
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        bio: Option<String>,
        #[arg(long)]
        avatar: Option<PathBuf>,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        website: Option<String>,
        #[arg(long)]
        github: Option<String>,
        #[arg(long)]
        twitter: Option<String>,
    },
    Show {
        #[arg()]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}
