use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use crate::reporters;

pub(crate) struct BuildLikeCommandArgs {
    pub(crate) dir: PathBuf,
    pub(crate) init: bool,
    pub(crate) key: Option<PathBuf>,
    pub(crate) standalone: bool,
    pub(crate) force_large_payload: bool,
    pub(crate) enforcement: String,
    pub(crate) keep_failed_artifacts: bool,
    pub(crate) timings: bool,
    pub(crate) strict_v3: bool,
    pub(crate) json: bool,
    pub(crate) nacelle: Option<PathBuf>,
    pub(crate) deprecation_warning: Option<&'static str>,
    pub(crate) reporter: Arc<reporters::CliReporter>,
}

pub(crate) fn execute_build_like_command(args: BuildLikeCommandArgs) -> Result<()> {
    if let Some(warning) = args.deprecation_warning {
        eprintln!("{warning}");
    }

    let result = crate::commands::build::execute_pack_command(
        args.dir,
        args.init,
        args.key,
        args.standalone,
        args.force_large_payload,
        args.keep_failed_artifacts,
        args.strict_v3,
        args.enforcement,
        args.reporter,
        args.timings,
        args.json,
        args.nacelle,
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}

pub(crate) fn execute_validate_command(path: PathBuf, json: bool) -> Result<()> {
    crate::commands::validate::execute(path, json)?;
    Ok(())
}
