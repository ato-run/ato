use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum StateCommands {
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        owner_scope: Option<String>,
        #[arg(long)]
        state_name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        state_ref: String,
        #[arg(long)]
        json: bool,
    },
    Register {
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long = "name")]
        state_name: String,
        #[arg(long = "path", value_name = "/ABS/PATH")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}
