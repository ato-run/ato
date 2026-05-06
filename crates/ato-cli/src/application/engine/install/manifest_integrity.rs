use super::*;

pub(crate) struct ReconstructResult {
    pub(crate) payload_tar: Vec<u8>,
    pub(crate) missing_chunks: Vec<String>,
}

pub(crate) fn manifest_distribution(
    manifest: &CapsuleManifest,
) -> Result<&capsule_core::types::DistributionInfo> {
    manifest.distribution.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{}: distribution metadata is missing from capsule.toml",
            crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
        )
    })
}

pub(crate) fn reconstruct_payload_from_local_chunks(
    cas_index: &LocalCasIndex,
    manifest: &CapsuleManifest,
) -> Result<ReconstructResult> {
    let mut payload = Vec::new();
    let mut missing = Vec::new();
    for chunk in &manifest_distribution(manifest)?.chunk_list {
        match cas_index.load_chunk_bytes(&chunk.chunk_hash)? {
            Some(bytes) => {
                if bytes.len() as u64 != chunk.length {
                    bail!(
                        "{}: chunk length mismatch for {} (expected {}, got {})",
                        crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                        chunk.chunk_hash,
                        chunk.length,
                        bytes.len()
                    );
                }
                payload.extend_from_slice(&bytes);
            }
            None => {
                missing.push(chunk.chunk_hash.clone());
            }
        }
    }
    Ok(ReconstructResult {
        payload_tar: payload,
        missing_chunks: missing,
    })
}

pub(crate) fn build_capsule_artifact(
    capsule_toml: Option<&str>,
    capsule_lock: Option<&str>,
    payload_tar_zst: &[u8],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut out);
        if let Some(manifest_toml) = capsule_toml {
            if !manifest_toml.is_empty() {
                append_capsule_entry(&mut builder, "capsule.toml", manifest_toml.as_bytes())?;
            }
        }
        if let Some(lockfile) = capsule_lock {
            if !lockfile.is_empty() {
                append_capsule_entry(&mut builder, "capsule.lock.json", lockfile.as_bytes())?;
            }
        }
        append_capsule_entry(&mut builder, "payload.tar.zst", payload_tar_zst)?;
        builder
            .finish()
            .with_context(|| "Failed to finalize reconstructed .capsule archive")?;
    }
    Ok(out)
}

pub(crate) fn append_capsule_entry(
    builder: &mut tar::Builder<&mut Vec<u8>>,
    path: &str,
    bytes: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(bytes))
        .with_context(|| format!("Failed to append {} to reconstructed artifact", path))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct YankedResponsePayload {
    #[serde(default)]
    yanked: Option<bool>,
    #[serde(default)]
    message: Option<String>,
}

pub(crate) fn parse_yanked_message(body: &str) -> Option<String> {
    let parsed: YankedResponsePayload = serde_json::from_str(body).ok()?;
    if parsed.yanked.unwrap_or(false) {
        return Some(
            parsed
                .message
                .unwrap_or_else(|| "Manifest has been yanked by the publisher.".to_string()),
        );
    }
    None
}

pub(crate) fn sweep_stale_tmp_capsules(install_dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(install_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(anyhow::anyhow!(
                "Failed to read install directory {}: {}",
                install_dir.display(),
                err
            ))
        }
    };
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "Failed to enumerate install directory {}",
                install_dir.display()
            )
        })?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(".capsule.tmp.") {
            continue;
        }
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
    Ok(())
}

pub(crate) fn write_capsule_atomic(path: &Path, bytes: &[u8], expected_blake3: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid install path without parent directory: {}",
            path.display()
        )
    })?;
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let tmp_path = parent.join(format!(".capsule.tmp.{}", hex::encode(nonce)));

    let result = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create file: {}", tmp_path.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("Failed to write file: {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync file: {}", tmp_path.display()))?;

        let computed = compute_blake3(bytes);
        if !equals_hash(expected_blake3, &computed) {
            bail!(
                "{}: computed artifact hash changed during atomic install write",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
            );
        }

        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Failed to atomically move {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

pub(crate) async fn verify_manifest_supply_chain(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    artifact_bytes: &[u8],
    allow_unverified: bool,
    allow_downgrade: bool,
) -> Result<()> {
    let base = registry.trim_end_matches('/');
    let endpoint = format!("{}/v1/manifest/epoch/resolve", base);
    let has_token = has_ato_token();
    let resolution = if requested_version.is_some() {
        resolve_manifest_target(
            client,
            base,
            scoped_ref,
            requested_version,
            has_token,
            false,
        )
        .await?
    } else {
        let response = crate::registry::http::with_ato_token(
            client
                .post(&endpoint)
                .json(&serde_json::json!({ "scoped_id": scoped_ref.scoped_id })),
        )
        .send()
        .await
        .with_context(|| "Failed to fetch manifest epoch pointer")?;
        if !response.status().is_success() {
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
                bail!(
                    "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                    crate::error_codes::ATO_ERR_AUTH_REQUIRED
                );
            }
            if allow_unverified {
                eprintln!(
                    "⚠️  manifest epoch pointer unavailable (status={}): continuing due to --allow-unverified",
                    response.status()
                );
                return Ok(());
            }
            bail!(
                "manifest epoch pointer is required for verified install (status={})",
                response.status()
            );
        }
        let epoch = response
            .json::<ManifestEpochResolveResponse>()
            .await
            .with_context(|| "Invalid manifest epoch response")?;
        verify_epoch_signature(&epoch).with_context(|| "Epoch signature verification failed")?;
        ManifestResolution::Current(epoch)
    };
    let target_manifest_hash = resolution.manifest_hash().to_string();

    let local_manifest_bytes = extract_manifest_toml_from_capsule(artifact_bytes)
        .with_context(|| "capsule.toml is required in artifact")?;
    let local_manifest: CapsuleManifest =
        toml::from_str(&local_manifest_bytes).with_context(|| "Invalid local capsule.toml")?;
    let local_manifest_hash = compute_manifest_hash_without_signatures(&local_manifest)?;
    if normalize_hash_for_compare(&local_manifest_hash)
        != normalize_hash_for_compare(&target_manifest_hash)
    {
        bail!(
            "Artifact manifest hash mismatch against resolved manifest (expected {}, got {})",
            target_manifest_hash,
            local_manifest_hash
        );
    }

    let manifest_endpoint = format!(
        "{}/v1/manifest/documents/{}",
        base,
        urlencoding::encode(&target_manifest_hash)
    );
    let manifest_response = crate::registry::http::with_ato_token(client.get(&manifest_endpoint))
        .send()
        .await
        .with_context(|| "Failed to fetch manifest payload")?;
    let manifest_status = manifest_response.status();
    if manifest_status.is_success() {
        let remote_manifest_bytes = manifest_response
            .bytes()
            .await
            .with_context(|| "Failed to read remote manifest payload")?;
        let remote_manifest_toml = String::from_utf8(remote_manifest_bytes.to_vec())
            .with_context(|| "Remote manifest payload must be UTF-8 TOML")?;
        let remote_manifest: CapsuleManifest =
            toml::from_str(&remote_manifest_toml).with_context(|| "Invalid remote capsule.toml")?;
        let remote_manifest_hash = compute_manifest_hash_without_signatures(&remote_manifest)?;
        if normalize_hash_for_compare(&remote_manifest_hash)
            != normalize_hash_for_compare(&target_manifest_hash)
        {
            bail!(
                "Remote manifest hash mismatch against resolved manifest (expected {}, got {})",
                target_manifest_hash,
                remote_manifest_hash
            );
        }
    } else if manifest_status == reqwest::StatusCode::UNAUTHORIZED && !has_token {
        bail!(
            "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
            crate::error_codes::ATO_ERR_AUTH_REQUIRED
        );
    } else {
        let body = manifest_response.text().await.unwrap_or_default();
        if let Some(message) = parse_yanked_message(&body) {
            bail!(
                "{}: {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                message
            );
        }
    }
    if !manifest_status.is_success() && !allow_unverified {
        bail!(
            "Failed to fetch registry manifest (status={})",
            manifest_status
        );
    }

    let payload_tar_bytes = extract_payload_tar_from_capsule(artifact_bytes)?;
    verify_payload_chunks(&local_manifest, &payload_tar_bytes)?;
    verify_manifest_merkle_root(&local_manifest)?;

    if let ManifestResolution::Current(epoch) = resolution {
        enforce_epoch_monotonicity(
            &scoped_ref.scoped_id,
            epoch.pointer.epoch,
            &epoch.pointer.manifest_hash,
            allow_downgrade,
        )?;
    }

    Ok(())
}

pub(crate) fn artifact_request_builder(
    client: &reqwest::Client,
    registry: &str,
    artifact_url: &str,
) -> reqwest::RequestBuilder {
    let request = client.get(artifact_url);
    if should_attach_ato_token_to_artifact_url(registry, artifact_url) {
        crate::registry::http::with_ato_token(request)
    } else {
        request
    }
}

pub(crate) fn should_attach_ato_token_to_artifact_url(registry: &str, artifact_url: &str) -> bool {
    let Ok(registry_url) = reqwest::Url::parse(registry) else {
        return false;
    };
    let Ok(artifact) = reqwest::Url::parse(artifact_url) else {
        return false;
    };
    registry_url.scheme() == artifact.scheme()
        && registry_url.host_str() == artifact.host_str()
        && registry_url.port_or_known_default() == artifact.port_or_known_default()
        && artifact.path().starts_with("/v1/capsules/")
}

pub(crate) fn has_ato_token() -> bool {
    crate::registry::http::current_ato_token().is_some()
}

pub(crate) fn verify_epoch_signature(epoch: &ManifestEpochResolveResponse) -> Result<()> {
    let pub_bytes = BASE64
        .decode(epoch.public_key.as_bytes())
        .with_context(|| "Invalid base64 public key")?;
    if pub_bytes.len() != 32 {
        bail!(
            "Invalid manifest epoch public key length: {}",
            pub_bytes.len()
        );
    }
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&pub_bytes);
    let did = public_key_to_did(&pubkey);
    if did != epoch.pointer.signer_did {
        bail!(
            "Epoch signer DID mismatch (expected {}, got {})",
            epoch.pointer.signer_did,
            did
        );
    }
    let verifying_key =
        VerifyingKey::from_bytes(&pubkey).with_context(|| "Invalid manifest epoch public key")?;
    let signature_bytes = BASE64
        .decode(epoch.pointer.signature.as_bytes())
        .with_context(|| "Invalid base64 epoch signature")?;
    if signature_bytes.len() != 64 {
        bail!(
            "Invalid manifest epoch signature length: {}",
            signature_bytes.len()
        );
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(&signature_bytes);
    let signature = Signature::from_bytes(&sig);
    let unsigned = serde_json::json!({
        "scoped_id": epoch.pointer.scoped_id,
        "epoch": epoch.pointer.epoch,
        "manifest_hash": epoch.pointer.manifest_hash,
        "prev_epoch_hash": epoch.pointer.prev_epoch_hash,
        "issued_at": epoch.pointer.issued_at,
        "signer_did": epoch.pointer.signer_did,
        "key_id": epoch.pointer.key_id,
    });
    let canonical = serde_jcs::to_vec(&unsigned)?;
    verifying_key
        .verify(&canonical, &signature)
        .with_context(|| "ed25519 verification failed")?;
    Ok(())
}

pub(crate) fn extract_manifest_toml_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let path = entry.path().context("Failed to read archive entry path")?;
        if path.to_string_lossy() == "capsule.toml" {
            let mut manifest = Vec::new();
            entry
                .read_to_end(&mut manifest)
                .context("Failed to read capsule.toml from artifact")?;
            return String::from_utf8(manifest).with_context(|| "capsule.toml must be UTF-8");
        }
    }
    bail!("Invalid artifact: capsule.toml not found in .capsule archive")
}

pub(crate) fn extract_embedded_ato_lock_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::ato_lock::AtoLock>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let path = entry.path().context("Failed to read archive entry path")?;
        let path_str = path.to_string_lossy();
        if path_str == "ato.lock.json" || path_str == "capsule.lock.json" {
            let mut lock_raw = Vec::new();
            entry
                .read_to_end(&mut lock_raw)
                .context("Failed to read embedded lock from artifact")?;
            let lock_raw =
                String::from_utf8(lock_raw).with_context(|| "embedded lock must be UTF-8")?;
            let lock = capsule_core::ato_lock::load_unvalidated_from_str(&lock_raw)
                .map_err(anyhow::Error::from)
                .with_context(|| "embedded lock is invalid")?;
            return Ok(Some(lock));
        }
    }
    Ok(None)
}

pub(crate) fn compute_manifest_hash_without_signatures(
    manifest: &CapsuleManifest,
) -> Result<String> {
    manifest_payload::compute_manifest_hash_without_signatures(manifest)
        .map_err(anyhow::Error::from)
}

pub(crate) fn verify_payload_chunks(manifest: &CapsuleManifest, payload_tar: &[u8]) -> Result<()> {
    let distribution = manifest_distribution(manifest)?;
    let mut next_offset = 0u64;
    for chunk in &distribution.chunk_list {
        if chunk.offset != next_offset {
            bail!(
                "manifest chunk_list offset mismatch: expected {}, got {}",
                next_offset,
                chunk.offset
            );
        }
        let start = chunk.offset as usize;
        let end = start.saturating_add(chunk.length as usize);
        if end > payload_tar.len() {
            bail!(
                "manifest chunk range out of bounds: {}..{} (payload={})",
                start,
                end,
                payload_tar.len()
            );
        }
        let actual = format!("blake3:{}", blake3::hash(&payload_tar[start..end]).to_hex());
        if normalize_hash_for_compare(&actual) != normalize_hash_for_compare(&chunk.chunk_hash) {
            bail!(
                "manifest chunk hash mismatch at offset {}: expected {}, got {}",
                chunk.offset,
                chunk.chunk_hash,
                actual
            );
        }
        next_offset = chunk.offset.saturating_add(chunk.length);
    }
    if next_offset != payload_tar.len() as u64 {
        bail!(
            "manifest chunk coverage mismatch: covered {}, payload {}",
            next_offset,
            payload_tar.len()
        );
    }
    Ok(())
}

pub(crate) fn verify_manifest_merkle_root(manifest: &CapsuleManifest) -> Result<()> {
    let distribution = manifest_distribution(manifest)?;
    let mut level: Vec<[u8; 32]> = manifest
        .distribution
        .as_ref()
        .expect("distribution metadata")
        .chunk_list
        .iter()
        .map(|chunk| {
            let normalized = normalize_hash_for_compare(&chunk.chunk_hash);
            let decoded = hex::decode(normalized).unwrap_or_default();
            let mut out = [0u8; 32];
            if decoded.len() == 32 {
                out.copy_from_slice(&decoded);
            }
            out
        })
        .collect();
    let actual_merkle = if level.is_empty() {
        format!("blake3:{}", blake3::hash(b"").to_hex())
    } else {
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(2));
            let mut idx = 0usize;
            while idx < level.len() {
                let left = level[idx];
                let right = if idx + 1 < level.len() {
                    level[idx + 1]
                } else {
                    level[idx]
                };
                let mut hasher = blake3::Hasher::new();
                hasher.update(&left);
                hasher.update(&right);
                let digest = hasher.finalize();
                let mut out = [0u8; 32];
                out.copy_from_slice(digest.as_bytes());
                next.push(out);
                idx += 2;
            }
            level = next;
        }
        format!("blake3:{}", hex::encode(level[0]))
    };
    if normalize_hash_for_compare(&actual_merkle)
        != normalize_hash_for_compare(&distribution.merkle_root)
    {
        bail!(
            "manifest merkle_root mismatch: expected {}, got {}",
            distribution.merkle_root,
            actual_merkle
        );
    }
    Ok(())
}

pub(crate) fn epoch_guard_path() -> PathBuf {
    capsule_core::common::paths::ato_state_dir().join("epoch-guard.json")
}

pub(crate) fn enforce_epoch_monotonicity(
    scoped_id: &str,
    epoch: u64,
    manifest_hash: &str,
    allow_downgrade: bool,
) -> Result<()> {
    enforce_epoch_monotonicity_at(
        &epoch_guard_path(),
        scoped_id,
        epoch,
        manifest_hash,
        allow_downgrade,
    )
}

pub(crate) fn enforce_epoch_monotonicity_at(
    state_path: &Path,
    scoped_id: &str,
    epoch: u64,
    manifest_hash: &str,
    allow_downgrade: bool,
) -> Result<()> {
    let mut state = load_epoch_guard_state(state_path)?;
    let manifest_norm = normalize_hash_for_compare(manifest_hash);
    let now = chrono::Utc::now().to_rfc3339();

    if let Some(previous) = state.capsules.get(scoped_id) {
        if epoch == previous.max_epoch
            && normalize_hash_for_compare(&previous.manifest_hash) != manifest_norm
        {
            bail!(
                "Epoch replay mismatch for {} at epoch {}: manifest differs from previously trusted value",
                scoped_id,
                epoch
            );
        }
        if epoch < previous.max_epoch && !allow_downgrade {
            bail!(
                "Downgrade detected for {}: remote epoch {} is older than trusted epoch {}. Re-run with --allow-downgrade to proceed.",
                scoped_id,
                epoch,
                previous.max_epoch
            );
        }
    }

    let mut should_persist = false;
    match state.capsules.get_mut(scoped_id) {
        Some(entry) => {
            if epoch > entry.max_epoch {
                entry.max_epoch = epoch;
                entry.manifest_hash = manifest_hash.to_string();
                entry.updated_at = now;
                should_persist = true;
            }
        }
        None => {
            state.capsules.insert(
                scoped_id.to_string(),
                EpochGuardEntry {
                    max_epoch: epoch,
                    manifest_hash: manifest_hash.to_string(),
                    updated_at: now,
                },
            );
            should_persist = true;
        }
    }

    if should_persist {
        write_epoch_guard_state_atomic(state_path, &state)?;
    }
    Ok(())
}

pub(crate) fn load_epoch_guard_state(path: &Path) -> Result<EpochGuardState> {
    if !path.exists() {
        return Ok(EpochGuardState::default());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read epoch guard state: {}", path.display()))?;
    if raw.trim().is_empty() {
        return Ok(EpochGuardState::default());
    }
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse epoch guard state: {}", path.display()))
}

pub(crate) fn write_epoch_guard_state_atomic(path: &Path, state: &EpochGuardState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create epoch guard state directory: {}",
                parent.display()
            )
        })?;
    }

    let payload = serde_json::to_vec_pretty(state).context("Failed to serialize epoch guard")?;
    let mut nonce = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut nonce);
    let tmp_name = format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("epoch-guard"),
        hex::encode(nonce)
    );
    let tmp_path = path.with_file_name(tmp_name);
    {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("Failed to create {}", tmp_path.display()))?;
        file.write_all(&payload)
            .with_context(|| format!("Failed to write {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to flush {}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to atomically replace epoch guard state at {}",
            path.display()
        )
    })?;
    Ok(())
}
