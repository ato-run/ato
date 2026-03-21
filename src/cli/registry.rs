use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum RegistryCommands {
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
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        #[arg(long, default_value = "~/.ato/local-registry")]
        data_dir: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long)]
        auth_token: Option<String>,
    },
}
