use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ReceiptsCommands {
    #[command(
        about = "Diff two execution receipts and explain which graph components drifted",
        long_about = "Compare two execution receipt JSON files and report the specific launch-graph \
                      components that changed — nodes, edges, and declared/resolved facet fields — \
                      classified as DeclaredDrift (the requested launch changed) or ResolvedDrift \
                      (Ato resolved different concrete objects). Reports component-level differences \
                      rather than only that execution_id changed. Does not perform runtime \
                      observation: ObservedDrift is reserved and never emitted (#496)."
    )]
    Diff {
        /// Path to the older execution receipt JSON file
        #[arg(value_name = "OLD_RECEIPT")]
        old: PathBuf,

        /// Path to the newer execution receipt JSON file
        #[arg(value_name = "NEW_RECEIPT")]
        new: PathBuf,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}
