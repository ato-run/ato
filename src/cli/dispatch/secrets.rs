use anyhow::{Context, Result};
use std::io::IsTerminal;

use crate::application::secrets::store::SecretEntry;
use crate::application::secrets::SecretStore;
use crate::cli::secrets::SecretsCommands;

pub(crate) fn execute_secrets_command(command: SecretsCommands) -> Result<()> {
    match command {
        SecretsCommands::Set {
            key,
            description,
            allow,
            deny,
        } => {
            let value = if std::io::stdin().is_terminal() {
                rpassword::prompt_password(format!("{key} (hidden): "))
                    .context("failed to read secret value")?
            } else {
                use std::io::BufRead;
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .context("failed to read secret value from stdin")?;
                line.trim_end_matches(['\n', '\r']).to_string()
            };
            let store = SecretStore::open()?;
            store.set(
                &key,
                &value,
                description.as_deref(),
                if allow.is_empty() { None } else { Some(allow) },
                if deny.is_empty() { None } else { Some(deny) },
            )?;
            eprintln!("✅ Secret '{key}' stored.");
            Ok(())
        }

        SecretsCommands::Get { key } => {
            let store = SecretStore::open()?;
            match store.get(&key)? {
                Some(value) => {
                    println!("{value}");
                    Ok(())
                }
                None => {
                    anyhow::bail!("secret '{}' not found", key);
                }
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

        SecretsCommands::Delete { key } => {
            let store = SecretStore::open()?;
            store.delete(&key)?;
            eprintln!("🗑  Secret '{key}' deleted.");
            Ok(())
        }

        SecretsCommands::Import { env_file } => {
            let store = SecretStore::open()?;
            let count = store.import_env_file(&env_file)?;
            eprintln!("✅ Imported {count} secret(s) from {}.", env_file.display());
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
    }
}

fn print_secrets_table(entries: &[SecretEntry]) {
    if entries.is_empty() {
        eprintln!("No secrets stored. Use `ato secrets set KEY` to add one.");
        return;
    }
    eprintln!("{:<30} {:<10} UPDATED", "KEY", "SCOPE");
    eprintln!("{}", "-".repeat(60));
    for entry in entries {
        let scope = match &entry.scope {
            crate::application::secrets::store::SecretScope::Global => "global".to_string(),
            crate::application::secrets::store::SecretScope::Capsule(id) => {
                format!("capsule:{}", id)
            }
        };
        let updated = entry
            .updated_at
            .split('T')
            .next()
            .unwrap_or(&entry.updated_at);
        eprintln!("{:<30} {:<10} {}", entry.key, scope, updated);
    }
}
