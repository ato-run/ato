use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::capsule_toml::resolve_community_api_base_url;

const MAX_RECEIPT_SIZE_BYTES: usize = 1_048_576;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptUploadPayload {
    receipt: serde_json::Value,
    metadata: ReceiptUploadMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptUploadMetadata {
    client: String,
    platform: String,
    submitted_at: String,
    receipt_kind: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptUploadResponse {
    id: String,
    status: String,
    url: String,
}

fn upload_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .with_context(|| "Failed to build receipt upload client")
}

fn detect_platform() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        std::env::consts::ARCH
    };
    format!("{}-{}", std::env::consts::OS, arch)
}

fn scan_receipt_for_secrets(receipt: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    scan_value_for_secrets(receipt, "", &mut warnings);
    warnings
}

fn scan_value_for_secrets(value: &serde_json::Value, path: &str, warnings: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", path, key)
                };
                let lower_key = key.to_lowercase();
                let is_sensitive_key = lower_key.contains("token")
                    || lower_key.contains("secret")
                    || lower_key.contains("password")
                    || lower_key.contains("private_key");
                if is_sensitive_key {
                    if let serde_json::Value::String(s) = child {
                        if !is_placeholder_value(s) {
                            warnings.push(format!(
                                "Sensitive-looking key '{}' has a non-placeholder value",
                                child_path
                            ));
                        }
                    } else if !child.is_null() {
                        warnings.push(format!(
                            "Sensitive-looking key '{}' has a non-null value",
                            child_path
                        ));
                    }
                }
                scan_value_for_secrets(child, &child_path, warnings);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, child) in arr.iter().enumerate() {
                let child_path = format!("{}[{}]", path, i);
                scan_value_for_secrets(child, &child_path, warnings);
            }
        }
        serde_json::Value::String(s) if contains_private_key_marker(s) => {
            warnings.push(format!(
                "Value at '{}' looks like a private key or base64-encoded secret",
                path
            ));
        }
        _ => {}
    }
}

fn contains_private_key_marker(value: &str) -> bool {
    if value.contains("-----BEGIN") && value.contains("PRIVATE KEY") {
        return true;
    }
    if value.starts_with("ssh-") {
        return true;
    }
    if value.len() > 40
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || c == '+' || c == '/' || c == '=')
        && value.len().is_multiple_of(4)
    {
        return true;
    }
    false
}

fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim();
    v.is_empty()
        || v == "YOUR_API_KEY"
        || v == "YOUR_TOKEN"
        || v == "YOUR_SECRET"
        || v.contains("your-")
        || v.contains("YOUR_")
        || v == "CHANGE_ME"
        || v == "change_me"
        || v == "REPLACE_ME"
        || v == "replace_me"
        || v == "xxx"
        || v == "X"
}

fn validate_capsule_toml_id(id: &str) -> Result<()> {
    if !id.starts_with("ctoml_") {
        bail!(
            "Invalid capsule_toml_id: '{}'. Expected format: ctoml_<id>",
            id
        );
    }
    let rest = &id[6..];
    if rest.is_empty()
        || !rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!(
            "Invalid capsule_toml_id: '{}'. Expected format: ctoml_<id> with alphanumeric, underscore, or hyphen",
            id
        );
    }
    Ok(())
}

pub(crate) async fn execute_receipt_upload(
    capsule_toml_id: &str,
    receipt_path: &Path,
    dry_run: bool,
    yes: bool,
    json_mode: bool,
) -> Result<()> {
    if !receipt_path.exists() {
        bail!("Receipt file not found: {}", receipt_path.display());
    }

    validate_capsule_toml_id(capsule_toml_id)
        .with_context(|| format!("Invalid capsule_toml_id: {capsule_toml_id}"))?;

    let metadata = std::fs::metadata(receipt_path).with_context(|| {
        format!(
            "Failed to read receipt file metadata: {}",
            receipt_path.display()
        )
    })?;

    let file_size = metadata.len() as usize;
    if file_size > MAX_RECEIPT_SIZE_BYTES {
        bail!(
            "Receipt file too large: {} bytes (limit: {} bytes)",
            file_size,
            MAX_RECEIPT_SIZE_BYTES
        );
    }

    let receipt_content = std::fs::read_to_string(receipt_path)
        .with_context(|| format!("Failed to read receipt file: {}", receipt_path.display()))?;

    let receipt_value: serde_json::Value = serde_json::from_str(&receipt_content)
        .with_context(|| format!("Receipt file is not valid JSON: {}", receipt_path.display()))?;

    let secret_warnings = scan_receipt_for_secrets(&receipt_value);
    if !secret_warnings.is_empty() {
        for w in &secret_warnings {
            eprintln!("WARNING: {}", w);
        }
        let is_tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        if is_tty && !yes {
            eprint!("Continue with upload anyway? [y/N] ");
            use std::io::Write;
            let _ = std::io::stderr().flush();
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .context("Failed to read confirmation")?;
            if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
                bail!("Upload aborted due to sensitive-looking values.");
            }
        } else if !is_tty && !yes {
            bail!(
                "Receipt contains {} sensitive-looking value(s). \
                 Review the warnings above and re-run with -y to override.",
                secret_warnings.len()
            );
        }
    }

    let payload = ReceiptUploadPayload {
        receipt: receipt_value,
        metadata: ReceiptUploadMetadata {
            client: "ato-cli".to_string(),
            platform: detect_platform(),
            submitted_at: chrono::Utc::now().to_rfc3339(),
            receipt_kind: "aodd".to_string(),
        },
    };

    if dry_run {
        if json_mode {
            let output = serde_json::json!({
                "status": "dry_run",
                "payload_summary": {
                    "capsuleTomlId": capsule_toml_id,
                    "client": payload.metadata.client,
                    "platform": payload.metadata.platform,
                    "receiptKind": payload.metadata.receipt_kind,
                    "submittedAt": payload.metadata.submitted_at,
                    "receiptSizeBytes": file_size,
                }
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!();
            eprintln!("[dry-run] Would upload receipt:");
            eprintln!("  capsule_toml: {}", capsule_toml_id);
            eprintln!("  client: {}", payload.metadata.client);
            eprintln!("  platform: {}", payload.metadata.platform);
            eprintln!("  receipt_kind: {}", payload.metadata.receipt_kind);
            eprintln!("  receipt size: {} bytes", file_size);
            eprintln!();
            eprintln!("No network call was made.");
        }
        return Ok(());
    }

    let is_tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();

    if is_tty && !yes {
        eprintln!();
        eprintln!("Upload community verification receipt?");
        eprintln!();
        eprintln!("  capsule_toml: {}", capsule_toml_id);
        eprintln!("  receipt: {}", receipt_path.display());
        eprintln!("  receipt size: {} bytes", file_size);
        eprintln!();
        eprintln!("This will attach the receipt to the community capsule.toml record.");
        eprint!("Continue? [y/N] ");
        use std::io::Write;
        let _ = std::io::stderr().flush();
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .context("Failed to read confirmation")?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            if json_mode {
                let output = serde_json::json!({"status": "aborted"});
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                eprintln!("Upload aborted.");
            }
            return Ok(());
        }
    } else if !is_tty && !yes {
        bail!(
            "Non-interactive upload requires -y/--yes. \
             Re-run with -y/--yes to confirm."
        );
    }

    let client = upload_client()?;
    let endpoint = format!(
        "{}/v1/capsule-tomls/{}/receipts",
        resolve_community_api_base_url(),
        capsule_toml_id
    );
    debug!(%endpoint, "uploading community verification receipt");

    let response = client
        .post(&endpoint)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&payload)
        .send()
        .await
        .with_context(|| "Failed to upload community verification receipt")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("Receipt upload failed (status={}): {}", status, body);
    }

    let result: ReceiptUploadResponse = response
        .json()
        .await
        .with_context(|| "Failed to parse receipt upload response")?;

    if json_mode {
        let output = serde_json::json!({
            "capsuleTomlId": capsule_toml_id,
            "receiptId": result.id,
            "status": result.status,
            "url": result.url,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        eprintln!();
        eprintln!("Uploaded community verification receipt:");
        eprintln!("  capsule_toml: {}", capsule_toml_id);
        eprintln!("  receipt: {}", result.id);
        eprintln!("  status: {}", result.status);
        eprintln!("  url: {}", result.url);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_receipt_detects_token_in_json() {
        let receipt = serde_json::json!({
            "execution": {
                "token": "sk-abc123def456"
            }
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("token"));
    }

    #[test]
    fn scan_receipt_accepts_placeholder_token() {
        let receipt = serde_json::json!({
            "execution": {
                "token": "YOUR_API_KEY"
            }
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(warnings.is_empty());
    }

    #[test]
    fn scan_receipt_detects_secret_key() {
        let receipt = serde_json::json!({
            "config": {
                "database_secret": "super-secret-value"
            }
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn scan_receipt_detects_password_field() {
        let receipt = serde_json::json!({
            "auth": {
                "password": "hunter2"
            }
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn scan_receipt_detects_private_key_field() {
        let receipt = serde_json::json!({
            "ssh": {
                "private_key": "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----"
            }
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn scan_receipt_handles_clean_receipt() {
        let receipt = serde_json::json!({
            "schema_version": "0.3",
            "name": "test-capsule",
            "status": "success",
            "exit_code": 0
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(warnings.is_empty());
    }

    #[test]
    fn scan_receipt_handles_nested_arrays() {
        let receipt = serde_json::json!({
            "steps": [
                {"token": "real-value-123"}
            ]
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn scan_receipt_null_sensitive_key_is_ok() {
        let receipt = serde_json::json!({
            "secret": null
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(warnings.is_empty());
    }

    #[test]
    fn placeholder_values_are_recognized() {
        assert!(is_placeholder_value(""));
        assert!(is_placeholder_value("YOUR_API_KEY"));
        assert!(is_placeholder_value("YOUR_TOKEN"));
        assert!(is_placeholder_value("YOUR_SECRET"));
        assert!(is_placeholder_value("your-api-key"));
        assert!(is_placeholder_value("YOUR_VAR"));
        assert!(is_placeholder_value("CHANGE_ME"));
        assert!(is_placeholder_value("change_me"));
        assert!(is_placeholder_value("REPLACE_ME"));
        assert!(is_placeholder_value("replace_me"));
        assert!(is_placeholder_value("xxx"));
        assert!(is_placeholder_value("X"));
        assert!(!is_placeholder_value("sk-abc123"));
    }

    #[test]
    fn payload_serialization_has_expected_fields() {
        let payload = ReceiptUploadPayload {
            receipt: serde_json::json!({"result": "success"}),
            metadata: ReceiptUploadMetadata {
                client: "ato-cli".to_string(),
                platform: "macos-arm64".to_string(),
                submitted_at: "2026-05-31T00:00:00Z".to_string(),
                receipt_kind: "aodd".to_string(),
            },
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["receipt"]["result"], "success");
        assert_eq!(json["metadata"]["client"], "ato-cli");
        assert_eq!(json["metadata"]["platform"], "macos-arm64");
        assert_eq!(json["metadata"]["submittedAt"], "2026-05-31T00:00:00Z");
        assert_eq!(json["metadata"]["receiptKind"], "aodd");
    }

    #[test]
    fn response_deserializes_expected_fields() {
        let json = r#"{"id":"receipt_abc123","status":"pending","url":"https://ato.run/capsule-toml/ctoml_abc123/receipts/receipt_abc123"}"#;
        let resp: ReceiptUploadResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, "receipt_abc123");
        assert_eq!(resp.status, "pending");
        assert_eq!(
            resp.url,
            "https://ato.run/capsule-toml/ctoml_abc123/receipts/receipt_abc123"
        );
    }

    #[test]
    fn max_receipt_size_is_one_mb() {
        assert_eq!(MAX_RECEIPT_SIZE_BYTES, 1_048_576);
    }

    #[test]
    fn private_key_marker_detected_in_arbitrary_string() {
        let receipt = serde_json::json!({
            "config": "-----BEGIN RSA PRIVATE KEY-----\nabc123\n-----END RSA PRIVATE KEY-----"
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("private key"));
    }

    #[test]
    fn private_key_marker_detected_in_nested_string() {
        let receipt = serde_json::json!({
            "execution": {
                "env": {
                    "SOME_KEY": "ssh-rsa AAAAB3..."
                }
            }
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn validate_capsule_toml_id_accepts_valid() {
        assert!(validate_capsule_toml_id("ctoml_abc123").is_ok());
        assert!(validate_capsule_toml_id("ctoml_abc123_xyz").is_ok());
    }

    #[test]
    fn validate_capsule_toml_id_rejects_missing_prefix() {
        let err = validate_capsule_toml_id("abc123").unwrap_err();
        assert!(err.to_string().contains("ctoml_"));
    }

    #[test]
    fn validate_capsule_toml_id_rejects_empty_suffix() {
        let err = validate_capsule_toml_id("ctoml_").unwrap_err();
        assert!(err.to_string().contains("Invalid"));
    }

    #[test]
    fn validate_capsule_toml_id_rejects_special_chars() {
        let err = validate_capsule_toml_id("ctoml_abc/123").unwrap_err();
        assert!(err.to_string().contains("Invalid"));
    }

    #[test]
    fn validate_capsule_toml_id_accepts_hyphens() {
        assert!(validate_capsule_toml_id("ctoml_abc-123").is_ok());
        assert!(validate_capsule_toml_id("ctoml_xxx-yyy-zzz").is_ok());
    }

    #[test]
    fn private_key_marker_detected_in_embedded_error() {
        let receipt = serde_json::json!({
            "log": "error: -----BEGIN RSA PRIVATE KEY-----\nMIIEog...\n-----END RSA PRIVATE KEY-----"
        });
        let warnings = scan_receipt_for_secrets(&receipt);
        assert!(!warnings.is_empty());
        assert!(warnings[0].contains("private key"));
    }
}
