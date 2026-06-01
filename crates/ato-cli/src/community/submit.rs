use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml::Value as TomlValue;
use tracing::debug;

use super::capsule_toml::{
    SourceValidationOutcome, extract_toml_source, resolve_community_api_base_url,
    validate_capsule_toml_source_matches_run_target,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubmissionPayload {
    source: String,
    capsule_toml: String,
    metadata: SubmissionMetadata,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmissionMetadata {
    client: String,
    platform: String,
    #[serde(rename = "trustRequested")]
    trust_requested: String,
    source_identity: SourceIdentity,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    declared: Option<String>,
    provenance: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubmissionResponse {
    id: String,
    url: String,
    status: String,
}

fn submission_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .with_context(|| "Failed to build submission client")
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

pub(crate) fn validate_toml_shape(toml_content: &str) -> Result<()> {
    let parsed: TomlValue =
        toml::from_str(toml_content).with_context(|| "Invalid TOML: failed to parse")?;

    let table = parsed
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("capsule.toml must be a TOML table"))?;

    if !table.contains_key("name") {
        bail!("capsule.toml is missing required field 'name'");
    }

    if !table.contains_key("schema_version") {
        bail!("capsule.toml is missing required field 'schema_version'");
    }

    let has_runnable_targets = table
        .get("targets")
        .and_then(|v| v.as_table())
        .map(|t| !t.is_empty())
        .unwrap_or(false);

    let has_runnable = table.contains_key("run")
        || table.contains_key("run_command")
        || table.contains_key("entrypoint")
        || table.contains_key("cmd")
        || has_runnable_targets;

    if !has_runnable {
        bail!(
            "capsule.toml does not declare any runnable target. \
             Add at least 'run', 'run_command', 'entrypoint', or a [targets] section."
        );
    }

    Ok(())
}

fn scan_for_secrets(toml_content: &str) -> Result<Vec<String>> {
    let mut warnings = Vec::new();

    for line in toml_content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }

        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_lowercase();
            let value = trimmed[eq_pos + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');

            let is_sensitive_key = key.contains("token")
                || key.contains("secret")
                || key.contains("password")
                || key.contains("passwd")
                || key.contains("pwd")
                || key.contains("api_key")
                || key.contains("private_key");

            let looks_like_private_key = value.starts_with("-----BEGIN")
                || value.starts_with("ssh-")
                || (value.len() > 40
                    && value
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '+' || c == '/' || c == '=')
                    && value.len() % 4 == 0);

            if is_sensitive_key && !is_placeholder_value(value) {
                warnings.push(format!(
                    "Sensitive-looking key '{}' has a non-placeholder value",
                    trimmed[..eq_pos].trim()
                ));
            }

            if looks_like_private_key && !value.starts_with("sha256:") {
                warnings.push(format!(
                    "Value for key '{}' looks like a private key or base64-encoded secret",
                    trimmed[..eq_pos].trim()
                ));
            }
        }
    }

    Ok(warnings)
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

pub(crate) struct SubmitResult {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) status: String,
}

pub(crate) async fn execute_submit(
    source: &str,
    toml_path: &Path,
    dry_run: bool,
    yes: bool,
    json_mode: bool,
) -> Result<()> {
    let normalized_source =
        crate::install::normalize_github_repository(source).with_context(|| {
            format!(
                "Invalid source '{}'. Use github.com/owner/repo format.",
                source
            )
        })?;

    let toml_content = std::fs::read_to_string(toml_path)
        .with_context(|| format!("Failed to read capsule.toml: {}", toml_path.display()))?;

    let dry_run_mode = if dry_run {
        Some(SubmitDryRun::PrintToConsole { json_mode })
    } else {
        None
    };

    let payload = prepare_submission(
        &normalized_source,
        toml_path,
        &toml_content,
        yes,
        dry_run_mode.as_ref(),
    )?;

    let Some(payload) = payload else {
        return Ok(());
    };

    if !dry_run {
        let is_tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        if is_tty && !yes {
            eprintln!();
            eprintln!("Submit capsule.toml to Ato community?");
            eprintln!();
            eprintln!("  source: {}", normalized_source);
            eprintln!("  toml: {}", toml_path.display());
            eprintln!("  trust: community");
            eprintln!("  visibility: public");
            eprintln!();
            eprintln!("This will publish the capsule.toml as public execution metadata.");
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
                    eprintln!("Submission aborted.");
                }
                return Ok(());
            }
        } else if !is_tty && !yes {
            bail!(
                "Non-interactive submission requires -y/--yes. \
                 Re-run with -y/--yes to confirm."
            );
        }
    }

    let result = submit_prepared_with_response(&payload).await?;

    if json_mode {
        let output = serde_json::json!({
            "id": result.id,
            "url": result.url,
            "status": result.status,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        eprintln!();
        eprintln!("Submitted community capsule.toml:");
        eprintln!("  id: {}", result.id);
        eprintln!("  url: {}", result.url);
        eprintln!("  status: {}", result.status);
    }

    Ok(())
}

pub(crate) struct PreparedSubmission {
    pub(crate) payload: SubmissionPayload,
}

pub(crate) enum SubmitDryRun {
    PrintToConsole { json_mode: bool },
}

pub(crate) fn prepare_submission(
    normalized_source: &str,
    toml_path: &Path,
    toml_content: &str,
    yes: bool,
    dry_run: Option<&SubmitDryRun>,
) -> Result<Option<PreparedSubmission>> {
    validate_toml_shape(toml_content)
        .with_context(|| format!("Invalid capsule.toml: {}", toml_path.display()))?;

    let secret_warnings = scan_for_secrets(toml_content)?;
    if !secret_warnings.is_empty() {
        for w in &secret_warnings {
            eprintln!("WARNING: {}", w);
        }
        if !yes && std::io::stdin().is_terminal() {
            eprint!("Continue with submission anyway? [y/N] ");
            use std::io::Write;
            let _ = std::io::stderr().flush();
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .context("Failed to read confirmation")?;
            if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
                bail!("Submission aborted due to sensitive-looking values.");
            }
        } else if !yes {
            bail!(
                "Submission blocked: capsule.toml contains {} sensitive-looking value(s). \
                 Review the warnings above and re-run with -y to override.",
                secret_warnings.len()
            );
        }
    }

    let declared_source = extract_toml_source(toml_content);
    let source_validation =
        validate_capsule_toml_source_matches_run_target(toml_content, normalized_source);

    match source_validation {
        SourceValidationOutcome::Match => {}
        SourceValidationOutcome::MissingSource => {
            let is_tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            if is_tty {
                eprintln!(
                    "WARNING: capsule.toml does not declare a source.repository. \
                     The submission will be registered with source '{}'.",
                    normalized_source
                );
                eprint!("Continue? [y/N] ");
                use std::io::Write;
                let _ = std::io::stderr().flush();
                let mut input = String::new();
                std::io::stdin()
                    .read_line(&mut input)
                    .context("Failed to read confirmation")?;
                if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
                    bail!("Submission aborted by user.");
                }
            } else if yes {
                eprintln!(
                    "WARNING: capsule.toml has no source.repository; \
                     using '{}' as provenance (--yes).",
                    normalized_source
                );
            } else {
                bail!(
                    "capsule.toml does not declare a source.repository. \
                     Re-run with -y/--yes to continue, or add [source.repository] to the TOML."
                );
            }
        }
        SourceValidationOutcome::Mismatch {
            toml_source,
            expected_source: _,
        } => {
            bail!(
                "Source identity mismatch: capsule.toml declares '{}', \
                 but submission source is '{}'.",
                toml_source,
                normalized_source
            );
        }
    }

    let payload = SubmissionPayload {
        source: normalized_source.to_string(),
        capsule_toml: toml_content.to_string(),
        metadata: SubmissionMetadata {
            client: "ato-cli".to_string(),
            platform: detect_platform(),
            trust_requested: "community".to_string(),
            source_identity: SourceIdentity {
                declared: declared_source,
                provenance: normalized_source.to_string(),
            },
        },
    };

    if let Some(SubmitDryRun::PrintToConsole { json_mode }) = dry_run {
        if *json_mode {
            let output = serde_json::json!({
                "status": "dry_run",
                "payload_summary": {
                    "source": payload.source,
                    "client": payload.metadata.client,
                    "platform": payload.metadata.platform,
                    "trustRequested": payload.metadata.trust_requested,
                    "sourceIdentity": payload.metadata.source_identity,
                    "tomlSizeBytes": payload.capsule_toml.len(),
                }
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            eprintln!();
            eprintln!("[dry-run] Would submit:");
            eprintln!("  source: {}", payload.source);
            eprintln!("  client: {}", payload.metadata.client);
            eprintln!("  platform: {}", payload.metadata.platform);
            eprintln!("  trust: {}", payload.metadata.trust_requested);
            eprintln!("  toml size: {} bytes", payload.capsule_toml.len());
            eprintln!();
            eprintln!("No network call was made.");
        }
        return Ok(None);
    }

    Ok(Some(PreparedSubmission { payload }))
}

pub(crate) async fn submit_prepared_with_response(
    submission: &PreparedSubmission,
) -> Result<SubmitResult> {
    let client = submission_client()?;
    let endpoint = format!("{}/v1/capsule-tomls", resolve_community_api_base_url());
    debug!(%endpoint, "submitting community capsule.toml");
    let response = client
        .post(&endpoint)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&submission.payload)
        .send()
        .await
        .with_context(|| "Failed to submit community capsule.toml")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!("Submission failed (status={}): {}", status, body);
    }

    let result: SubmissionResponse = response
        .json()
        .await
        .with_context(|| "Failed to parse submission response")?;

    Ok(SubmitResult {
        id: result.id,
        url: result.url,
        status: result.status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_toml_shape_accepts_valid_minimal_manifest() {
        let toml = r#"
schema_version = "0.3"
name = "test"
version = "1.0.0"
run = "index.js"
"#;
        assert!(validate_toml_shape(toml).is_ok());
    }

    #[test]
    fn validate_toml_shape_rejects_missing_name() {
        let toml = r#"
schema_version = "0.3"
run = "index.js"
"#;
        let err = validate_toml_shape(toml).unwrap_err();
        assert!(err.to_string().contains("missing required field 'name'"));
    }

    #[test]
    fn validate_toml_shape_rejects_missing_schema_version() {
        let toml = r#"
name = "test"
run = "index.js"
"#;
        let err = validate_toml_shape(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("missing required field 'schema_version'")
        );
    }

    #[test]
    fn validate_toml_shape_rejects_empty_targets_table() {
        let toml = r#"
schema_version = "0.3"
name = "test"
version = "1.0.0"
[targets]
"#;
        let err = validate_toml_shape(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not declare any runnable target")
        );
    }

    #[test]
    fn validate_toml_shape_accepts_targets_with_entries() {
        let toml = r#"
schema_version = "0.3"
name = "test"
version = "1.0.0"
[targets.default]
runtime = "source"
run_command = "index.js"
"#;
        assert!(validate_toml_shape(toml).is_ok());
    }

    #[test]
    fn validate_toml_shape_rejects_no_runnable_target() {
        let toml = r#"
schema_version = "0.3"
name = "test"
version = "1.0.0"
"#;
        let err = validate_toml_shape(toml).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not declare any runnable target")
        );
    }

    #[test]
    fn scan_secrets_detects_token_key_with_real_value() {
        let toml = r#"
name = "test"
token = "sk-abc123def456"
"#;
        let warnings = scan_for_secrets(toml).unwrap();
        assert!(!warnings.is_empty());
    }

    #[test]
    fn scan_secrets_accepts_token_key_with_placeholder() {
        let toml = r#"
name = "test"
token = "YOUR_API_KEY"
"#;
        let warnings = scan_for_secrets(toml).unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn scan_secrets_detects_private_key_pattern() {
        let toml = r#"
name = "test"
key = "-----BEGIN RSA PRIVATE KEY-----\nabc123\n-----END RSA PRIVATE KEY-----"
"#;
        let warnings = scan_for_secrets(toml).unwrap();
        assert!(!warnings.is_empty());
    }

    #[test]
    fn scan_secrets_handles_clean_toml() {
        let toml = r#"
schema_version = "0.3"
name = "test"
version = "1.0.0"
run = "index.js"
port = 3000
"#;
        let warnings = scan_for_secrets(toml).unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn payload_serialization_has_expected_fields() {
        let payload = SubmissionPayload {
            source: "github.com/owner/repo".to_string(),
            capsule_toml: "name = \"test\"\n".to_string(),
            metadata: SubmissionMetadata {
                client: "ato-cli".to_string(),
                platform: "macos-arm64".to_string(),
                trust_requested: "community".to_string(),
                source_identity: SourceIdentity {
                    declared: Some("github.com/owner/repo".to_string()),
                    provenance: "github.com/owner/repo".to_string(),
                },
            },
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["source"], "github.com/owner/repo");
        assert_eq!(json["capsuleToml"], "name = \"test\"\n");
        assert_eq!(json["metadata"]["client"], "ato-cli");
        assert_eq!(json["metadata"]["platform"], "macos-arm64");
        assert_eq!(json["metadata"]["trustRequested"], "community");
    }

    #[test]
    fn payload_with_missing_declared_omits_field() {
        let payload = SubmissionPayload {
            source: "github.com/owner/repo".to_string(),
            capsule_toml: "name = \"test\"\n".to_string(),
            metadata: SubmissionMetadata {
                client: "ato-cli".to_string(),
                platform: "macos-arm64".to_string(),
                trust_requested: "community".to_string(),
                source_identity: SourceIdentity {
                    declared: None,
                    provenance: "github.com/owner/repo".to_string(),
                },
            },
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["source"], "github.com/owner/repo");
        assert!(json["metadata"]["sourceIdentity"].get("declared").is_none());
        assert_eq!(
            json["metadata"]["sourceIdentity"]["provenance"],
            "github.com/owner/repo"
        );
    }

    #[test]
    fn response_deserializes_expected_fields() {
        let json = r#"{"id":"ctoml_abc123","url":"https://ato.run/capsule-toml/ctoml_abc123","status":"pending"}"#;
        let resp: SubmissionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, "ctoml_abc123");
        assert_eq!(resp.url, "https://ato.run/capsule-toml/ctoml_abc123");
        assert_eq!(resp.status, "pending");
    }

    #[test]
    fn platform_string_is_not_empty() {
        assert!(!detect_platform().is_empty());
    }
}
