use super::*;

pub(crate) async fn resolve_registry_url(
    registry_url: Option<&str>,
    emit_log: bool,
) -> Result<String> {
    crate::registry::url::resolve_registry_url_with_log(registry_url, emit_log).await
}

pub(crate) async fn resolve_manifest_target(
    client: &reqwest::Client,
    base: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    has_token: bool,
    require_current_epoch: bool,
) -> Result<ManifestResolution> {
    if let Some(version) = requested_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let endpoint = format!(
            "{}/v1/manifest/resolve/{}/{}/{}",
            base,
            urlencoding::encode(&scoped_ref.publisher),
            urlencoding::encode(&scoped_ref.slug),
            urlencoding::encode(version)
        );
        let response = crate::registry::http::with_ato_token(client.get(&endpoint))
            .send()
            .await
            .with_context(|| "Failed to resolve versioned manifest hash")?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && !has_token {
            bail!(
                "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                crate::error_codes::ATO_ERR_AUTH_REQUIRED
            );
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            if let Some(message) = parse_yanked_message(&body) {
                bail!(
                    "{}: {}",
                    crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                    message
                );
            }
            bail!(
                "Failed to resolve manifest for {}@{} (status={}): {}",
                scoped_ref.scoped_id,
                version,
                status,
                body
            );
        }
        let payload = response
            .json::<VersionManifestResolveResponse>()
            .await
            .with_context(|| "Invalid version resolve response")?;
        if payload.scoped_id != scoped_ref.scoped_id {
            bail!(
                "version resolve scoped_id mismatch (expected {}, got {})",
                scoped_ref.scoped_id,
                payload.scoped_id
            );
        }
        if payload.version != version {
            bail!(
                "version resolve mismatch (expected {}, got {})",
                version,
                payload.version
            );
        }
        if let Some(yanked_at) = payload.yanked_at.as_deref() {
            bail!(
                "{}: manifest has been yanked by the publisher at {}",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                yanked_at
            );
        }
        return Ok(ManifestResolution::Version(payload));
    }

    let epoch_endpoint = format!("{}/v1/manifest/epoch/resolve", base);
    let epoch_response = crate::registry::http::with_ato_token(
        client
            .post(&epoch_endpoint)
            .json(&serde_json::json!({ "scoped_id": scoped_ref.scoped_id })),
    )
    .send()
    .await
    .with_context(|| "Failed to fetch manifest epoch pointer")?;
    if !epoch_response.status().is_success() {
        if epoch_response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
            bail!(
                "{}: registry requires authentication for manifest read APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
                crate::error_codes::ATO_ERR_AUTH_REQUIRED
            );
        }
        if require_current_epoch {
            bail!(
                "manifest epoch pointer is required for delta install (status={})",
                epoch_response.status()
            );
        }
        bail!(
            "manifest epoch pointer is required for verified install (status={})",
            epoch_response.status()
        );
    }
    let epoch = epoch_response
        .json::<ManifestEpochResolveResponse>()
        .await
        .with_context(|| "Invalid manifest epoch response")?;
    verify_epoch_signature(&epoch).with_context(|| "Epoch signature verification failed")?;
    Ok(ManifestResolution::Current(epoch))
}

pub(crate) async fn install_manifest_delta_path(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
    capsule_toml: Option<&str>,
    capsule_lock: Option<&str>,
) -> Result<DeltaInstallResult> {
    let mut lease_id: Option<String> = None;
    let result = install_manifest_delta_path_inner(
        client,
        registry,
        scoped_ref,
        requested_version,
        capsule_toml,
        capsule_lock,
        &mut lease_id,
    )
    .await;
    if let Some(lease_id) = lease_id {
        let _ = release_lease_best_effort(client, registry, &lease_id).await;
    }
    match result {
        Ok(result) => Ok(result),
        Err(err) if is_manifest_api_unsupported_error(&err) => {
            download_capsule_artifact_via_distribution(
                client,
                registry,
                scoped_ref,
                requested_version,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

#[derive(Debug, Deserialize)]
struct RegistryDistributionResponse {
    version: String,
    artifact_url: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    blake3: Option<String>,
}

pub(crate) fn is_manifest_api_unsupported_error(err: &anyhow::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    (message.contains("endpoint not found") || message.contains("status=404"))
        && (message.contains("manifest") || message.contains("epoch pointer"))
}

pub(crate) async fn download_capsule_artifact_via_distribution(
    client: &reqwest::Client,
    registry: &str,
    scoped_ref: &ScopedCapsuleRef,
    requested_version: Option<&str>,
) -> Result<DeltaInstallResult> {
    let base = registry.trim_end_matches('/');
    let mut distribution_url = format!(
        "{}/v1/capsules/by/{}/{}/distributions",
        base,
        urlencoding::encode(&scoped_ref.publisher),
        urlencoding::encode(&scoped_ref.slug)
    );
    if let Some(version) = requested_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        distribution_url.push_str(&format!("?version={}", urlencoding::encode(version)));
    }

    let has_token = has_ato_token();
    let distribution_response =
        crate::registry::http::with_ato_token(client.get(&distribution_url))
            .send()
            .await
            .with_context(|| "Failed to resolve distribution fallback for install")?;
    if distribution_response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
        bail!(
            "{}: registry requires authentication for capsule download APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
            crate::error_codes::ATO_ERR_AUTH_REQUIRED
        );
    }
    let distribution = distribution_response
        .error_for_status()
        .with_context(|| {
            format!(
                "Failed to resolve direct download fallback for {}",
                scoped_ref.scoped_id
            )
        })?
        .json::<RegistryDistributionResponse>()
        .await
        .with_context(|| "Invalid distribution fallback response")?;

    let artifact_request = artifact_request_builder(client, registry, &distribution.artifact_url);
    let artifact_response = artifact_request
        .send()
        .await
        .with_context(|| "Failed to download artifact for direct install fallback")?;
    if artifact_response.status() == reqwest::StatusCode::UNAUTHORIZED && !has_token {
        bail!(
            "{}: registry requires authentication for capsule download APIs. Run `ato login` or set `ATO_TOKEN=<token>`.",
            crate::error_codes::ATO_ERR_AUTH_REQUIRED
        );
    }
    let artifact_status = artifact_response.status();
    if !artifact_status.is_success() {
        let body = artifact_response
            .text()
            .await
            .unwrap_or_else(|_| String::new());
        let body = body.trim();
        if body.is_empty() {
            bail!(
                "Artifact download fallback failed (status={})",
                artifact_status
            );
        }
        bail!(
            "Artifact download fallback failed (status={}): {}",
            artifact_status,
            body
        );
    }
    let bytes = artifact_response
        .bytes()
        .await
        .with_context(|| "Failed to read downloaded artifact body")?
        .to_vec();

    if let Some(expected_sha256) = distribution.sha256.as_deref() {
        let computed_sha256 = compute_sha256(&bytes);
        if !equals_hash(expected_sha256, &computed_sha256) {
            bail!(
                "{}: downloaded artifact sha256 mismatch during install fallback",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
            );
        }
    }
    if let Some(expected_blake3) = distribution.blake3.as_deref() {
        let computed_blake3 = compute_blake3(&bytes);
        if !equals_hash(expected_blake3, &computed_blake3) {
            bail!(
                "{}: downloaded artifact blake3 mismatch during install fallback",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
            );
        }
    }

    Ok(DeltaInstallResult::DownloadedArtifact {
        bytes,
        file_name: distribution
            .file_name
            .unwrap_or_else(|| format!("{}-{}.capsule", scoped_ref.slug, distribution.version)),
    })
}
