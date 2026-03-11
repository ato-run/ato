//! `ato keygen` - generate signing keys for capsules.

use anyhow::{Context, Result};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

use capsule_core::types::signing::StoredKey;
use capsule_core::CapsuleReporter;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub struct KeygenArgs {
    pub out: Option<PathBuf>,
    pub force: bool,
    pub json: bool,
}

pub fn execute(
    args: KeygenArgs,
    reporter: std::sync::Arc<crate::reporters::CliReporter>,
) -> Result<()> {
    let (private_path, public_path) = resolve_output_paths(args.out.as_ref());

    if args.json {
        if private_path.exists() && !args.force {
            anyhow::bail!(
                "Refusing to overwrite existing file: {} (use --force)",
                private_path.display()
            );
        }

        let stored = StoredKey::generate();
        stored.write(&private_path)?;

        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&private_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&private_path, perms)
                .with_context(|| format!("Failed to set permissions on: {:?}", private_path))?;
        }

        futures::executor::block_on(reporter.notify("✅ Key generated successfully!".to_string()))?;
        futures::executor::block_on(reporter.notify("".to_string()))?;
        futures::executor::block_on(
            reporter.notify(format!("Key file:      {}", private_path.display())),
        )?;
        futures::executor::block_on(reporter.notify(format!(
            "Developer key: {}",
            stored.developer_key_fingerprint()
        )))?;
        futures::executor::block_on(
            reporter.notify(format!("Public key:    {}", stored.public_key)),
        )?;
        futures::executor::block_on(reporter.notify("".to_string()))?;
        futures::executor::block_on(
            reporter.notify("⚠️  Keep your private key secure!".to_string()),
        )?;

        return Ok(());
    }

    if (public_path.exists() || private_path.exists()) && !args.force {
        anyhow::bail!(
            "Refusing to overwrite existing keys (use --force): {} {}",
            private_path.display(),
            public_path.display()
        );
    }

    let mut csprng = OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let verifying_key: VerifyingKey = (&signing_key).into();

    let secret_bytes = signing_key.to_bytes();
    fs::write(&private_path, secret_bytes)
        .with_context(|| format!("Failed to write secret key: {:?}", private_path))?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&private_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&private_path, perms)
            .with_context(|| format!("Failed to set permissions on: {:?}", private_path))?;
    }

    let public_bytes = verifying_key.to_bytes();
    fs::write(&public_path, public_bytes)
        .with_context(|| format!("Failed to write public key: {:?}", public_path))?;

    let mut hasher = Sha256::new();
    hasher.update(public_bytes);
    let fingerprint = hasher.finalize();
    let fingerprint_hex: String = fingerprint.iter().map(|b| format!("{:02x}", b)).collect();

    futures::executor::block_on(reporter.notify("✅ Key generated successfully!".to_string()))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(
        reporter.notify(format!("Private key:   {}", private_path.display())),
    )?;
    futures::executor::block_on(
        reporter.notify(format!("Public key:    {}", public_path.display())),
    )?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify("Public key (hex):".to_string()))?;
    futures::executor::block_on(reporter.notify(hex::encode(public_bytes)))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify("Fingerprint (SHA256):".to_string()))?;
    futures::executor::block_on(reporter.notify(fingerprint_hex))?;
    futures::executor::block_on(reporter.notify("".to_string()))?;
    futures::executor::block_on(reporter.notify("⚠️  Keep your private key secure!".to_string()))?;

    Ok(())
}

fn resolve_output_paths(out: Option<&PathBuf>) -> (PathBuf, PathBuf) {
    if let Some(path) = out {
        let public = path.with_extension("pub");
        return (path.clone(), public);
    }
    (PathBuf::from("private.key"), PathBuf::from("public.key"))
}
