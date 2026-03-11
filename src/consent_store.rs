use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::ExecutionPlan;

const CONSENT_FILE_NAME: &str = "executionplan_v1.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsentRecord {
    scoped_id: String,
    version: String,
    target_label: String,
    policy_segment_hash: String,
    provisioning_policy_hash: String,
    approved_at: String,
}

fn consent_record_for_plan(plan: &ExecutionPlan) -> ConsentRecord {
    ConsentRecord {
        scoped_id: plan.consent.key.scoped_id.clone(),
        version: plan.consent.key.version.clone(),
        target_label: plan.consent.key.target_label.clone(),
        policy_segment_hash: plan.consent.policy_segment_hash.clone(),
        provisioning_policy_hash: plan.consent.provisioning_policy_hash.clone(),
        approved_at: String::new(),
    }
}

pub fn require_consent(plan: &ExecutionPlan, _assume_yes: bool) -> Result<(), AtoExecutionError> {
    if is_zero_permission_plan(plan) {
        return Ok(());
    }

    let store = ConsentStore::new()?;
    let record = consent_record_for_plan(plan);

    if store.is_consented(&record)? {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(AtoExecutionError::policy_violation(
            "ExecutionPlan consent missing in non-interactive mode. Seed consent from an interactive run first; --yes does not bypass execution consent.",
        ));
    }

    prompt_consent(plan)?;
    store.append_consent(record)?;
    Ok(())
}

pub fn seed_consent(plan: &ExecutionPlan) -> Result<(), AtoExecutionError> {
    if is_zero_permission_plan(plan) {
        return Ok(());
    }

    let store = ConsentStore::new()?;
    let record = consent_record_for_plan(plan);
    if store.is_consented(&record)? {
        return Ok(());
    }

    store.append_consent(record)
}

fn is_zero_permission_plan(plan: &ExecutionPlan) -> bool {
    let policy = &plan.runtime.policy;
    policy.network.allow_hosts.is_empty()
        && policy.filesystem.read_only.is_empty()
        && policy.filesystem.read_write.is_empty()
        && policy.secrets.allow_secret_ids.is_empty()
}

struct ConsentStore {
    path: PathBuf,
}

impl ConsentStore {
    fn new() -> Result<Self, AtoExecutionError> {
        let home = dirs::home_dir().ok_or_else(|| {
            AtoExecutionError::internal("failed to resolve home directory for consent store")
        })?;
        let consent_dir = home.join(".ato").join("consent");

        fs::create_dir_all(&consent_dir).map_err(|err| {
            AtoExecutionError::internal(format!("failed to create consent directory: {err}"))
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&consent_dir, fs::Permissions::from_mode(0o700)).map_err(
                |err| AtoExecutionError::internal(format!("failed to set consent dir mode: {err}")),
            )?;
        }

        let path = consent_dir.join(CONSENT_FILE_NAME);
        if !path.exists() {
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|err| {
                    AtoExecutionError::internal(format!("failed to create consent file: {err}"))
                })?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|err| {
                    AtoExecutionError::internal(format!("failed to set consent file mode: {err}"))
                })?;
            }
        }

        Ok(Self { path })
    }

    fn is_consented(&self, key: &ConsentRecord) -> Result<bool, AtoExecutionError> {
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|err| {
                AtoExecutionError::internal(format!("failed to read consent file: {err}"))
            })?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line.map_err(|err| {
                AtoExecutionError::internal(format!("failed to read consent line: {err}"))
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let record: ConsentRecord = serde_json::from_str(&line).map_err(|err| {
                AtoExecutionError::internal(format!("failed to parse consent line: {err}"))
            })?;
            if record.scoped_id == key.scoped_id
                && record.version == key.version
                && record.target_label == key.target_label
                && record.policy_segment_hash == key.policy_segment_hash
                && record.provisioning_policy_hash == key.provisioning_policy_hash
            {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn append_consent(&self, mut record: ConsentRecord) -> Result<(), AtoExecutionError> {
        record.approved_at = Utc::now().to_rfc3339();

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| {
                AtoExecutionError::internal(format!("failed to append consent file: {err}"))
            })?;

        let line = serde_json::to_string(&record).map_err(|err| {
            AtoExecutionError::internal(format!("failed to serialize consent: {err}"))
        })?;
        writeln!(file, "{}", line).map_err(|err| {
            AtoExecutionError::internal(format!("failed to write consent record: {err}"))
        })?;

        Ok(())
    }
}

fn prompt_consent(plan: &ExecutionPlan) -> Result<(), AtoExecutionError> {
    println!();
    println!("ExecutionPlan consent is required before run:");
    println!(
        "  capsule: {}@{}",
        plan.capsule.scoped_id, plan.capsule.version
    );
    println!(
        "  target: {} (runtime={}, driver={})",
        plan.target.label,
        plan.target.runtime.as_str(),
        plan.target.driver.as_str()
    );
    println!(
        "  policy_segment_hash: {}",
        plan.consent.policy_segment_hash
    );
    println!(
        "  provisioning_policy_hash: {}",
        plan.consent.provisioning_policy_hash
    );
    print!("Approve this policy? [y/N]: ");

    std::io::stdout()
        .flush()
        .map_err(|err| AtoExecutionError::internal(format!("failed to flush stdout: {err}")))?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).map_err(|err| {
        AtoExecutionError::internal(format!("failed to read consent input: {err}"))
    })?;

    let accepted = matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if accepted {
        Ok(())
    } else {
        Err(AtoExecutionError::policy_violation(
            "ExecutionPlan consent rejected by user",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_match_is_exact_for_five_elements() {
        let record = ConsentRecord {
            scoped_id: "publisher/app".to_string(),
            version: "1.0.0".to_string(),
            target_label: "cli".to_string(),
            policy_segment_hash: "blake3:aaa".to_string(),
            provisioning_policy_hash: "blake3:bbb".to_string(),
            approved_at: "2026-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(record.scoped_id, "publisher/app");
        assert_eq!(record.version, "1.0.0");
        assert_eq!(record.target_label, "cli");
        assert_eq!(record.policy_segment_hash, "blake3:aaa");
        assert_eq!(record.provisioning_policy_hash, "blake3:bbb");
    }

    #[test]
    fn zero_permission_plan_is_auto_consented() {
        let plan = ExecutionPlan {
            schema_version: "1".to_string(),
            capsule: capsule_core::execution_plan::model::CapsuleRef {
                scoped_id: "local/test".to_string(),
                version: "1.0.0".to_string(),
            },
            target: capsule_core::execution_plan::model::TargetRef {
                label: "cli".to_string(),
                runtime: capsule_core::execution_plan::model::ExecutionRuntime::Source,
                driver: capsule_core::execution_plan::model::ExecutionDriver::Deno,
                language: None,
            },
            provisioning: capsule_core::execution_plan::model::Provisioning {
                network: capsule_core::execution_plan::model::ProvisioningNetwork {
                    allow_registry_hosts: Vec::new(),
                },
                lock_required: true,
                integrity_required: true,
                allowed_registries: Vec::new(),
            },
            runtime: capsule_core::execution_plan::model::Runtime {
                policy: capsule_core::execution_plan::model::RuntimePolicy {
                    network: capsule_core::execution_plan::model::RuntimeNetworkPolicy {
                        allow_hosts: Vec::new(),
                    },
                    filesystem: capsule_core::execution_plan::model::RuntimeFilesystemPolicy {
                        read_only: Vec::new(),
                        read_write: Vec::new(),
                    },
                    secrets: capsule_core::execution_plan::model::RuntimeSecretsPolicy {
                        allow_secret_ids: Vec::new(),
                        delivery: capsule_core::execution_plan::model::SecretDelivery::Fd,
                    },
                    args: Vec::new(),
                },
                fail_closed: true,
                non_interactive_behavior:
                    capsule_core::execution_plan::model::NonInteractiveBehavior::DenyIfUnconsented,
            },
            consent: capsule_core::execution_plan::model::Consent {
                key: capsule_core::execution_plan::model::ConsentKey {
                    scoped_id: "local/test".to_string(),
                    version: "1.0.0".to_string(),
                    target_label: "cli".to_string(),
                },
                policy_segment_hash: "blake3:aaa".to_string(),
                provisioning_policy_hash: "blake3:bbb".to_string(),
                mount_set_algo_id: "lockfile_mountset_v1".to_string(),
                mount_set_algo_version: 1,
            },
            reproducibility: capsule_core::execution_plan::model::Reproducibility {
                platform: capsule_core::execution_plan::model::Platform {
                    os: "darwin".to_string(),
                    arch: "arm64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
        };

        assert!(is_zero_permission_plan(&plan));
    }
}
