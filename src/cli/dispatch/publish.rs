use anyhow::Result;

use crate::orchestration::publish_command;

use super::Reporter;

pub(super) fn execute_publish_command(
    args: publish_command::PublishCommandArgs,
    ci: bool,
    dry_run: bool,
    force_large_payload: bool,
    json: bool,
    reporter: Reporter,
) -> Result<()> {
    if ci {
        publish_command::execute_publish_ci_command(json, force_large_payload, reporter)
    } else if dry_run {
        publish_command::execute_publish_dry_run_command(args, reporter)
    } else {
        publish_command::execute_publish_command(args, reporter)
    }
}
