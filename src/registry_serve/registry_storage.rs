use super::*;

pub(super) fn store_metadata_key(publisher: &str, slug: &str) -> String {
    format!("{}/{}", publisher, slug)
}

pub(super) fn runtime_config_key(publisher: &str, slug: &str) -> String {
    format!("{}/{}", publisher, slug)
}

pub(super) fn get_store_metadata_entry<'a>(
    index: &'a StoreMetadataIndex,
    publisher: &str,
    slug: &str,
) -> Option<&'a StoreMetadataEntry> {
    index.entries.get(&store_metadata_key(publisher, slug))
}

pub(super) fn get_runtime_config_entry<'a>(
    index: &'a RuntimeConfigIndex,
    publisher: &str,
    slug: &str,
) -> Option<&'a CapsuleRuntimeConfig> {
    index.entries.get(&runtime_config_key(publisher, slug))
}

pub(super) fn metadata_icon_url(
    base_url: &str,
    publisher: &str,
    slug: &str,
    icon_path: Option<&str>,
) -> Option<String> {
    icon_path.map(|_| {
        format!(
            "{}/v1/local/capsules/by/{}/{}/store-icon",
            base_url.trim_end_matches('/'),
            urlencoding::encode(publisher),
            urlencoding::encode(slug),
        )
    })
}

pub(super) fn metadata_to_payload(
    metadata: Option<&StoreMetadataEntry>,
    base_url: &str,
    publisher: &str,
    slug: &str,
) -> Option<StoreMetadataPayload> {
    metadata.map(|entry| {
        let icon_path = entry.icon_path.clone();
        StoreMetadataPayload {
            icon_url: metadata_icon_url(base_url, publisher, slug, icon_path.as_deref()),
            icon_path,
            text: entry.text.clone(),
        }
    })
}

pub(super) fn append_store_metadata_section(
    readme_markdown: Option<String>,
    metadata: Option<&StoreMetadataEntry>,
) -> Option<String> {
    let Some(entry) = metadata else {
        return readme_markdown;
    };
    if entry.icon_path.is_none() && entry.text.is_none() {
        return readme_markdown;
    }

    let mut section_lines = vec!["## store.metadata".to_string(), "".to_string()];
    if let Some(icon_path) = entry.icon_path.as_ref() {
        section_lines.push(format!("- file_path: `{}`", icon_path));
    }
    if let Some(text) = entry.text.as_ref() {
        section_lines.push(format!("- text: {}", text));
    }
    let section = section_lines.join("\n");
    match readme_markdown {
        Some(existing) if !existing.trim().is_empty() => {
            Some(format!("{}\n\n{}", existing.trim_end(), section))
        }
        _ => Some(section),
    }
}

pub(super) fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn publisher_info(handle: &str) -> PublisherInfo {
    PublisherInfo {
        handle: handle.to_string(),
        author_did: format!("did:key:local:{}", handle),
        verified: true,
    }
}

pub(super) fn stored_to_search_row(
    capsule: &StoredCapsule,
    metadata: Option<&StoreMetadataEntry>,
    base_url: &str,
) -> SearchCapsuleRow {
    let scoped_id = format!("{}/{}", capsule.publisher, capsule.slug);
    let description = metadata
        .and_then(|entry| entry.text.as_ref())
        .map(String::as_str)
        .unwrap_or(capsule.description.as_str())
        .to_string();
    let latest_size_bytes = capsule
        .releases
        .iter()
        .find(|release| release.version == capsule.latest_version)
        .or_else(|| capsule.releases.last())
        .map(|release| release.size_bytes)
        .unwrap_or(0);
    let store_metadata = metadata_to_payload(metadata, base_url, &capsule.publisher, &capsule.slug);
    SearchCapsuleRow {
        id: capsule.id.clone(),
        slug: capsule.slug.clone(),
        scoped_id: scoped_id.clone(),
        scoped_id_camel: scoped_id,
        name: capsule.name.clone(),
        description,
        category: capsule.category.clone(),
        capsule_type: capsule.capsule_type.clone(),
        price: capsule.price,
        currency: capsule.currency.clone(),
        publisher: publisher_info(&capsule.publisher),
        latest_version: capsule.latest_version.clone(),
        latest_size_bytes,
        downloads: capsule.downloads,
        created_at: capsule.created_at.clone(),
        updated_at: capsule.updated_at.clone(),
        store_metadata,
    }
}

#[cfg(test)]
pub(super) fn upsert_capsule(
    index: &mut RegistryIndex,
    publisher: &str,
    slug: &str,
    name: &str,
    description: &str,
    release: StoredRelease,
    now: &str,
) {
    if let Some(capsule) = index
        .capsules
        .iter_mut()
        .find(|c| c.publisher == publisher && c.slug == slug)
    {
        capsule.latest_version = release.version.clone();
        capsule.updated_at = now.to_string();
        capsule.releases.push(release);
        return;
    }

    index.capsules.push(StoredCapsule {
        id: format!("local-{}-{}", publisher, slug),
        publisher: publisher.to_string(),
        slug: slug.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        category: "tools".to_string(),
        capsule_type: "app".to_string(),
        price: 0,
        currency: "usd".to_string(),
        latest_version: release.version.clone(),
        releases: vec![release],
        downloads: 0,
        created_at: now.to_string(),
        updated_at: now.to_string(),
    });
}

#[cfg(test)]
pub(super) fn has_release_version(
    index: &RegistryIndex,
    publisher: &str,
    slug: &str,
    version: &str,
) -> bool {
    find_release_by_version(index, publisher, slug, version).is_some()
}

#[cfg(test)]
pub(super) fn find_release_by_version<'a>(
    index: &'a RegistryIndex,
    publisher: &str,
    slug: &str,
    version: &str,
) -> Option<&'a StoredRelease> {
    index
        .capsules
        .iter()
        .find(|capsule| capsule.publisher == publisher && capsule.slug == slug)
        .and_then(|capsule| {
            capsule
                .releases
                .iter()
                .find(|release| release.version == version)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExistingReleaseOutcome {
    Reuse,
    Conflict(&'static str),
}

pub(super) fn existing_release_outcome(
    existing_sha256: &str,
    allow_existing: bool,
    actual_sha: &str,
) -> ExistingReleaseOutcome {
    if !allow_existing {
        return ExistingReleaseOutcome::Conflict("same version is already published");
    }

    if equals_hash(existing_sha256, actual_sha) {
        return ExistingReleaseOutcome::Reuse;
    }

    ExistingReleaseOutcome::Conflict("same version is already published (sha256 mismatch)")
}

pub(super) fn verify_uploaded_chunk(
    raw_hash: &str,
    raw_size: u32,
    zstd_bytes: &[u8],
) -> std::result::Result<(), String> {
    let mut decoder = zstd::stream::Decoder::new(Cursor::new(zstd_bytes))
        .map_err(|e| format!("failed to initialize zstd decoder: {}", e))?;

    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| format!("failed to decode zstd chunk: {}", e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }

    if total != raw_size as u64 {
        return Err(format!(
            "raw size mismatch: expected {} got {}",
            raw_size, total
        ));
    }

    let computed = format!("blake3:{}", hasher.finalize().to_hex());
    if computed != raw_hash {
        return Err(format!(
            "raw hash mismatch: expected {} got {}",
            raw_hash, computed
        ));
    }
    Ok(())
}

pub(super) fn registry_cas_store(data_dir: &Path) -> Result<CasStore> {
    CasStore::new(data_dir.join("cas")).map_err(|e| anyhow::anyhow!("{}", e))
}

pub(super) fn parse_artifact_manifest(bytes: &[u8]) -> Result<ArtifactMeta> {
    let manifest = extract_manifest_from_capsule(bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("{}", err))?;
    Ok(ArtifactMeta {
        name: parsed.name,
        version: parsed.version,
        description: parsed.metadata.description.unwrap_or_default(),
    })
}

pub(super) fn extract_manifest_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to iterate artifact entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid artifact entry")?;
        let entry_path = entry.path()?.to_string_lossy().to_string();
        if entry_path == "capsule.toml" {
            let mut manifest = String::new();
            entry
                .read_to_string(&mut manifest)
                .context("Failed to read capsule.toml")?;
            return Ok(manifest);
        }
    }

    bail!("capsule.toml not found in artifact")
}

pub(super) fn extract_capsule_lock_from_capsule(bytes: &[u8]) -> Option<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive.entries().ok()?;
    for entry in entries {
        let mut entry = entry.ok()?;
        let entry_path = entry.path().ok()?.to_string_lossy().to_string();
        if entry_path == "capsule.lock.json" || entry_path == "capsule.lock" {
            let mut lock = String::new();
            entry.read_to_string(&mut lock).ok()?;
            return Some(lock);
        }
    }
    None
}

pub(super) fn collect_readme_candidates<R: Read>(
    archive: &mut tar::Archive<R>,
) -> HashMap<String, Vec<u8>> {
    let mut candidates = HashMap::new();
    let Ok(entries) = archive.entries() else {
        return candidates;
    };

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let entry_path = match entry.path() {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let file_name = match entry_path.rsplit('/').next() {
            Some(name) => name.to_string(),
            None => continue,
        };
        if !README_CANDIDATES
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(file_name.as_str()))
        {
            continue;
        }

        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        if buf.len() > README_MAX_BYTES {
            buf.truncate(README_MAX_BYTES);
        }
        candidates.entry(file_name).or_insert(buf);
    }

    candidates
}

pub(super) fn extract_readme_from_capsule(bytes: &[u8]) -> Option<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut candidates = collect_readme_candidates(&mut archive);

    if candidates.is_empty() {
        let mut archive = tar::Archive::new(Cursor::new(bytes));
        let entries = archive.entries().ok()?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let entry_path = match entry.path() {
                Ok(path) => path.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            if entry_path != "payload.tar.zst" {
                continue;
            }

            let decoder = match zstd::stream::Decoder::new(entry) {
                Ok(decoder) => decoder,
                Err(_) => continue,
            };
            let mut payload_archive = tar::Archive::new(decoder);
            candidates = collect_readme_candidates(&mut payload_archive);
            if !candidates.is_empty() {
                break;
            }
        }
    }

    for candidate in README_CANDIDATES {
        if let Some((_, content)) = candidates
            .iter()
            .find(|(name, _)| candidate.eq_ignore_ascii_case(name.as_str()))
        {
            return Some(String::from_utf8_lossy(content).to_string());
        }
    }
    None
}

pub(super) type CapsuleDetailManifestParts = (
    Option<serde_json::Value>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(super) fn load_capsule_detail_manifest(
    data_dir: &Path,
    capsule: &StoredCapsule,
) -> CapsuleDetailManifestParts {
    let Some(release) = capsule
        .releases
        .iter()
        .find(|release| release.version == capsule.latest_version)
        .or_else(|| capsule.releases.last())
    else {
        return (None, None, None, None, None, None);
    };
    let path = artifact_path(
        data_dir,
        &capsule.publisher,
        &capsule.slug,
        &release.version,
        &release.file_name,
    );

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(
                "local registry failed to read artifact for detail manifest path={} error={}",
                path.display(),
                err
            );
            return (None, None, None, None, None, None);
        }
    };
    let readme_markdown = extract_readme_from_capsule(&bytes);
    let capsule_lock = extract_capsule_lock_from_capsule(&bytes);
    let readme_source = readme_markdown
        .as_ref()
        .map(|_| "artifact".to_string())
        .or_else(|| Some("none".to_string()));
    let manifest_raw = match extract_manifest_from_capsule(&bytes) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(
                "local registry failed to extract capsule.toml for {}/{}@{}: {}",
                capsule.publisher,
                capsule.slug,
                release.version,
                err
            );
            return (
                None,
                None,
                None,
                capsule_lock,
                readme_markdown,
                readme_source,
            );
        }
    };
    let parsed = toml::from_str::<toml::Value>(&manifest_raw);
    let (manifest, repository) = match parsed {
        Ok(parsed) => {
            let repository = extract_repository_from_manifest(&parsed);
            let manifest = match serde_json::to_value(parsed) {
                Ok(value) => Some(value),
                Err(err) => {
                    tracing::warn!(
                        "local registry failed to serialize manifest JSON for {}/{}@{}: {}",
                        capsule.publisher,
                        capsule.slug,
                        release.version,
                        err
                    );
                    None
                }
            };
            (manifest, repository)
        }
        Err(err) => {
            tracing::warn!(
                "local registry failed to parse capsule.toml for {}/{}@{}: {}",
                capsule.publisher,
                capsule.slug,
                release.version,
                err
            );
            (None, None)
        }
    };
    (
        manifest,
        repository,
        Some(manifest_raw),
        capsule_lock,
        readme_markdown,
        readme_source,
    )
}

pub(super) fn extract_repository_from_manifest(parsed: &toml::Value) -> Option<String> {
    parsed
        .get("metadata")
        .and_then(|v| v.get("repository"))
        .and_then(toml::Value::as_str)
        .or_else(|| parsed.get("repository").and_then(toml::Value::as_str))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

pub(super) fn expand_data_dir(raw: &str) -> Result<PathBuf> {
    if raw == "~" {
        return dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home directory"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home directory"))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(raw))
}

pub(super) fn initialize_storage(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("Failed to create data dir {}", data_dir.display()))?;
    std::fs::create_dir_all(data_dir.join("artifacts"))
        .with_context(|| format!("Failed to create artifact dir {}", data_dir.display()))?;
    let _ = RegistryStore::open(data_dir)?;
    let runtime_config_path = runtime_config_path(data_dir);
    if !runtime_config_path.exists() {
        write_runtime_config(data_dir, &RuntimeConfigIndex::default())?;
    }
    Ok(())
}

pub(super) fn runtime_config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime-config.json")
}

pub(super) fn load_index(data_dir: &Path) -> Result<RegistryIndex> {
    let store = RegistryStore::open(data_dir)?;
    let packages = store.list_registry_packages()?;
    Ok(RegistryIndex {
        schema_version: "local-registry-v1".to_string(),
        capsules: packages
            .into_iter()
            .map(|package| StoredCapsule {
                id: format!("local-{}-{}", package.publisher, package.slug),
                publisher: package.publisher,
                slug: package.slug,
                name: package.name,
                description: package.description,
                category: "tools".to_string(),
                capsule_type: "app".to_string(),
                price: 0,
                currency: "usd".to_string(),
                latest_version: package.latest_version,
                releases: package
                    .releases
                    .into_iter()
                    .map(|release| StoredRelease {
                        version: release.version,
                        file_name: release.file_name,
                        sha256: format!("sha256:{}", release.sha256),
                        blake3: format!("blake3:{}", release.blake3),
                        size_bytes: release.size_bytes,
                        signature_status: release.signature_status,
                        created_at: release.created_at,
                        payload_v3: None,
                    })
                    .collect(),
                downloads: 0,
                created_at: package.created_at,
                updated_at: package.updated_at,
            })
            .collect(),
    })
}

pub(super) fn release_manifest_rel_path(publisher: &str, slug: &str, version: &str) -> PathBuf {
    PathBuf::from("payload-v3")
        .join(publisher)
        .join(slug)
        .join(format!("{}.json", version))
}

pub(super) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("payload v3 manifest path must have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create directory {}", parent.display()))?;

    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, bytes)
        .with_context(|| format!("Failed to write temporary file {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to atomically rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

pub(super) fn load_store_metadata(data_dir: &Path) -> Result<StoreMetadataIndex> {
    let store = RegistryStore::open(data_dir)?;
    let entries = store.list_store_metadata_entries()?;
    let mut index = StoreMetadataIndex::default();
    for entry in entries {
        index.entries.insert(
            entry.scoped_id,
            StoreMetadataEntry {
                icon_path: entry.icon_path,
                text: entry.text,
                updated_at: entry.updated_at,
            },
        );
    }
    Ok(index)
}

pub(super) fn load_runtime_config(data_dir: &Path) -> Result<RuntimeConfigIndex> {
    let path = runtime_config_path(data_dir);
    if !path.exists() {
        return Ok(RuntimeConfigIndex::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let parsed = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(parsed)
}

pub(super) fn write_runtime_config(data_dir: &Path, config: &RuntimeConfigIndex) -> Result<()> {
    let path = runtime_config_path(data_dir);
    let json =
        serde_json::to_string_pretty(config).context("Failed to serialize runtime config")?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub(super) fn expand_user_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

pub(super) fn artifact_path(
    data_dir: &Path,
    publisher: &str,
    slug: &str,
    version: &str,
    file_name: &str,
) -> PathBuf {
    data_dir
        .join("artifacts")
        .join(publisher)
        .join(slug)
        .join(version)
        .join(file_name)
}

pub(super) fn resolve_run_artifact_path(
    data_dir: &Path,
    capsule: &StoredCapsule,
) -> Option<PathBuf> {
    // Prefer the freshest on-disk artifact to avoid stale legacy index snapshots.
    find_latest_capsule_artifact_on_disk(data_dir, &capsule.publisher, &capsule.slug).or_else(
        || {
            capsule
                .releases
                .iter()
                .find(|release| release.version == capsule.latest_version)
                .map(|release| {
                    artifact_path(
                        data_dir,
                        &capsule.publisher,
                        &capsule.slug,
                        &release.version,
                        &release.file_name,
                    )
                })
        },
    )
}

pub(super) fn find_latest_capsule_artifact_on_disk(
    data_dir: &Path,
    publisher: &str,
    slug: &str,
) -> Option<PathBuf> {
    let root = data_dir.join("artifacts").join(publisher).join(slug);
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;

    let versions = std::fs::read_dir(root).ok()?;
    for version_entry in versions.flatten() {
        let version_path = version_entry.path();
        if !version_path.is_dir() {
            continue;
        }
        let files = match std::fs::read_dir(&version_path) {
            Ok(files) => files,
            Err(_) => continue,
        };
        for file_entry in files.flatten() {
            let file_path = file_entry.path();
            if !file_path.is_file() {
                continue;
            }
            if file_path.extension().and_then(|ext| ext.to_str()) != Some("capsule") {
                continue;
            }
            let modified = file_entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let is_newer = newest
                .as_ref()
                .map(|(current, _)| modified > *current)
                .unwrap_or(true);
            if is_newer {
                newest = Some((modified, file_path));
            }
        }
    }

    newest.map(|(_, path)| path)
}

pub(super) fn allocate_loopback_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    if port == 0 {
        None
    } else {
        Some(port)
    }
}

pub(super) fn validate_capsule_segments(publisher: &str, slug: &str) -> Result<()> {
    let scoped = format!("{}/{}", publisher, slug);
    let _ = crate::install::parse_capsule_ref(&scoped)?;
    Ok(())
}

pub(super) fn validate_version(value: &str) -> Result<()> {
    if value.is_empty() || value.contains('/') || value.contains('\\') || value.contains("..") {
        bail!("invalid version segment");
    }
    Ok(())
}

pub(super) fn validate_file_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || !value.to_ascii_lowercase().ends_with(".capsule")
    {
        bail!("file_name must be a .capsule file name");
    }
    Ok(())
}
