use clap::Parser;
use ed25519_dalek::{Signature, Verifier};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to audit log file
    #[arg(long, default_value = "/tmp/capsuled/logs/audit.jsonl")]
    log_path: PathBuf,

    /// Path to node key file (PEM)
    #[arg(long, default_value = "/tmp/capsuled/keys/node_key.pem")]
    key_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuditRecord {
    timestamp: String,
    node_id: String,
    gpu_uuid: Option<String>,
    action: String,
    status: String,
    details: Option<String>,
    signature: String,
}

impl AuditRecord {
    fn signable_string(&self) -> String {
        // Must match AuditRecord::signable_string in audit.rs
        // format!("{}|{}|{}|{}|{}|{}", timestamp, node_id, gpu_uuid, action, status, details)
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.timestamp,
            self.node_id,
            self.gpu_uuid.as_deref().unwrap_or(""),
            self.action,
            self.status,
            self.details.as_deref().unwrap_or("")
        )
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("Verifying audit log: {:?}", args.log_path);
    println!("Using key from: {:?}", args.key_path);

    if !args.log_path.exists() {
        anyhow::bail!("Audit log file not found: {:?}", args.log_path);
    }
    if !args.key_path.exists() {
        anyhow::bail!("Key file not found: {:?}", args.key_path);
    }

    // Load key
    // Load key (raw bytes)
    let key_bytes = fs::read(&args.key_path)?;
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid key length (expected 32 bytes)"))?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&key_array);
    let verifying_key = signing_key.verifying_key();

    println!(
        "Loaded key. Public Key: {}",
        hex::encode(verifying_key.as_bytes())
    );

    let file = fs::File::open(&args.log_path)?;
    let reader = BufReader::new(file);

    let mut valid_count = 0;
    let mut error_count = 0;

    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let record: AuditRecord = serde_json::from_str(&line)
            .map_err(|e| anyhow::anyhow!("Failed to parse line {}: {}", i + 1, e))?;

        let signable = record.signable_string();
        println!("Debug: Signable string: '{}'", signable);

        // Debug: Generate signature with loaded key
        use ed25519_dalek::Signer;
        let expected_sig = signing_key.sign(signable.as_bytes());
        let expected_hex = hex::encode(expected_sig.to_bytes());
        println!("Debug: Expected signature: {}", expected_hex);
        println!("Debug: Actual signature:   {}", record.signature);

        let signature_bytes = hex::decode(&record.signature)
            .map_err(|e| anyhow::anyhow!("Invalid hex signature at line {}: {}", i + 1, e))?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid signature format at line {}: {}", i + 1, e))?;

        match verifying_key.verify(signable.as_bytes(), &signature) {
            Ok(_) => {
                println!(
                    "✅ Record {}: {} - {} (Verified)",
                    i + 1,
                    record.action,
                    record.status
                );
                valid_count += 1;
            }
            Err(e) => {
                println!(
                    "❌ Record {}: {} - {} (Verification Failed: {})",
                    i + 1,
                    record.action,
                    record.status,
                    e
                );
                error_count += 1;
            }
        }
    }

    println!(
        "Verification complete. {} valid, {} failed.",
        valid_count, error_count
    );
    if error_count > 0 {
        anyhow::bail!("Found {} invalid signatures", error_count);
    }

    Ok(())
}
