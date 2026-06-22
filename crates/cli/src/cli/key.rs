use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum KeyCommands {
    Gen {
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    Sign {
        target: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    Verify {
        target: PathBuf,
        #[arg(long)]
        sig: Option<PathBuf>,
        #[arg(long)]
        signer: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
