#![allow(clippy::result_large_err)]

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use capsule_core::common::paths::ato_path;
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::AtoError;

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
    if has_consent(plan)? {
        return Ok(());
    }

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        // Non-TTY caller — emit the typed `execution_plan_consent_required`
        // envelope so a UI shell (today: ato-desktop) can render an
        // approval modal directly from the carried summary + key, then
        // call back to `ato internal consent approve-execution-plan`.
        // The wire code stays `ATO_ERR_EXECUTION_CONTRACT_INVALID`; the
        // `details.reason` discriminator is what newer consumers route
        // on. Older consumers keep classifying this as a generic
        // execution-contract error and fall through to the existing
        // fatal-toast path.
        let scoped_id = plan.consent.key.scoped_id.clone();
        let version = plan.consent.key.version.clone();
        let target_label = plan.consent.key.target_label.clone();
        let policy_segment_hash = plan.consent.policy_segment_hash.clone();
        let provisioning_policy_hash = plan.consent.provisioning_policy_hash.clone();

        let approve_command = format!(
            "ato internal consent approve-execution-plan \\\n  \
             --scoped-id {scoped_id} \\\n  \
             --version {version} \\\n  \
             --target-label {target_label} \\\n  \
             --policy-segment-hash {policy_segment_hash} \\\n  \
             --provisioning-policy-hash {provisioning_policy_hash}",
        );

        let message = format!(
            "ExecutionPlan consent required for target={target_label} of \
             {scoped_id}@{version}. Approve via the desktop modal, a TTY \
             prompt, or `ato internal consent approve-execution-plan` \
             (the same identity fields are emitted as a JSON envelope on \
             stderr in non-TTY mode).",
        );

        let hint = format!(
            "Approve from CLI:\n  {approve_command}\n\
             Or open the launching app and click Approve in the modal.",
        );

        return Err(AtoExecutionError::from_ato_error(
            AtoError::ExecutionPlanConsentRequired {
                message,
                hint: Some(hint),
                scoped_id,
                version,
                target_label,
                policy_segment_hash,
                provisioning_policy_hash,
                summary: consent_summary(plan),
            },
        ));
    }

    prompt_consent(plan)?;
    record_consent(plan)
}

pub fn has_consent(plan: &ExecutionPlan) -> Result<bool, AtoExecutionError> {
    if is_zero_permission_plan(plan) {
        return Ok(true);
    }

    let store = ConsentStore::new()?;
    let record = consent_record_for_plan(plan);

    store.is_consented(&record)
}

pub fn record_consent(plan: &ExecutionPlan) -> Result<(), AtoExecutionError> {
    if is_zero_permission_plan(plan) {
        return Ok(());
    }

    let store = ConsentStore::new()?;
    let record = consent_record_for_plan(plan);
    if store.is_consented(&record)? {
        return Ok(());
    }

    store.append_consent(record)?;
    Ok(())
}

pub fn consent_summary(plan: &ExecutionPlan) -> String {
    let network = if plan.runtime.policy.network.allow_hosts.is_empty() {
        "None".to_string()
    } else {
        plan.runtime.policy.network.allow_hosts.join(", ")
    };
    let read_only = if plan.runtime.policy.filesystem.read_only.is_empty() {
        "None".to_string()
    } else {
        plan.runtime.policy.filesystem.read_only.join(", ")
    };
    let read_write = if plan.runtime.policy.filesystem.read_write.is_empty() {
        "None".to_string()
    } else {
        plan.runtime.policy.filesystem.read_write.join(", ")
    };
    let secrets = if plan.runtime.policy.secrets.allow_secret_ids.is_empty() {
        "None".to_string()
    } else {
        plan.runtime.policy.secrets.allow_secret_ids.join(", ")
    };

    format!(
        "Capsule      : {}@{}\nTarget       : {} (runtime={}, driver={})\nNetwork      : {}\nRead Only    : {}\nRead Write   : {}\nSecrets      : {}\nPolicy Hash  : {}\nProvisioning : {}",
        plan.capsule.scoped_id,
        plan.capsule.version,
        plan.target.label,
        plan.target.runtime.as_str(),
        plan.target.driver.as_str(),
        network,
        read_only,
        read_write,
        secrets,
        plan.consent.policy_segment_hash,
        plan.consent.provisioning_policy_hash,
    )
}

/// Append a consent record built from approval parameters that
/// originated on a UI shell (today: ato-desktop's E302 consent modal,
/// or the matching MCP tool). This is the *write* counterpart to the
/// `ExecutionPlanConsentRequired` envelope: `consent_store.rs` owns
/// every consent-file write, including those triggered by the desktop,
/// so that ATO_HOME resolution, file locking, and unix-mode enforcement
/// stay in one place.
///
/// The five identity fields must match exactly what shipped in the
/// most recent `execution_plan_consent_required` envelope for the
/// capsule — `(scoped_id, version, target_label, policy_segment_hash,
/// provisioning_policy_hash)`. Idempotent: if the matching record is
/// already present, no new line is appended (mirrors `record_consent`).
pub fn approve_execution_plan_consent(
    scoped_id: &str,
    version: &str,
    target_label: &str,
    policy_segment_hash: &str,
    provisioning_policy_hash: &str,
) -> Result<(), AtoExecutionError> {
    let store = ConsentStore::new()?;
    let record = ConsentRecord {
        scoped_id: scoped_id.to_string(),
        version: version.to_string(),
        target_label: target_label.to_string(),
        policy_segment_hash: policy_segment_hash.to_string(),
        provisioning_policy_hash: provisioning_policy_hash.to_string(),
        approved_at: String::new(),
    };
    if store.is_consented(&record)? {
        return Ok(());
    }
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
        let consent_dir = ato_path("consent").map_err(|err| {
            AtoExecutionError::internal(format!("failed to resolve consent directory: {err}"))
        })?;

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
    let summary = consent_summary(plan);
    let use_tui = crate::progressive_ui::can_use_progressive_ui(false);
    if use_tui {
        crate::progressive_ui::render_execution_consent_summary(&summary).map_err(|err| {
            AtoExecutionError::internal(format!("failed to render consent note: {err}"))
        })?;
    } else {
        println!();
        println!("ExecutionPlan consent is required before run:");
        println!("{}", summary);
    }

    let accepted = crate::progressive_ui::confirm_with_fallback(
        "Approve this Execution Plan? ",
        false,
        use_tui,
    )
    .map_err(|err| AtoExecutionError::internal(format!("failed to read consent input: {err}")))?;

    if accepted {
        Ok(())
    } else {
        Err(AtoExecutionError::from_ato_error(
            AtoError::ExecutionContractInvalid {
                message: "ExecutionPlan consent rejected by user".to_string(),
                hint: Some(
                    "Execution Plan の要約を確認し、許可する場合のみ再実行してください。"
                        .to_string(),
                ),
                field: Some("execution_plan.consent".to_string()),
                service: None,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::shared_env_lock as env_lock;
    use super::*;

    use tempfile::TempDir;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(next) => std::env::set_var(key, next),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn sample_plan() -> ExecutionPlan {
        ExecutionPlan {
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
        }
    }

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
        let plan = sample_plan();

        assert!(is_zero_permission_plan(&plan));
    }

    /// Wrapper around `sample_plan()` that flips the plan into a
    /// non-zero-permission state, so `has_consent` actually opens the
    /// store and `require_consent` exercises the TTY gate.
    fn non_trivial_plan() -> ExecutionPlan {
        let mut plan = sample_plan();
        plan.runtime
            .policy
            .network
            .allow_hosts
            .push("api.example.com".to_string());
        plan
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn require_consent_non_tty_emits_execution_plan_consent_required() {
        // Cargo's test runner closes stdin/stdout TTYs, so this
        // exercises the non-TTY branch without us having to fake one.
        let _serial = env_lock().lock().unwrap();
        let home = TempDir::new().expect("create temporary HOME");
        let home_path = home.path().to_string_lossy().to_string();
        let _home_guard = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let plan = non_trivial_plan();
        let err = require_consent(&plan, false).expect_err("must emit consent envelope");

        // The envelope (and exec error) keeps the wire code stable —
        // newer consumers route on details.reason, but the error code
        // and name remain ATO_ERR_EXECUTION_CONTRACT_INVALID / E302.
        let snapshot = format!("{err:?}");
        assert!(
            snapshot.contains("ExecutionPlanConsentRequired")
                || snapshot.contains("execution_plan_consent_required"),
            "expected the new variant to surface, got: {snapshot}"
        );
    }

    /// #126 — the non-TTY message and hint must give CLI users a
    /// concrete approval recipe. Without this, a CLI caller hitting
    /// E302 has no documented way to proceed (the receipt at
    /// claudedocs/aodd-receipts/117-cli-baseline-local-* shows this is
    /// the reason CLI runs of WasedaP2P were a true dead-end before
    /// this change). Anchored on actual content so future drift fails
    /// loudly at this site instead of silently regressing the CLI UX.
    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn non_tty_consent_message_carries_approve_command_recipe() {
        let _serial = env_lock().lock().unwrap();
        let home = TempDir::new().expect("create temporary HOME");
        let home_path = home.path().to_string_lossy().to_string();
        let _home_guard = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let plan = non_trivial_plan();
        let err = require_consent(&plan, false).expect_err("must emit consent envelope");

        let message = err.message.as_str();
        assert!(
            message.contains("ato internal consent approve-execution-plan"),
            "non-TTY consent message must mention the CLI approval command; got: {message}"
        );
        assert!(
            message.contains(&plan.consent.key.scoped_id),
            "non-TTY consent message must name the capsule scoped_id; got: {message}"
        );
        assert!(
            message.contains(&plan.consent.key.target_label),
            "non-TTY consent message must name the target_label; got: {message}"
        );

        let hint = err
            .hint
            .as_deref()
            .expect("non-TTY consent error must carry a hint");
        for field_value in [
            plan.consent.key.scoped_id.as_str(),
            plan.consent.key.version.as_str(),
            plan.consent.key.target_label.as_str(),
            plan.consent.policy_segment_hash.as_str(),
            plan.consent.provisioning_policy_hash.as_str(),
        ] {
            assert!(
                hint.contains(field_value),
                "non-TTY consent hint must embed identity field {field_value:?} so the user \
                 can copy-paste the approval command; got hint: {hint}"
            );
        }
    }

    /// #126 — the typed error's `details` JSON (the same shape the
    /// non-TTY-without-`--json` envelope writes to stderr) must carry
    /// the five identity fields plus the `execution_plan_consent_required`
    /// discriminator. Locks the wire shape so callers scraping stderr
    /// can rely on it across releases.
    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn non_tty_consent_envelope_details_carry_identity_tuple() {
        let _serial = env_lock().lock().unwrap();
        let home = TempDir::new().expect("create temporary HOME");
        let home_path = home.path().to_string_lossy().to_string();
        let _home_guard = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let plan = non_trivial_plan();
        let err = require_consent(&plan, false).expect_err("must emit consent envelope");

        let details = err
            .details
            .as_ref()
            .expect("non-TTY consent error must populate details");
        assert_eq!(
            details
                .get("reason")
                .and_then(|value| value.as_str()),
            Some("execution_plan_consent_required"),
            "details.reason must be the consent discriminator; got: {details}"
        );
        for (field, expected) in [
            ("scoped_id", plan.consent.key.scoped_id.as_str()),
            ("version", plan.consent.key.version.as_str()),
            ("target_label", plan.consent.key.target_label.as_str()),
            ("policy_segment_hash", plan.consent.policy_segment_hash.as_str()),
            (
                "provisioning_policy_hash",
                plan.consent.provisioning_policy_hash.as_str(),
            ),
        ] {
            let actual = details.get(field).and_then(|value| value.as_str());
            assert_eq!(
                actual,
                Some(expected),
                "details.{field} must be present in the envelope so non-TTY callers can scrape it; \
                 got: {details}"
            );
        }
        assert!(
            err.interactive_resolution,
            "ExecutionPlanConsentRequired must report interactive_resolution=true so the \
             non-TTY envelope helper picks it up"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn approve_execution_plan_consent_is_idempotent() {
        let _serial = env_lock().lock().unwrap();
        let home = TempDir::new().expect("create temporary HOME");
        let home_path = home.path().to_string_lossy().to_string();
        let _home_guard = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        // First call writes a record.
        approve_execution_plan_consent("publisher/app", "1.0.0", "cli", "blake3:aaa", "blake3:bbb")
            .expect("first approve");
        let consent_file = home
            .path()
            .join(".ato")
            .join("consent")
            .join(CONSENT_FILE_NAME);
        let after_first = std::fs::read_to_string(&consent_file).expect("read consent file");
        let lines_after_first = after_first.lines().count();
        assert!(
            lines_after_first >= 1,
            "expected at least one record after first approve: {after_first:?}"
        );

        // Second call must be a no-op — same identity tuple is already
        // consented.
        approve_execution_plan_consent("publisher/app", "1.0.0", "cli", "blake3:aaa", "blake3:bbb")
            .expect("second approve");
        let after_second = std::fs::read_to_string(&consent_file).expect("read consent file");
        assert_eq!(
            after_second.lines().count(),
            lines_after_first,
            "second approve must NOT append a duplicate record"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn approve_execution_plan_consent_satisfies_subsequent_has_consent() {
        let _serial = env_lock().lock().unwrap();
        let home = TempDir::new().expect("create temporary HOME");
        let home_path = home.path().to_string_lossy().to_string();
        let _home_guard = EnvVarGuard::set("HOME", Some(home_path.as_str()));

        let plan = non_trivial_plan();

        // No consent yet → has_consent must be false.
        assert!(
            !has_consent(&plan).expect("has_consent before approve"),
            "non-trivial plan must not start consented"
        );

        approve_execution_plan_consent(
            &plan.consent.key.scoped_id,
            &plan.consent.key.version,
            &plan.consent.key.target_label,
            &plan.consent.policy_segment_hash,
            &plan.consent.provisioning_policy_hash,
        )
        .expect("approve via plumbing surface");

        // After the plumbing surface writes the record, the regular
        // `has_consent` path (used by `require_consent`) must agree.
        // This is the contract that lets the desktop's modal flow work:
        // approve via plumbing → re-launch sees the record → no E302.
        assert!(
            has_consent(&plan).expect("has_consent after approve"),
            "approve_execution_plan_consent must satisfy has_consent"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn seed_consent_hardens_store_permissions() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let _serial = env_lock().lock().unwrap();
        let home = TempDir::new().expect("create temporary HOME");
        let home_path = home.path().to_string_lossy().to_string();
        let _home_guard = EnvVarGuard::set("HOME", Some(home_path.as_str()));
        let mut plan = sample_plan();
        plan.runtime
            .policy
            .network
            .allow_hosts
            .push("api.example.com".to_string());

        seed_consent(&plan).expect("seed consent store");

        let consent_dir = home.path().join(".ato").join("consent");
        let consent_file = consent_dir.join(CONSENT_FILE_NAME);
        let dir_mode = fs::metadata(&consent_dir)
            .expect("consent dir metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(&consent_file)
            .expect("consent file metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }

    /// Regression for #116: a test fixture must be able to isolate its
    /// consent state by setting `ATO_HOME=$tmp` alone, without touching
    /// the user's real `HOME`. The receipt that surfaced #116 reported
    /// the opposite — that `executionplan_v1.jsonl` lived under
    /// `~/.ato/consent/` regardless of `ATO_HOME`. That observation was
    /// a misattribution (the E302 reappearance came from the
    /// per-desktop-process retry-once budget, not the on-disk consent
    /// log), but there was no consent-store-level test that locked the
    /// invariant in. This is that test: a fresh `ATO_HOME` produces an
    /// isolated consent log, and writes never land under `HOME/.ato/`.
    #[test]
    #[cfg(unix)]
    #[serial_test::serial]
    fn approve_execution_plan_consent_isolates_via_ato_home() {
        let _serial = env_lock().lock().unwrap();
        let real_home = TempDir::new().expect("create temporary HOME");
        let ato_home = TempDir::new().expect("create temporary ATO_HOME");

        // Set HOME to a *different* temp dir so a regression that
        // anchors consent to `~/.ato` would land there visibly instead
        // of silently passing because HOME and ATO_HOME happened to
        // coincide.
        let _home_guard = EnvVarGuard::set("HOME", Some(&real_home.path().to_string_lossy()));
        let _ato_home_guard =
            EnvVarGuard::set("ATO_HOME", Some(&ato_home.path().to_string_lossy()));

        approve_execution_plan_consent("publisher/app", "1.0.0", "cli", "blake3:aaa", "blake3:bbb")
            .expect("approve under ATO_HOME");

        let ato_home_consent = ato_home.path().join("consent").join(CONSENT_FILE_NAME);
        assert!(
            ato_home_consent.exists(),
            "consent record must land under ATO_HOME ({}), not HOME ({})",
            ato_home.path().display(),
            real_home.path().display(),
        );
        let home_consent = real_home
            .path()
            .join(".ato")
            .join("consent")
            .join(CONSENT_FILE_NAME);
        assert!(
            !home_consent.exists(),
            "consent record must NOT leak into HOME/.ato/consent when ATO_HOME is set; found: {}",
            home_consent.display(),
        );

        // Re-reading via has_consent must agree — proves the read path
        // and the write path resolve the same ATO_HOME-anchored file.
        let plan = non_trivial_plan();
        approve_execution_plan_consent(
            &plan.consent.key.scoped_id,
            &plan.consent.key.version,
            &plan.consent.key.target_label,
            &plan.consent.policy_segment_hash,
            &plan.consent.provisioning_policy_hash,
        )
        .expect("approve plan under ATO_HOME");
        assert!(
            has_consent(&plan).expect("has_consent under ATO_HOME"),
            "approve write + has_consent read must agree under ATO_HOME isolation"
        );
    }
}
