use std::fs;

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use capsule_core::attestation::{
    blob_attestations_dir, generate_keypair, read_envelope, verify_envelope,
    write_trust_root_pubkey, AttestationKey, StoredAttestationKey, TrustRoot,
};
use capsule_core::common::paths::ato_trust_roots_dir;
use serde_json::json;

use crate::cli::attest::AttestCommands;

pub(crate) fn execute_attest_command(command: AttestCommands) -> Result<()> {
    match command {
        AttestCommands::Keygen {
            out,
            force,
            trust,
            label,
        } => keygen_command(out, force, trust, label),
        AttestCommands::Trust {
            pubkey,
            from_key,
            label,
        } => trust_command(pubkey, from_key, label),
        AttestCommands::TrustList => trust_list_command(),
        AttestCommands::Verify { blob, pretty } => verify_command(blob, pretty),
    }
}

fn keygen_command(
    out: std::path::PathBuf,
    force: bool,
    trust: bool,
    label: Option<String>,
) -> Result<()> {
    if out.exists() && !force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            out.display()
        );
    }
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let key = generate_keypair();
    let stored = key.to_stored();
    let bytes =
        serde_json::to_vec_pretty(&stored).context("failed to serialize attestation key")?;
    fs::write(&out, [bytes, b"\n".to_vec()].concat())
        .with_context(|| format!("failed to write {}", out.display()))?;

    let mut report = json!({
        "key_id": key.key_id(),
        "public_key_b64": key.public_key_b64(),
        "key_path": out.display().to_string(),
    });

    if trust {
        let trust_path = write_trust_root_pubkey(&key.public_key_bytes(), label.as_deref())
            .context("failed to register trust root")?;
        report["trust_root_path"] = json!(trust_path.display().to_string());
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn trust_command(
    pubkey: Option<String>,
    from_key: Option<std::path::PathBuf>,
    label: Option<String>,
) -> Result<()> {
    let public_key_bytes = if let Some(pubkey) = pubkey {
        let bytes = BASE64
            .decode(pubkey.trim())
            .context("--pubkey is not valid base64")?;
        bytes_to_array(&bytes)?
    } else if let Some(path) = from_key {
        let raw = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let stored: StoredAttestationKey =
            serde_json::from_slice(&raw).context("failed to parse StoredAttestationKey JSON")?;
        let key =
            AttestationKey::from_stored(&stored).context("failed to materialize AttestationKey")?;
        key.public_key_bytes()
    } else {
        anyhow::bail!("either --pubkey or --from-key must be provided");
    };

    let path = write_trust_root_pubkey(&public_key_bytes, label.as_deref())
        .context("failed to register trust root")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "trust_root_path": path.display().to_string(),
        }))?
    );
    Ok(())
}

fn trust_list_command() -> Result<()> {
    let dir = ato_trust_roots_dir();
    let mut entries = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let bytes = fs::read(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                if let Ok(root) = serde_json::from_slice::<TrustRoot>(&bytes) {
                    entries.push(json!({
                        "key_id": root.key_id,
                        "label": root.label,
                        "path": path.display().to_string(),
                    }));
                }
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "trust_root_dir": dir.display().to_string(),
            "trust_roots": entries,
        }))?
    );
    Ok(())
}

fn verify_command(blob_hash: String, pretty: bool) -> Result<()> {
    let trust_roots = load_trust_roots()?;
    if trust_roots.is_empty() {
        anyhow::bail!(
            "no trust roots registered. Use `ato attest trust --pubkey <BASE64>` or \
             `ato attest keygen --trust` first."
        );
    }

    let dir = blob_attestations_dir(&blob_hash);
    if !dir.is_dir() {
        anyhow::bail!(
            "no attestations found for blob {} (expected directory at {})",
            blob_hash,
            dir.display()
        );
    }

    let mut envelopes = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            envelopes.push(path);
        }
    }
    if envelopes.is_empty() {
        anyhow::bail!("no attestation envelopes for blob {}", blob_hash);
    }

    let mut results = Vec::new();
    let mut any_valid = false;
    for path in &envelopes {
        let envelope = read_envelope(path)?;
        let mut matched_root: Option<&TrustRoot> = None;
        for root in &trust_roots {
            if root.key_id == envelope.signature.key_id {
                matched_root = Some(root);
                break;
            }
        }
        let verdict = match matched_root {
            None => json!({
                "envelope_path": path.display().to_string(),
                "key_id": envelope.signature.key_id,
                "verdict": "untrusted_key",
            }),
            Some(root) => match verify_envelope(&envelope, root) {
                Ok(_) => {
                    any_valid = true;
                    json!({
                        "envelope_path": path.display().to_string(),
                        "key_id": envelope.signature.key_id,
                        "label": root.label,
                        "verdict": "valid",
                    })
                }
                Err(err) => json!({
                    "envelope_path": path.display().to_string(),
                    "key_id": envelope.signature.key_id,
                    "verdict": "invalid",
                    "error": err.to_string(),
                }),
            },
        };
        results.push(verdict);
    }

    let payload = json!({
        "blob_hash": blob_hash,
        "any_valid": any_valid,
        "envelopes": results,
    });
    let rendered = if pretty {
        serde_json::to_string_pretty(&payload)?
    } else {
        serde_json::to_string(&payload)?
    };
    println!("{rendered}");
    if !any_valid {
        anyhow::bail!("no envelope verified successfully against the configured trust roots");
    }
    Ok(())
}

fn load_trust_roots() -> Result<Vec<TrustRoot>> {
    let dir = ato_trust_roots_dir();
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "json").unwrap_or(false) {
            let bytes =
                fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
            if let Ok(root) = serde_json::from_slice::<TrustRoot>(&bytes) {
                out.push(root);
            }
        }
    }
    Ok(out)
}

fn bytes_to_array(bytes: &[u8]) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("public key must be exactly 32 bytes; got {}", bytes.len()))
}
