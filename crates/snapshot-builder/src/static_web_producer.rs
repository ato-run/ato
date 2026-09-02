//! Static Web Publication Lane producer for the clean-build (`clean_replay`)
//! lane.
//!
//! Runs ONLY when the claim's Build Config Revision resolved to
//! `static_web`. It reads the DECLARED `[outputs.static_web]` root from the
//! built guest filesystem the v1 lane exported (never guesses `dist/`),
//! produces the immutable Static Web Bundle, and registers it with the wizard
//! API's idempotent materialization registry. Any failure fails the build
//! attempt with one of the contract's structured codes — producer success is
//! REQUIRED for build success on this lane.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest as _, Sha256};

use snapshot_builder::static_web_bundle::{ProducedStaticWebBundle, produce_static_web_bundle};
use snapshot_builder::static_web_output::{StaticWebOutputPlan, extract_static_web_output};

use crate::authoring_runtime::AuthoringWork;

/// Contract failure codes (Publication Lane contract v1). The reason reported
/// for the failed build attempt is prefixed with one of these.
pub const STATIC_WEB_OUTPUT_MISSING: &str = "static_web_output_missing";
pub const STATIC_WEB_OUTPUT_OUTSIDE_WORKSPACE: &str = "static_web_output_outside_workspace";
pub const STATIC_WEB_ENTRY_MISSING: &str = "static_web_entry_missing";
pub const STATIC_WEB_BUNDLE_INVALID: &str = "static_web_bundle_invalid";
pub const STATIC_WEB_SECRET_DETECTED: &str = "static_web_secret_detected";
pub const STATIC_WEB_PREPARE_FAILED: &str = "static_web_prepare_failed";
pub const STATIC_WEB_UPLOAD_FAILED: &str = "static_web_upload_failed";
pub const STATIC_WEB_FINALIZE_FAILED: &str = "static_web_finalize_failed";

/// Every structured code, for the failure-code mapper in `main.rs`.
pub const STATIC_WEB_FAILURE_CODES: &[&str] = &[
    STATIC_WEB_OUTPUT_MISSING,
    STATIC_WEB_OUTPUT_OUTSIDE_WORKSPACE,
    STATIC_WEB_ENTRY_MISSING,
    STATIC_WEB_BUNDLE_INVALID,
    STATIC_WEB_SECRET_DETECTED,
    STATIC_WEB_PREPARE_FAILED,
    STATIC_WEB_UPLOAD_FAILED,
    STATIC_WEB_FINALIZE_FAILED,
];

/// The exported guest filesystem places the built workspace here
/// (`rootfs_builder::V1_GUEST_WORKING_DIRECTORY`, without the leading slash so
/// it can be joined below the exported tree).
const GUEST_WORKSPACE_DIR: &str = "app";

/// Registry-blob batch ceiling (`staticWebBlobBatchSchema` max 64).
const BLOB_BATCH: usize = 64;

const MATERIALIZATION_ID_DOMAIN: &str = "ato.static-web-materialization-id/v1";

pub struct StaticWebLaneFailure {
    pub code: &'static str,
    pub detail: String,
}

impl StaticWebLaneFailure {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

pub struct StaticWebLaneInputs<'a> {
    pub api_url: &'a str,
    pub builder_token: &'a str,
    pub agent_id: &'a str,
    pub work: &'a AuthoringWork,
    /// The EXACT Effective Manifest the build was produced from.
    pub effective_manifest_toml: &'a str,
    /// The v1 lane's exported guest filesystem (`<jobdir>/v1-work/guest-rootfs`).
    pub exported_guest_rootfs: &'a Path,
    /// Where the bundle directory is produced (below the job directory).
    pub bundle_parent: &'a Path,
    pub runtime_secret_canaries: &'a [&'a [u8]],
}

/// Deterministic materialization id for one Build Config Revision:
/// `swm_` + base32(sha256(domain || build_config_revision_id)), matching the
/// registry's `^swm_[a-z2-7]{52}$`. Rerunning the same revision derives the
/// same id, so the registry's idempotent prepare/complete absorbs retries.
pub fn derive_materialization_id(build_config_revision_id: &str) -> Result<String, String> {
    let digest = format!(
        "sha256:{:x}",
        Sha256::digest(format!(
            "{MATERIALIZATION_ID_DOMAIN}\0{build_config_revision_id}"
        ))
    );
    // Reuse the delivery contract's base32 encoder (host labels are
    // `<env>-<52 base32 chars>` over the same alphabet) instead of a second
    // encoder that could drift.
    let label = capsule::contract::static_web_receipt::host_label('p', &digest)
        .map_err(|error| error.to_string())?;
    Ok(format!("swm_{}", &label["p-".len()..]))
}

/// The registry's `^swm_[a-z2-7]{52}$` shape.
fn is_registry_materialization_id(value: &str) -> bool {
    value.strip_prefix("swm_").is_some_and(|body| {
        body.len() == 52
            && body
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
    })
}

/// The server-derived materialization id embedded in the claim's effective
/// build plan (`effective_build_plan.static_web_output.materialization_id`).
/// Only a registry-shaped value is honored; anything else falls back to the
/// local derivation rather than sending an id prepare would refuse.
fn claim_materialization_id(work: &AuthoringWork) -> Option<String> {
    plan_materialization_id(work.effective_build_plan.as_ref())
}

fn plan_materialization_id(plan: Option<&serde_json::Value>) -> Option<String> {
    plan?
        .get("static_web_output")?
        .get("materialization_id")?
        .as_str()
        .filter(|value| is_registry_materialization_id(value))
        .map(str::to_owned)
}

/// Produce the bundle from the built guest filesystem and drive
/// prepare → upload-authorizations → PUT → verify → complete.
pub fn produce_and_register_static_web(
    inputs: &StaticWebLaneInputs<'_>,
) -> Result<(), StaticWebLaneFailure> {
    // ── Claim binding. The registry fences on the exact job + claim + bcrev.
    let worker_claim_id = inputs.work.worker_claim_id.as_deref().ok_or_else(|| {
        StaticWebLaneFailure::new(
            STATIC_WEB_PREPARE_FAILED,
            "static_web claim carries no worker_claim_id",
        )
    })?;
    let build_config_revision_id =
        inputs
            .work
            .build_config_revision_id
            .as_deref()
            .ok_or_else(|| {
                StaticWebLaneFailure::new(
                    STATIC_WEB_PREPARE_FAILED,
                    "static_web claim carries no build_config_revision_id",
                )
            })?;
    let plan_digest = inputs.work.plan_digest.as_deref().ok_or_else(|| {
        StaticWebLaneFailure::new(
            STATIC_WEB_PREPARE_FAILED,
            "static_web claim carries no plan_digest",
        )
    })?;

    // ── The DECLARED output root, from the manifest the build ran with.
    let manifest =
        capsule::types::manifest_v1::CapsuleManifestV1::from_toml(inputs.effective_manifest_toml)
            .map_err(|error| {
            StaticWebLaneFailure::new(
                STATIC_WEB_BUNDLE_INVALID,
                format!("re-parse the Effective Manifest: {error}"),
            )
        })?;
    let declared = manifest
        .outputs
        .as_ref()
        .and_then(|outputs| outputs.static_web.as_ref())
        .ok_or_else(|| {
            StaticWebLaneFailure::new(
                STATIC_WEB_OUTPUT_MISSING,
                "the static_web lane requires an [outputs.static_web] declaration in the \
             Effective Manifest",
            )
        })?;

    // The claim's plan-embedded id (server-derived, deterministic per Build
    // Config Revision) is used verbatim when present, so the registry row and
    // the effective build plan name the SAME materialization; the local
    // derivation covers a claim whose plan carries none. Both are stable per
    // bcrev, which is what the registry's idempotent prepare/complete keys on.
    let materialization_id = match claim_materialization_id(inputs.work) {
        Some(id) => id,
        None => derive_materialization_id(build_config_revision_id).map_err(|error| {
            StaticWebLaneFailure::new(
                STATIC_WEB_PREPARE_FAILED,
                format!("derive materialization id: {error}"),
            )
        })?,
    };

    // ── Locate the declared root inside the exported guest workspace.
    let image_output_root = if declared.root == "." {
        PathBuf::from(GUEST_WORKSPACE_DIR)
    } else {
        Path::new(GUEST_WORKSPACE_DIR).join(&declared.root)
    };
    let plan = StaticWebOutputPlan {
        materialization_id: materialization_id.clone(),
        image_output_root,
        entry_path: declared.entry_path.clone(),
        spa_fallback: declared.spa_fallback,
        connect_src: declared.connect_src.clone(),
    };
    plan.validate().map_err(|error| {
        StaticWebLaneFailure::new(
            STATIC_WEB_OUTPUT_OUTSIDE_WORKSPACE,
            format!("declared output root is not a safe workspace path: {error:#}"),
        )
    })?;
    let source = inputs.exported_guest_rootfs.join(&plan.image_output_root);
    if !source.is_dir() {
        return Err(StaticWebLaneFailure::new(
            STATIC_WEB_OUTPUT_MISSING,
            format!(
                "declared output root {:?} does not exist in the built workspace",
                declared.root
            ),
        ));
    }
    let extracted =
        extract_static_web_output(inputs.exported_guest_rootfs, &plan).map_err(|error| {
            // Symlinks/hard links escaping or replacing the tree are the
            // traversal class; the extractor refuses them all.
            StaticWebLaneFailure::new(STATIC_WEB_OUTPUT_OUTSIDE_WORKSPACE, format!("{error:#}"))
        })?;
    if !extracted.output_root().join(&declared.entry_path).is_file() {
        return Err(StaticWebLaneFailure::new(
            STATIC_WEB_ENTRY_MISSING,
            format!(
                "entry document {:?} is missing from the built output",
                declared.entry_path
            ),
        ));
    }

    // ── Produce the immutable bundle. The producer runs the authoring lane's
    // no-secret scan (live builder-credential canaries) over EVERY file.
    let produced = produce_static_web_bundle(
        &plan,
        extracted.output_root(),
        inputs.bundle_parent,
        inputs.runtime_secret_canaries,
    )
    .map_err(|error| {
        let detail = format!("{error:#}");
        if detail.contains("secret canary") {
            StaticWebLaneFailure::new(STATIC_WEB_SECRET_DETECTED, detail)
        } else if detail.contains("no entry file") {
            StaticWebLaneFailure::new(STATIC_WEB_ENTRY_MISSING, detail)
        } else {
            StaticWebLaneFailure::new(STATIC_WEB_BUNDLE_INVALID, detail)
        }
    })?;
    scan_bundle_text_for_secrets(&produced)?;

    // ── Register with the wizard API. Every call is fenced by the SAME tuple
    // the authoring endpoints use: builder bearer token + authoring lease
    // header + worker_claim_id/agent_id body fields.
    let client = RegistryClient {
        api_url: inputs.api_url,
        builder_token: inputs.builder_token,
        agent_id: inputs.agent_id,
        job_id: &inputs.work.work_id,
        lease_token: inputs.work.lease_token.expose(),
        worker_claim_id,
    };
    let manifest_digest = format!("sha256:{:x}", Sha256::digest(&produced.manifest_bytes));
    let prepare = client
        .post(
            "prepare",
            &serde_json::json!({
                "agent_id": inputs.agent_id,
                "worker_claim_id": worker_claim_id,
                "materialization_id": materialization_id,
                "build_config_revision_id": build_config_revision_id,
                "expected_plan_digest": plan_digest,
                "manifest_base64": BASE64.encode(&produced.manifest_bytes),
                "receipt_base64": BASE64.encode(&produced.receipt_bytes),
                "manifest_digest": manifest_digest,
                "receipt_digest": produced.receipt_digest,
            }),
        )
        .map_err(|error| StaticWebLaneFailure::new(STATIC_WEB_PREPARE_FAILED, error))?;
    let producer_generation = prepare
        .get("producer_generation")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            StaticWebLaneFailure::new(
                STATIC_WEB_PREPARE_FAILED,
                "prepare response omitted producer_generation",
            )
        })?;

    let blobs: Vec<(String, u64)> = produced
        .receipt
        .blobs
        .iter()
        .map(|blob| (blob.digest.clone(), blob.size))
        .collect();
    for chunk in blobs.chunks(BLOB_BATCH) {
        upload_blob_chunk(
            &client,
            &produced,
            &materialization_id,
            producer_generation,
            chunk,
        )?;
    }
    for chunk in blobs.chunks(BLOB_BATCH) {
        let verified = client
            .post(
                "blobs/verify",
                &blob_batch_body(
                    inputs.agent_id,
                    worker_claim_id,
                    &materialization_id,
                    producer_generation,
                    chunk,
                ),
            )
            .map_err(|error| StaticWebLaneFailure::new(STATIC_WEB_UPLOAD_FAILED, error))?;
        let all_verified = verified
            .get("blobs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|entries| {
                entries.len() == chunk.len()
                    && entries.iter().all(|entry| {
                        entry.get("verified").and_then(serde_json::Value::as_bool) == Some(true)
                    })
            });
        if !all_verified {
            return Err(StaticWebLaneFailure::new(
                STATIC_WEB_UPLOAD_FAILED,
                "one or more uploaded blobs failed registry verification",
            ));
        }
    }

    client
        .post(
            "complete",
            &serde_json::json!({
                "agent_id": inputs.agent_id,
                "worker_claim_id": worker_claim_id,
                "producer_generation": producer_generation,
                "materialization_id": materialization_id,
            }),
        )
        .map_err(|error| StaticWebLaneFailure::new(STATIC_WEB_FINALIZE_FAILED, error))?;
    eprintln!(
        "[builder] static web materialization {materialization_id} ready \
         (manifest {manifest_digest}, {} files)",
        produced.receipt.file_count
    );
    Ok(())
}

/// Conservative pattern scan over the bundle's TEXT payloads, on top of the
/// exact-value canary scan the bundle producer already ran: the canary scan
/// only proves the BUILDER's credentials did not leak, while a repository can
/// carry its own. Patterns are chosen to be specific enough not to fire on
/// minified frameworks: AWS access key ids and PEM private key headers.
fn scan_bundle_text_for_secrets(
    produced: &ProducedStaticWebBundle,
) -> Result<(), StaticWebLaneFailure> {
    const PEM_HEADER: &[u8] = b"-----BEGIN ";
    const PEM_PRIVATE: &[u8] = b" PRIVATE KEY-----";
    let manifest: capsule::contract::static_web_manifest::StaticWebManifestV1 =
        serde_json::from_slice(&produced.manifest_bytes).map_err(|error| {
            StaticWebLaneFailure::new(
                STATIC_WEB_BUNDLE_INVALID,
                format!("re-parse the produced static web manifest: {error}"),
            )
        })?;
    for (path, file) in &manifest.files {
        if !is_text_media_type(&file.media_type) {
            continue;
        }
        let hex = file
            .blob
            .strip_prefix("sha256:")
            .unwrap_or(file.blob.as_str());
        let bytes = match std::fs::read(produced.bundle_root.join("blobs/sha256").join(hex)) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Err(StaticWebLaneFailure::new(
                    STATIC_WEB_BUNDLE_INVALID,
                    format!("re-read bundle blob for {path}: {error}"),
                ));
            }
        };
        let has_pem_private_key = bytes
            .windows(PEM_HEADER.len())
            .enumerate()
            .filter(|(_, window)| *window == PEM_HEADER)
            .any(|(index, _)| {
                bytes[index..]
                    .windows(PEM_PRIVATE.len())
                    .take(64)
                    .any(|window| window == PEM_PRIVATE)
            });
        if has_pem_private_key || contains_aws_access_key_id(&bytes) {
            return Err(StaticWebLaneFailure::new(
                STATIC_WEB_SECRET_DETECTED,
                format!("a credential-shaped value was found in {path}"),
            ));
        }
    }
    Ok(())
}

/// `AKIA`/`ASIA` followed by exactly 16 uppercase alphanumerics, delimited on
/// both sides — the fixed AWS access-key-id shape.
fn contains_aws_access_key_id(bytes: &[u8]) -> bool {
    let is_key_char = |byte: u8| byte.is_ascii_uppercase() || byte.is_ascii_digit();
    bytes.windows(4).enumerate().any(|(index, window)| {
        if window != b"AKIA" && window != b"ASIA" {
            return false;
        }
        if index > 0 && is_key_char(bytes[index - 1]) {
            return false;
        }
        let tail = &bytes[index + 4..];
        tail.len() >= 16
            && tail[..16].iter().all(|byte| is_key_char(*byte))
            && tail.get(16).is_none_or(|byte| !is_key_char(*byte))
    })
}

fn is_text_media_type(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || media_type.starts_with("application/javascript")
        || media_type.starts_with("application/json")
        || media_type == "image/svg+xml"
}

fn blob_batch_body(
    agent_id: &str,
    worker_claim_id: &str,
    materialization_id: &str,
    producer_generation: u64,
    chunk: &[(String, u64)],
) -> serde_json::Value {
    serde_json::json!({
        "agent_id": agent_id,
        "worker_claim_id": worker_claim_id,
        "producer_generation": producer_generation,
        "materialization_id": materialization_id,
        "blobs": chunk
            .iter()
            .map(|(digest, size)| serde_json::json!({ "digest": digest, "size_bytes": size }))
            .collect::<Vec<_>>(),
    })
}

fn upload_blob_chunk(
    client: &RegistryClient<'_>,
    produced: &ProducedStaticWebBundle,
    materialization_id: &str,
    producer_generation: u64,
    chunk: &[(String, u64)],
) -> Result<(), StaticWebLaneFailure> {
    let authorized = client
        .post(
            "blobs/upload-authorizations",
            &blob_batch_body(
                client.agent_id,
                client.worker_claim_id,
                materialization_id,
                producer_generation,
                chunk,
            ),
        )
        .map_err(|error| StaticWebLaneFailure::new(STATIC_WEB_UPLOAD_FAILED, error))?;
    let entries = authorized
        .get("blobs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            StaticWebLaneFailure::new(
                STATIC_WEB_UPLOAD_FAILED,
                "upload authorization response omitted blobs",
            )
        })?;
    for entry in entries {
        let digest = entry
            .get("digest")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match entry.get("status").and_then(serde_json::Value::as_str) {
            // Idempotency: a rerun of the same revision finds its blobs
            // already verified and uploads nothing.
            Some("already_present") => continue,
            Some("upload") => {}
            other => {
                return Err(StaticWebLaneFailure::new(
                    STATIC_WEB_UPLOAD_FAILED,
                    format!("unexpected authorization status {other:?} for {digest}"),
                ));
            }
        }
        let upload_url = entry
            .get("upload_url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                StaticWebLaneFailure::new(
                    STATIC_WEB_UPLOAD_FAILED,
                    format!("authorization for {digest} omitted upload_url"),
                )
            })?;
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        let bytes = std::fs::read(produced.bundle_root.join("blobs/sha256").join(hex)).map_err(
            |error| {
                StaticWebLaneFailure::new(
                    STATIC_WEB_UPLOAD_FAILED,
                    format!("read bundle blob {digest}: {error}"),
                )
            },
        )?;
        let mut request = ureq::put(upload_url);
        if let Some(headers) = entry
            .get("required_headers")
            .and_then(serde_json::Value::as_object)
        {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    request = request.set(name, value);
                }
            }
        }
        request.send_bytes(&bytes).map_err(|error| {
            StaticWebLaneFailure::new(
                STATIC_WEB_UPLOAD_FAILED,
                // ureq errors embed the URL; presigned URLs must never be
                // logged or reported. Keep the status only.
                format!(
                    "blob PUT for {digest} failed ({})",
                    match error {
                        ureq::Error::Status(status, _) => format!("HTTP {status}"),
                        ureq::Error::Transport(_) => "transport error".to_string(),
                    }
                ),
            )
        })?;
    }
    Ok(())
}

struct RegistryClient<'a> {
    api_url: &'a str,
    builder_token: &'a str,
    agent_id: &'a str,
    job_id: &'a str,
    lease_token: &'a str,
    worker_claim_id: &'a str,
}

impl RegistryClient<'_> {
    /// POST `/v1/static-web/jobs/:jobId/<endpoint>`. Non-2xx responses report
    /// the registry's structured `error` code; bodies never carry the lease.
    fn post(&self, endpoint: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        let url = format!(
            "{}/v1/static-web/jobs/{}/{endpoint}",
            self.api_url.trim_end_matches('/'),
            self.job_id
        );
        let response = ureq::post(&url)
            .set("authorization", &format!("Bearer {}", self.builder_token))
            .set("x-ato-authoring-lease-token", self.lease_token)
            .send_json(body.clone());
        match response {
            Ok(response) => response
                .into_json::<serde_json::Value>()
                .map_err(|error| format!("decode static-web {endpoint} response: {error}")),
            Err(ureq::Error::Status(status, response)) => {
                let body = response
                    .into_json::<serde_json::Value>()
                    .unwrap_or_default();
                let code = body
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown_error");
                let message = body
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                Err(format!(
                    "static-web {endpoint} refused (HTTP {status}, {code}): {message}"
                ))
            }
            Err(ureq::Error::Transport(transport)) => Err(format!(
                "static-web {endpoint} transport error: {transport}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialization_id_is_deterministic_and_registry_shaped() {
        let one = derive_materialization_id("bcrev_01KYN2ZEXAMPLE").expect("derives");
        let two = derive_materialization_id("bcrev_01KYN2ZEXAMPLE").expect("derives");
        assert_eq!(one, two);
        let other = derive_materialization_id("bcrev_01KYN2ZOTHER").expect("derives");
        assert_ne!(one, other);
        let body = one.strip_prefix("swm_").expect("swm_ prefix");
        assert_eq!(body.len(), 52);
        assert!(
            body.bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte)),
            "{one} must match ^swm_[a-z2-7]{{52}}$"
        );
    }

    #[test]
    fn plan_embedded_materialization_id_is_used_only_when_registry_shaped() {
        let valid = derive_materialization_id("bcrev_01KYN2ZEXAMPLE").expect("derives");
        let plan = serde_json::json!({
            "static_web_output": { "materialization_id": valid }
        });
        assert_eq!(
            plan_materialization_id(Some(&plan)).as_deref(),
            Some(valid.as_str())
        );
        for bogus in ["swm-not-base32", "swm_TOOSHORT", "mat_fixture", ""] {
            let plan = serde_json::json!({
                "static_web_output": { "materialization_id": bogus }
            });
            assert_eq!(plan_materialization_id(Some(&plan)), None, "{bogus:?}");
        }
        assert_eq!(plan_materialization_id(None), None);
        assert_eq!(plan_materialization_id(Some(&serde_json::json!({}))), None);
    }

    #[test]
    fn aws_access_key_shape_is_detected_only_when_delimited() {
        assert!(contains_aws_access_key_id(
            b"const key = \"AKIAIOSFODNN7EXAMPLE\";"
        ));
        assert!(!contains_aws_access_key_id(b"NOTAKIAIOSFODNN7EXAMPLE"));
        assert!(!contains_aws_access_key_id(b"AKIAIOSFODNN7EXAMPLETOOLONG"));
        assert!(!contains_aws_access_key_id(b"AKIA-short"));
    }
}
