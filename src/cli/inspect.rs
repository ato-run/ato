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
}
