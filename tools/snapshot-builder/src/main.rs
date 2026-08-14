use std::path::PathBuf;

use anyhow::{Context, Result};
use ato_computation::ComputationRef;
use ato_objects::FsObjectStore;
use ato_provider_snapshot::{RealizationContract, register_materialization};

fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let computation = arguments
        .next()
        .context("usage: snapshot-builder COMPUTATION ARTIFACT...")?;
    let computation = ComputationRef::parse(computation.to_string_lossy().into_owned())?;
    let artifacts: Vec<PathBuf> = arguments.map(PathBuf::from).collect();
    let root = std::env::var_os("ATO_OBJECTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".ato/objects"));
    let objects = FsObjectStore::open(root)?;
    let reference = register_materialization(
        &computation,
        RealizationContract::host("ato-provider-snapshot"),
        &artifacts,
        &objects,
    )?;
    println!("{}", reference.content_ref());
    Ok(())
}
