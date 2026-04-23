use anyhow::{bail, Context, Result};
use std::io::IsTerminal;

use age::secrecy::ExposeSecret;

use crate::application::secrets::backend::age::load_identity_bytes;
use crate::application::secrets::store::{SecretEntry, SecretScope};
use crate::application::secrets::{AgeFileBackend, SecretStore};
use crate::cli::secrets::SecretsCommands;

pub(crate) fn execute_secrets_command(command: SecretsCommands) -> Result<()> {
    match command {
        SecretsCommands::Init {
            no_passphrase,
            ssh_key,
        } => cmd_init(no_passphrase, ssh_key),

        SecretsCommands::Set {
            key,
            namespace,
            description,
            allow,
            deny,
        } => {
            let value = read_secret_value(&key)?;
            let store = SecretStore::open()?;
            store.set_in_namespace(
                &key,
                &namespace,
                &value,
                description.as_deref(),
                if allow.is_empty() { None } else { Some(allow) },
                if deny.is_empty() { None } else { Some(deny) },
            )?;
            eprintln!("✅ Secret '{key}' stored in namespace '{namespace}'.");
            Ok(())
        }

        SecretsCommands::Get { key, namespace } => {
            let store = SecretStore::open()?;
            let value = if namespace == "default" {
                store.get(&key)?
            } else {
                store.get_in_namespace(&key, &namespace)?
            };
            match value {
                Some(v) => { println!("{v}"); Ok(()) }
                None => bail!("secret '{}' not found", key),
            }
        }

        SecretsCommands::List { json } => {
            let store = SecretStore::open()?;
            let entries = store.list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else {
                print_secrets_table(&entries);
            }
            Ok(())
        }

        SecretsCommands::Delete { key, namespace } => {
            let store = SecretStore::open()?;
            if namespace == "*" {
                store.delete(&key)?;
            } else if namespace == "default" {
                store.delete(&key)?;
            } else {
                // Delete from specific namespace only.
                if let Some(age) = store.age() {
                    use crate::application::secrets::backend::traits::{SecretBackend, SecretKey};
                    let sk = SecretKey::with_namespace(&namespace, &key);
                    age.delete(&sk)?;
                } else {
                    store.delete(&key)?;
                }
            }
            eprintln!("🗑  Secret '{key}' deleted.");
            Ok(())
        }

        SecretsCommands::Import { env_file, namespace } => {
            let store = SecretStore::open()?;
            if namespace == "default" {
                let count = store.import_env_file(&env_file)?;
                eprintln!("✅ Imported {count} secret(s) from {}.", env_file.display());
            } else {
                // Parse env file manually and use namespace-aware set.
                let raw = std::fs::read_to_string(&env_file)
                    .with_context(|| format!("failed to read {}", env_file.display()))?;
                let mut count = 0usize;
                for line in raw.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') { continue; }
                    if let Some((k, v)) = t.split_once('=') {
                        store.set_in_namespace(k.trim(), &namespace, v.trim(), None, None, None)?;
                        count += 1;
                    }
                }
                eprintln!("✅ Imported {count} secret(s) from {} into namespace '{namespace}'.", env_file.display());
            }
            Ok(())
        }

        SecretsCommands::Allow { key, capsule_id } => {
            let store = SecretStore::open()?;
            let entries = store.list()?;
            let entry = entries.iter().find(|e| e.key == key);
            let mut allow = entry.and_then(|e| e.allow.clone()).unwrap_or_default();
            if !allow.contains(&capsule_id) {
                allow.push(capsule_id.clone());
            }
            store.update_acl(&key, Some(allow), None)?;
            eprintln!("✅ Granted '{capsule_id}' access to '{key}'.");
            Ok(())
        }

        SecretsCommands::Deny { key, capsule_id } => {
            let store = SecretStore::open()?;
            let entries = store.list()?;
            let entry = entries.iter().find(|e| e.key == key);
            let mut deny = entry.and_then(|e| e.deny.clone()).unwrap_or_default();
            if !deny.contains(&capsule_id) {
                deny.push(capsule_id.clone());
            }
            store.update_acl(&key, None, Some(deny))?;
            eprintln!("🚫 Denied '{capsule_id}' access to '{key}'.");
            Ok(())
        }

        SecretsCommands::RotateIdentity { new_identity } => {
            cmd_rotate_identity(new_identity)
        }
    }
}

// ── Subcommand implementations ───────────────────────────────────────────────

fn cmd_init(
    no_passphrase: bool,
    ssh_key: Option<std::path::PathBuf>,
) -> Result<()> {
    if ssh_key.is_some() {
        bail!("--ssh-key is not yet implemented; use the default X25519 identity");
    }

    let home = dirs::home_dir().context("failed to resolve home directory")?;
    let age = AgeFileBackend::new(home.clone());

    if age.identity_exists() {
        bail!(
            "age identity already exists at {}\n\
             Use `ato secrets rotate-identity` to generate a new one.",
            age.identity_key_path().display()
        );
    }

    let passphrase = if no_passphrase {
        None
    } else {
        let pp = rpassword::prompt_password("Passphrase for identity.key (leave empty to skip): ")
            .context("failed to read passphrase")?;
        if pp.is_empty() { None } else { Some(pp) }
    };

    let identity = age.init_identity(passphrase.as_deref())?;

    eprintln!(
        "✅ age identity created at {}",
        age.identity_key_path().display()
    );
    eprintln!("   Public key: {}", identity.to_public());
    if passphrase.is_some() {
        eprintln!("   Protected with passphrase.");
        eprintln!("   Run `ato session start` to unlock once per shell session.");
    } else {
        eprintln!("   ⚠️  No passphrase — protected by file permissions (chmod 600).");
    }
    Ok(())
}

fn cmd_rotate_identity(
    new_identity_path: Option<std::path::PathBuf>,
) -> Result<()> {
    let store = SecretStore::open()?;

    let age = store.age().ok_or_else(|| {
        anyhow::anyhow!(
            "age identity not loaded.\n\
             Run `ato secrets init` first."
        )
    })?;

    // Generate or load the new identity.
    let new_id_secret = if let Some(ref path) = new_identity_path {
        let raw = std::fs::read(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        // Load passphrase if needed.
        let pp_str;
        let pp = if raw.starts_with(b"-----BEGIN") || raw.starts_with(b"age-encryption") {
            pp_str = rpassword::prompt_password("Passphrase for new identity: ")?;
            Some(pp_str.as_str())
        } else {
            None
        };
        let id = load_identity_bytes(&raw, pp).context("failed to load new identity")?;
        id.to_string()
    } else {
        age::x25519::Identity::generate().to_string()
    };

    let new_identity = new_id_secret
        .expose_secret()
        .parse::<age::x25519::Identity>()
        .map_err(|e| anyhow::anyhow!("invalid identity: {}", e))?;

    // Back up current identity.
    let key_path = age.identity_key_path();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    let backup_path = key_path.with_extension(format!("key.old-{timestamp}"));
    std::fs::copy(&key_path, &backup_path)
        .context("failed to back up current identity")?;
    eprintln!("🔒 Old identity backed up to {}", backup_path.display());

    // Re-encrypt all namespace files.
    age.reencrypt_all(&new_identity)
        .context("failed to re-encrypt secrets")?;

    // Write the new identity (no passphrase – user can re-init with passphrase).
    use crate::application::secrets::store::write_secure_file;
    write_secure_file(&key_path, new_id_secret.expose_secret().as_bytes())?;
    let pub_str = new_identity.to_public().to_string();
    write_secure_file(&age.identity_pub_path(), pub_str.as_bytes())?;

    eprintln!("✅ Identity rotated. New public key: {}", new_identity.to_public());
    eprintln!("   Backup: {}", backup_path.display());
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_secret_value(key: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("{key} (hidden): "))
            .context("failed to read secret value")
    } else {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("failed to read secret value from stdin")?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    }
}

fn print_secrets_table(entries: &[SecretEntry]) {
    if entries.is_empty() {
        eprintln!("No secrets stored. Use `ato secrets set KEY` to add one.");
        return;
    }
    eprintln!("{:<30} {:<18} UPDATED", "KEY", "SCOPE");
    eprintln!("{}", "-".repeat(65));
    for entry in entries {
        let scope = match &entry.scope {
            SecretScope::Global => "global".to_string(),
            SecretScope::Capsule(id) => format!("capsule:{}", id),
        };
        let updated = entry
            .updated_at
            .split('T')
            .next()
            .unwrap_or(&entry.updated_at);
        eprintln!("{:<30} {:<18} {}", entry.key, scope, updated);
    }
}
