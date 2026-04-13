use std::sync::Arc;

use anyhow::Result;

use crate::application::share;
use crate::cli::shared::{GitMode, ShareToolRuntime};
use crate::reporters::CliReporter;

pub(crate) struct EncapCommandArgs {
    pub(crate) path: std::path::PathBuf,
    pub(crate) share: bool,
    pub(crate) save_only: bool,
    pub(crate) print_plan: bool,
    pub(crate) git_mode: GitMode,
    pub(crate) tool_runtime: ShareToolRuntime,
    pub(crate) allow_dirty: bool,
    pub(crate) reporter: Arc<CliReporter>,
}

pub(crate) struct DecapCommandArgs {
    pub(crate) input: String,
    pub(crate) into: std::path::PathBuf,
    pub(crate) plan: bool,
    pub(crate) tool_runtime: ShareToolRuntime,
    pub(crate) strict: bool,
    pub(crate) reporter: Arc<CliReporter>,
}

pub(crate) fn execute_encap_command(args: EncapCommandArgs) -> Result<()> {
    share::execute_encap(
        share::EncapArgs {
            path: args.path,
            share: args.share,
            save_only: args.save_only,
            print_plan: args.print_plan,
            git_mode: args.git_mode,
            tool_runtime: args.tool_runtime,
            allow_dirty: args.allow_dirty,
        },
        args.reporter,
    )
}

pub(crate) fn execute_decap_command(args: DecapCommandArgs) -> Result<()> {
    share::execute_decap(
        share::DecapArgs {
            input: args.input,
            into: args.into,
            plan: args.plan,
            tool_runtime: args.tool_runtime,
            strict: args.strict,
        },
        args.reporter,
    )
}
