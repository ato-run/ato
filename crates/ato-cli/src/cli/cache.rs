use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum CacheCommands {
    /// Print cache size and reference counts as JSON.
    #[command(about = "Show A1 dependency cache statistics")]
    Stats {
        /// Pretty-print the JSON output (default: compact)
        #[arg(long, default_value_t = false)]
        pretty: bool,
    },

    /// Remove cached blobs and ref records.
    ///
    /// By default this is a user-driven reset, not a GC pass: every blob
    /// the CLI does not consider strongly referenced is removed. Use
    /// `--derivation` for surgical removal.
    #[command(about = "Clear A1 dependency cache entries")]
    Clear {
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Only remove the cache entry whose derivation_hash matches this
        /// value. The blob is removed only if no other ref still points at
        /// it.
        #[arg(long = "derivation", value_name = "HASH")]
        derivation: Option<String>,
    },
}
