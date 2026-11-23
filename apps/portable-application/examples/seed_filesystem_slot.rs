//! Bind a stopped, operator-selected filesystem tree to an app's initial snapshot.
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

use anyhow::{Context, Result, ensure};
use ato_objects::PortableDependencyProfile;
use ato_portable_application::{
    bundle_sha256,
    hosted_export::{
        HOSTED_CAPTURE_SCHEMA, HostedCaptureResourceV1, HostedInstanceCaptureV1,
        build_hosted_export, portable_bundle_from_bytes,
    },
    validate_all_derivations,
};
use ato_runtime_attempt::launch::state_artifact::{STATE_ARTIFACT_FORMAT, pack_state_tree};
use clap::Parser;
use tempfile::NamedTempFile;

#[derive(Parser)]
struct Args {
    /// App-only portable bundle with a declared filesystem slot.
    source: PathBuf,
    slot: String,
    /// A cleanly stopped data directory. Its files will be embedded in the output.
    stopped_directory: PathBuf,
    /// New private output file. An existing file is never replaced.
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let source = portable_bundle_from_bytes(&fs::read(&args.source)?)?;
    ensure!(
        source.index.instance_snapshot_ref.is_none(),
        "source already contains saved data"
    );
    let routes = validate_all_derivations(&source)?;
    let slots: Vec<_> = routes
        .iter()
        .flat_map(|route| &route.derivation.state)
        .filter(|slot| slot.id == args.slot)
        .collect();
    ensure!(!slots.is_empty(), "no declared state slot with that ID");
    ensure!(
        slots
            .iter()
            .all(|slot| slot.protocol == STATE_ARTIFACT_FORMAT && slot.save.is_none()),
        "this example accepts an unversioned filesystem state slot"
    );

    let root = args.stopped_directory.canonicalize()?;
    let parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."))
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&root),
        "write the bundle outside its input tree"
    );
    let state = pack_state_tree(&root)?;
    let capture = HostedInstanceCaptureV1 {
        schema: HOSTED_CAPTURE_SCHEMA.into(),
        resources: vec![HostedCaptureResourceV1 {
            object_id: state.digest().to_owned(),
            slot: args.slot,
            protocol: STATE_ARTIFACT_FORMAT.into(),
            size: state.len() as u64,
        }],
        assets: Vec::new(),
        asset_bindings: Vec::new(),
    };
    let objects = BTreeMap::from([(state.digest().to_owned(), state.bytes().to_vec())]);
    let (bytes, saved) = build_hosted_export(
        &source,
        Some(&capture),
        &objects,
        PortableDependencyProfile::Cached,
        &BTreeMap::new(),
        &[],
    )?;
    let mut output = NamedTempFile::new_in(&parent)?;
    output.write_all(&bytes)?;
    output.as_file().sync_all()?;
    output
        .persist_noclobber(&args.output)
        .context("create private output bundle")?;
    println!("{}  {}", bundle_sha256(&bytes), args.output.display());
    println!("Contract {}", saved.index.root_contract_ref);
    Ok(())
}
