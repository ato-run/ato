use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ConsoleCommands {
    /// Open the Ato Web Console connected to the local Runtime
    Open {
        /// Local registry endpoint (default: http://127.0.0.1:8787)
        #[arg(long, default_value = "http://127.0.0.1:8787")]
        endpoint: String,

        /// Bearer token for Runtime Control API.
        /// Falls back to ATO_REGISTRY_TOKEN env var, then
        /// ~/.ato/local-registry/.console-token.
        #[arg(long)]
        token: Option<String>,

        /// Override the PWA URL (default: https://app.ato.run)
        #[arg(long, default_value = "https://app.ato.run")]
        app_url: String,

        /// Print the full console URL (including token) to stdout instead of
        /// opening the browser.  The URL contains the bearer token — treat it
        /// as sensitive.
        #[arg(long)]
        print_url: bool,
    },
}
