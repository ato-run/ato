use std::path::PathBuf;

use clap::Subcommand;

use super::shared::{GitMode, ShareToolRuntime};

#[derive(Subcommand, Debug)]
pub(crate) enum WorkspaceCommands {
    #[command(about = "Share your current workspace")]
    Share {
        /// Local workspace path to capture (default: current directory)
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Print the detected capture plan without writing files
        #[arg(long, default_value_t = false)]
        print_plan: bool,

        /// Scan workspace for secret patterns and show what would be included; no files written
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// How to resolve the git revision: same-commit (default) or latest-at-share
        #[arg(long, value_enum, default_value_t = GitMode::SameCommit)]
        git_mode: GitMode,

        /// Runtime strategy for install steps: auto (default), ato, or system
        #[arg(long, value_enum, default_value_t = ShareToolRuntime::Auto)]
        tool_runtime: ShareToolRuntime,

        /// Allow sharing even when repositories have uncommitted changes
        #[arg(long, default_value_t = false)]
        allow_dirty: bool,

        /// Accept all detected items without prompting (CI-friendly)
        #[arg(long, short = 'y', default_value_t = false)]
        yes: bool,

        /// Write detected share settings to capsule.toml [share] after capture
        #[arg(long, default_value_t = false)]
        save_config: bool,

        /// Include dev setup contract in the shared workspace
        #[arg(long, default_value_t = false)]
        dev: bool,
    },

    #[command(about = "Set up a shared workspace locally")]
    Setup {
        /// Local share.spec.json or share.lock.json path
        input: String,

        /// Target directory to materialize into
        #[arg(long, value_name = "PATH")]
        into: PathBuf,

        /// Print the materialization plan without executing it
        #[arg(long, default_value_t = false)]
        plan: bool,

        /// Runtime strategy for install steps: auto (default), ato, or system
        #[arg(long, value_enum, default_value_t = ShareToolRuntime::Auto)]
        tool_runtime: ShareToolRuntime,

        /// Treat any verification issue as a fatal error (exit 1)
        #[arg(long, default_value_t = false)]
        strict: bool,

        /// Execute dev setup/install steps in the materialized workspace
        #[arg(long, default_value_t = false)]
        dev: bool,
    },
}
