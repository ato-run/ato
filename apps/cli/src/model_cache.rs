//! Explicit offline Model Set import, using the same object cache as a Worker.
//! No workload, cloud credential, provider or upstream model planner is used.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use ato_formation::model_set::ModelSetManifest;
use ato_runtime_attempt::data_plane::{ModelCache, ModelSetEntry, ObjectDelivery};
use clap::{Args, Subcommand};
use serde::Serialize;

#[derive(Subcommand)]
pub enum ModelSetCommands {
    /// Verify and cache an explicitly named Model Set from an existing directory.
    Import(ImportArgs),
}

#[derive(Args)]
pub struct ImportArgs {
    /// Canonical ato.model-set/1 manifest, carried by the application bundle.
    #[arg(long)]
    manifest: PathBuf,
    /// Expected sha256 reference of the canonical manifest.
    #[arg(long)]
    digest: String,
    /// Directory containing files at the manifest's relative paths.
    #[arg(long)]
    source: PathBuf,
    /// Parent of model-cache/. Defaults to ATO_HOME/cache.
    #[arg(long)]
    cache_root: Option<PathBuf>,
    /// Maximum logical object/partial bytes; unrelated Instance data is excluded.
    #[arg(long, default_value_t = 64 * 1024 * 1024 * 1024)]
    max_cache_bytes: u64,
}

#[derive(Serialize)]
pub struct ImportReport {
    pub model_set_ref: String,
    pub cache_usage_bytes: u64,
    pub objects: Vec<ObjectDelivery>,
}

pub fn execute(command: ModelSetCommands) -> Result<()> {
    let ModelSetCommands::Import(args) = command;
    let bytes = std::fs::read(&args.manifest).context("read Model Set manifest")?;
    let root = match args.cache_root {
        Some(root) => root,
        None => super::ato_home()?.join("cache"),
    };
    let report = import(
        &bytes,
        &args.digest,
        &args.source,
        &root,
        args.max_cache_bytes,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn import(
    bytes: &[u8],
    expected: &str,
    source: &Path,
    root: &Path,
    limit: u64,
) -> Result<ImportReport> {
    let (manifest, reference) = ModelSetManifest::from_canonical_bytes(bytes)
        .context("invalid/noncanonical Model Set manifest")?;
    ensure!(reference == expected, "model_set_digest_mismatch");
    let source = source.canonicalize().context("resolve source directory")?;
    ensure!(source.is_dir(), "model source is not a directory");
    let mut unique = BTreeMap::new();
    for entry in &manifest.objects {
        if let Some(previous) = unique.insert(entry.digest.clone(), entry.bytes) {
            ensure!(
                previous == entry.bytes,
                "model object digest has inconsistent sizes"
            );
        }
    }
    let required = unique
        .values()
        .try_fold(0u64, |sum, bytes| sum.checked_add(*bytes))
        .context("Model Set size overflow")?;
    ensure!(
        required <= limit,
        "model_cache_quota_exceeded: Model Set needs {required} bytes, limit is {limit}"
    );
    let cache = ModelCache::open(root)?;
    let mut objects = Vec::new();
    for entry in &manifest.objects {
        let entry = ModelSetEntry {
            path: entry.path.clone(),
            digest: entry.digest.clone(),
            bytes: entry.bytes,
        };
        objects.push(cache.ensure_bounded(
            &entry,
            limit,
            |start, end| {
                let source_file = source
                    .join(&entry.path)
                    .canonicalize()
                    .context("resolve model source file")?;
                ensure!(
                    source_file.starts_with(&source),
                    "model source path escapes its directory"
                );
                let mut file = File::open(&source_file).context("open model source file")?;
                let metadata = file.metadata()?;
                ensure!(
                    metadata.is_file() && metadata.len() == entry.bytes,
                    "model source size/type differs from its manifest"
                );
                file.seek(SeekFrom::Start(start))?;
                Ok(Box::new(file.take(end - start + 1)) as Box<dyn Read>)
            },
            || Ok(()),
        )?);
    }
    Ok(ImportReport {
        model_set_ref: reference,
        cache_usage_bytes: cache.usage_bytes()?,
        objects,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ato_formation::model_set::{MODEL_SET_SCHEMA, ModelSetEntry as ManifestEntry};
    use sha2::{Digest, Sha256};

    fn fixture() -> (tempfile::TempDir, ModelSetManifest) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("source")).unwrap();
        std::fs::write(root.path().join("source/model.bin"), b"weights").unwrap();
        let manifest = ModelSetManifest {
            schema: MODEL_SET_SCHEMA.to_owned(),
            objects: vec![ManifestEntry {
                path: "model.bin".to_owned(),
                digest: format!("sha256:{:x}", Sha256::digest(b"weights")),
                bytes: 7,
            }],
        };
        (root, manifest)
    }

    #[test]
    fn importing_offline_reuses_the_shared_verified_cache_without_copying_again() {
        let (root, manifest) = fixture();
        let bytes = manifest.canonical_bytes().unwrap();
        let reference = manifest.reference().unwrap();
        let source = root.path().join("source");
        let first = import(&bytes, &reference, &source, root.path(), 7).unwrap();
        assert_eq!(first.objects[0].cache, "miss");
        // Changing the upstream source cannot change a verified cache hit.
        std::fs::remove_file(source.join("model.bin")).unwrap();
        let next = import(&bytes, &reference, &source, root.path(), 7).unwrap();
        assert_eq!(next.objects[0].cache, "hit");
        assert_eq!(next.objects[0].bytes_transferred, 0);
        assert_eq!(next.cache_usage_bytes, 7);
        assert_eq!(next.model_set_ref, reference);
    }

    #[test]
    fn invalid_manifest_digest_and_quota_fail_before_creating_a_cache() {
        let (root, manifest) = fixture();
        let bytes = manifest.canonical_bytes().unwrap();
        assert!(
            import(
                &bytes,
                "sha256:wrong",
                &root.path().join("source"),
                root.path(),
                7
            )
            .is_err()
        );
        assert!(
            import(
                &bytes,
                &manifest.reference().unwrap(),
                &root.path().join("source"),
                root.path(),
                6
            )
            .is_err()
        );
        assert!(!root.path().join("model-cache").exists());
    }

    #[test]
    fn corrupt_source_is_not_published_as_a_cached_object() {
        let (root, manifest) = fixture();
        std::fs::write(root.path().join("source/model.bin"), b"changed").unwrap();
        assert!(
            import(
                &manifest.canonical_bytes().unwrap(),
                &manifest.reference().unwrap(),
                &root.path().join("source"),
                root.path(),
                7
            )
            .is_err()
        );
        let cache = ModelCache::open(root.path()).unwrap();
        assert!(
            !cache
                .object_path(&manifest.objects[0].digest)
                .unwrap()
                .exists()
        );
    }
}
