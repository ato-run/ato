use anyhow::{Context, Result};
use std::path::PathBuf;

use capsule_core::CapsuleReporter;

pub struct SignArgs {
    pub target: PathBuf,
    pub key: PathBuf,
    pub out: Option<PathBuf>,
}

pub fn execute(
    args: SignArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let target = args
        .target
        .canonicalize()
        .with_context(|| format!("Failed to resolve target: {}", args.target.display()))?;
    let key = args
        .key
        .canonicalize()
        .with_context(|| format!("Failed to resolve key: {}", args.key.display()))?;

    let sig_path =
        capsule_core::signing::sign_artifact(&target, &key, "ato-cli", args.out.clone())?;

    futures::executor::block_on(
        reporter.notify(format!("✅ Signature written: {}", sig_path.display())),
    )?;

    Ok(())
}
