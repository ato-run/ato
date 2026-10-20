use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ato_objects::{
    CapsuleBundleDocument, PortableApplicationBundle, decode_capsule_bundle_document,
};
use ato_portable_application::preparation::{
    NativeProcessProposal, PreparationConstraints, assess_preparation, form_native_candidate,
};
use clap::Args;

#[derive(Debug, Args)]
pub struct PrepareArgs {
    pub capsule: PathBuf,
    #[arg(long)]
    pub candidate: Option<PathBuf>,
    #[arg(long)]
    pub no_vm: bool,
    #[arg(long)]
    pub offline_after_save: bool,
    #[arg(long)]
    pub derivation: Option<String>,
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Args)]
pub struct FormNativeArgs {
    pub capsule: PathBuf,
    #[arg(long)]
    pub workspace: PathBuf,
    #[arg(long)]
    pub proposal: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
}

fn decode(bytes: &[u8]) -> Result<PortableApplicationBundle> {
    match decode_capsule_bundle_document(bytes)? {
        CapsuleBundleDocument::PortableApplicationV3(bundle)
        | CapsuleBundleDocument::PortableApplicationV4(bundle) => Ok(bundle),
        _ => bail!("local preparation requires a portable application bundle"),
    }
}

pub fn prepare(args: PrepareArgs) -> Result<()> {
    let original_bytes = fs::read(&args.capsule).context("read frozen preparation input")?;
    let original = decode(&original_bytes)?;
    let candidate_bytes = args.candidate.as_ref().map(fs::read).transpose()?;
    let candidate = candidate_bytes
        .as_ref()
        .map(|bytes| decode(bytes))
        .transpose()?;
    let report = assess_preparation(
        &original,
        candidate.as_ref(),
        PreparationConstraints {
            no_vm: args.no_vm,
            offline_after_save: args.offline_after_save,
        },
        args.derivation.as_deref(),
    )?;
    if report.status == "ready" {
        fs::write(
            &args.output,
            candidate_bytes.as_ref().unwrap_or(&original_bytes),
        )?;
    }
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

pub fn form_native(args: FormNativeArgs) -> Result<()> {
    let original = decode(&fs::read(args.capsule)?)?;
    let proposal: NativeProcessProposal = serde_json::from_slice(&fs::read(args.proposal)?)?;
    let (bytes, _) = form_native_candidate(&original, &args.workspace, proposal)?;
    fs::write(args.output, bytes)?;
    Ok(())
}
