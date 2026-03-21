use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum SourceCommands {
    SyncStatus {
        #[arg(long = "source-id")]
        source_id: String,
        #[arg(long = "sync-run-id")]
        sync_run_id: String,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Rebuild {
        #[arg(long = "source-id")]
        source_id: String,
        #[arg(long = "ref", alias = "reference")]
        reference: Option<String>,
        #[arg(long, default_value_t = false)]
        wait: bool,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
    },
}
