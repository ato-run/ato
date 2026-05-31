use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum CommunityCommands {
    #[command(
        about = "Submit a capsule.toml to the Ato community",
        after_help = "\
Examples:
  ato community submit github.com/usememos/memos -T ./capsule.toml
  ato community submit github.com/usememos/memos -T ./capsule.toml --dry-run
  ato community submit github.com/usememos/memos -T ./capsule.toml -y"
    )]
    Submit {
        /// Normalized source locator (e.g. github.com/owner/repo)
        source: String,

        /// Local path to the capsule.toml file to submit
        #[arg(short = 'T', long = "toml", value_name = "PATH")]
        toml_path: PathBuf,

        /// Validate and print request summary without sending
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// Skip interactive confirmation (required in non-TTY environments)
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
    },

    Receipt {
        #[command(subcommand)]
        command: ReceiptCommands,
    },
}

#[derive(Subcommand)]
pub(crate) enum ReceiptCommands {
    #[command(
        about = "Upload a verification receipt to an existing community capsule.toml",
        after_help = "\
Examples:
  ato community receipt upload ctoml_abc123 --receipt ./receipt.json
  ato community receipt upload ctoml_abc123 --receipt ./receipt.json --dry-run
  ato community receipt upload ctoml_abc123 --receipt ./receipt.json -y"
    )]
    Upload {
        /// ID of the community capsule.toml to attach the receipt to
        capsule_toml_id: String,

        /// Path to the receipt file (JSON)
        #[arg(long)]
        receipt: PathBuf,

        /// Validate and print request summary without sending
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// Skip interactive confirmation (required in non-TTY environments)
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
    },
}
