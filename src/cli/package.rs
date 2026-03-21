use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum PackageCommands {
    Search {
        query: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long = "tag", value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = false)]
        no_tui: bool,
        #[arg(long, default_value_t = false)]
        show_manifest: bool,
    },
}
