use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::ato_lock::DeliveryEnvironment;
use chrono::Utc;
use dirs::home_dir;
use serde::{Deserialize, Serialize};

use crate::cli::{ModelTierArg, PrivacyModeArg, RepairActionArg};

const DESKY_PACKAGE_ID: &str = "ato/desky";
const SCHEMA_VERSION: &str = "desky-control-plane/v1";
const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct InstallBootstrapStateWriteResult {
    pub state_path: PathBuf,
    pub state: StoredBootstrapState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredBootstrapState {
    pub state_version: u32,
    pub materialization: MaterializationState,
    pub personalization: PersonalizationState,
    pub health: HealthState,
    pub repair_history: Vec<RepairRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaterializationState {
    pub shell_installed: bool,
    pub opencode_installed: bool,
    pub ollama_mode: String,
    pub bootstrap_phase: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersonalizationState {
    pub workspace_path: Option<String>,
    pub model_tier: Option<String>,
    pub privacy_mode: Option<String>,
    pub submitted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthState {
    pub overall: String,
    pub services: BTreeMap<String, String>,
    pub last_error: Option<String>,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepairRecord {
    pub action: String,
    pub status: String,
    pub detail: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct VersionlessStoredBootstrapState {
    materialization: MaterializationState,
    personalization: PersonalizationState,
    health: HealthState,
    repair_history: Vec<RepairRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct StatusEnvelope<'a> {
    schema_version: &'static str,
    package_id: &'a str,
    control_plane: &'static str,
    state_path: String,
    state: StoredBootstrapState,
}

#[derive(Debug, Clone, Serialize)]
struct BootstrapEnvelope<'a> {
    schema_version: &'static str,
    package_id: &'a str,
    action: &'static str,
    state_path: String,
    state: StoredBootstrapState,
}

#[derive(Debug, Clone, Serialize)]
struct RepairEnvelope<'a> {
    schema_version: &'static str,
    package_id: &'a str,
    action: String,
    state_path: String,
    state: StoredBootstrapState,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyBootstrapState {
    desired_personalization: Option<LegacyDesiredPersonalization>,
    observed_machine_state: LegacyObservedMachineState,
    materialization_progress: LegacyMaterializationProgress,
    service_health_snapshot: Vec<LegacyServiceHealth>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyDesiredPersonalization {
    workspace_path: String,
    model_tier: String,
    privacy_mode: String,
    submitted_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyObservedMachineState {
    ollama_mode: String,
}

#[derive(Debug, Clone, Deserialize)]
struct LegacyMaterializationProgress {
    phase: String,
    ready: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyServiceHealth {
    name: String,
    status: String,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

pub fn write_install_bootstrap_state(
    package_id: &str,
    environment: &DeliveryEnvironment,
    shell_installed: bool,
    projection_performed: bool,
) -> Result<InstallBootstrapStateWriteResult> {
    ensure_desky_package(package_id)?;
    let path = bootstrap_state_path();
    let state = build_install_bootstrap_state(environment, shell_installed, projection_performed);
    write_state_to_path(&path, &state)?;
    Ok(InstallBootstrapStateWriteResult {
        state_path: path,
        state,
    })
}

pub fn status(package_id: &str, json: bool) -> Result<()> {
    ensure_desky_package(package_id)?;
    let path = bootstrap_state_path();
    let state = load_state_from_path(&path)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&build_status_envelope(package_id, &path, state))?
        );
        return Ok(());
    }

    println!("App: {package_id}");
    println!("Control plane: ato-cli");
    println!("State path: {}", path.display());
    println!("State version: {}", state.state_version);
    println!("Bootstrap phase: {}", state.materialization.bootstrap_phase);
    println!("Health: {}", state.health.overall);
    for (service, service_state) in state.health.services {
        println!("  - {service}: {service_state}");
    }
    Ok(())
}

pub fn bootstrap(
    package_id: &str,
    finalize: bool,
    workspace: Option<&str>,
    model_tier: Option<ModelTierArg>,
    privacy_mode: Option<PrivacyModeArg>,
    json: bool,
) -> Result<()> {
    ensure_desky_package(package_id)?;
    if !finalize {
        anyhow::bail!("MVP only supports `ato app bootstrap ato/desky --finalize` right now.");
    }

    let workspace = workspace
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--workspace is required with --finalize"))?;
    let model_tier =
        model_tier.ok_or_else(|| anyhow::anyhow!("--model-tier is required with --finalize"))?;
    let privacy_mode = privacy_mode
        .ok_or_else(|| anyhow::anyhow!("--privacy-mode is required with --finalize"))?;

    let path = bootstrap_state_path();
    let mut state = load_state_from_path(&path)?;
    state.personalization.workspace_path = Some(workspace.to_string());
    state.personalization.model_tier = Some(model_tier.as_str().to_string());
    state.personalization.privacy_mode = Some(privacy_mode.as_str().to_string());
    state.personalization.submitted_at = Some(now_iso());
    state.health.checked_at = now_iso();
    recalculate_stub_health(&mut state);
    state.materialization.bootstrap_phase = phase_after_personalization(&state).to_string();
    write_state_to_path(&path, &state)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&build_bootstrap_envelope(package_id, &path, state))?
        );
        return Ok(());
    }

    println!("✅ Finalized {package_id}");
    println!("   workspace: {workspace}");
    println!("   model_tier: {}", model_tier.as_str());
    println!("   privacy_mode: {}", privacy_mode.as_str());
    println!("   state_path: {}", path.display());
    Ok(())
}

pub fn repair(package_id: &str, action: RepairActionArg, json: bool) -> Result<()> {
    ensure_desky_package(package_id)?;
    let path = bootstrap_state_path();
    let mut state = load_state_from_path(&path)?;

    let detail = match action {
        RepairActionArg::RestartServices => {
            state.health.last_error = None;
            "Re-ran stub service health checks for Desky control-plane state.".to_string()
        }
        RepairActionArg::RewriteConfig => {
            state.health.last_error = None;
            if state.personalization.submitted_at.is_some() {
                state.materialization.bootstrap_phase =
                    phase_after_personalization(&state).to_string();
            }
            "Rewrote Desky bootstrap configuration state.".to_string()
        }
        RepairActionArg::SwitchModelTier => {
            state.personalization.model_tier = Some("fallback".to_string());
            state.health.last_error = None;
            "Switched Desky model tier to fallback.".to_string()
        }
    };

    state.health.checked_at = now_iso();
    recalculate_stub_health(&mut state);
    state.repair_history.push(RepairRecord {
        action: action.as_str().to_string(),
        status: "applied".to_string(),
        detail: detail.clone(),
        recorded_at: now_iso(),
    });
    write_state_to_path(&path, &state)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&build_repair_envelope(package_id, &path, action, state))?
        );
        return Ok(());
    }

    println!("✅ Repair applied for {package_id}");
    println!("   action: {}", action.as_str());
    println!("   detail: {detail}");
    println!("   state_path: {}", path.display());
    Ok(())
}

fn build_status_envelope<'a>(
    package_id: &'a str,
    path: &Path,
    state: StoredBootstrapState,
) -> StatusEnvelope<'a> {
    StatusEnvelope {
        schema_version: SCHEMA_VERSION,
        package_id,
        control_plane: "ato-cli",
        state_path: path.display().to_string(),
        state,
    }
}

fn build_bootstrap_envelope<'a>(
    package_id: &'a str,
    path: &Path,
    state: StoredBootstrapState,
) -> BootstrapEnvelope<'a> {
    BootstrapEnvelope {
        schema_version: SCHEMA_VERSION,
        package_id,
        action: "bootstrap_finalize",
        state_path: path.display().to_string(),
        state,
    }
}

fn build_repair_envelope<'a>(
    package_id: &'a str,
    path: &Path,
    action: RepairActionArg,
    state: StoredBootstrapState,
) -> RepairEnvelope<'a> {
    RepairEnvelope {
        schema_version: SCHEMA_VERSION,
        package_id,
        action: action.as_str().to_string(),
        state_path: path.display().to_string(),
        state,
    }
}

fn build_install_bootstrap_state(
    environment: &DeliveryEnvironment,
    shell_installed: bool,
    projection_performed: bool,
) -> StoredBootstrapState {
    let mut services = BTreeMap::new();
    let mut opencode_installed = false;
    let mut ollama_mode = "reused".to_string();

    for service in &environment.services {
        let service_name = service.name.trim();
        if service_name.is_empty() {
            continue;
        }
        services.insert(service_name.to_string(), "healthy".to_string());
        if service_name.eq_ignore_ascii_case("opencode") {
            opencode_installed = true;
        }
        if service_name.eq_ignore_ascii_case("ollama")
            && service.lifecycle.eq_ignore_ascii_case("managed")
        {
            ollama_mode = "managed".to_string();
        }
    }

    if !services.contains_key("ollama") {
        services.insert("ollama".to_string(), "healthy".to_string());
    }
    if !services.contains_key("opencode") {
        services.insert("opencode".to_string(), "missing".to_string());
    }

    let mut state = StoredBootstrapState {
        state_version: STATE_VERSION,
        materialization: MaterializationState {
            shell_installed,
            opencode_installed,
            ollama_mode,
            bootstrap_phase: if projection_performed {
                "shell_projected".to_string()
            } else {
                "environment_materialized".to_string()
            },
        },
        personalization: PersonalizationState {
            workspace_path: None,
            model_tier: None,
            privacy_mode: None,
            submitted_at: None,
        },
        health: HealthState {
            overall: "degraded".to_string(),
            services,
            last_error: None,
            checked_at: now_iso(),
        },
        repair_history: Vec::new(),
    };
    recalculate_stub_health(&mut state);
    state
}

fn phase_after_personalization(state: &StoredBootstrapState) -> &'static str {
    if state.materialization.opencode_installed && state.health.overall == "healthy" {
        "ready"
    } else {
        "personalization_finalized"
    }
}

fn recalculate_stub_health(state: &mut StoredBootstrapState) {
    let ollama_status = if state
        .materialization
        .ollama_mode
        .eq_ignore_ascii_case("missing")
    {
        "missing"
    } else {
        "healthy"
    };
    let opencode_status = if state.materialization.opencode_installed {
        "healthy"
    } else {
        "missing"
    };

    state
        .health
        .services
        .insert("ollama".to_string(), ollama_status.to_string());
    state
        .health
        .services
        .insert("opencode".to_string(), opencode_status.to_string());

    if state.materialization.opencode_installed && ollama_status == "healthy" {
        state.health.overall = "healthy".to_string();
        if state.health.last_error.as_deref() == Some("opencode_not_materialized") {
            state.health.last_error = None;
        }
    } else {
        state.health.overall = "degraded".to_string();
        if !state.materialization.opencode_installed {
            state.health.last_error = Some("opencode_not_materialized".to_string());
        }
    }
}

fn ensure_desky_package(package_id: &str) -> Result<()> {
    if package_id.trim() != DESKY_PACKAGE_ID {
        anyhow::bail!(
            "MVP app control plane currently supports only `{}` (got `{}`).",
            DESKY_PACKAGE_ID,
            package_id
        );
    }
    Ok(())
}

fn bootstrap_state_path() -> PathBuf {
    if let Ok(path) = std::env::var("DESKY_BOOTSTRAP_STATE_PATH") {
        return PathBuf::from(path);
    }

    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ato")
        .join("apps")
        .join("desky")
        .join("bootstrap-state.json")
}

fn load_state_from_path(path: &Path) -> Result<StoredBootstrapState> {
    match fs::read_to_string(path) {
        Ok(raw) => deserialize_state(&raw)
            .with_context(|| format!("failed to parse Desky bootstrap state: {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(default_state()),
        Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn write_state_to_path(path: &Path, state: &StoredBootstrapState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn normalize_state_version(mut state: StoredBootstrapState) -> StoredBootstrapState {
    state.state_version = STATE_VERSION;
    state
}

fn deserialize_state(raw: &str) -> Result<StoredBootstrapState> {
    if let Ok(state) = serde_json::from_str::<StoredBootstrapState>(raw) {
        return Ok(normalize_state_version(state));
    }

    if let Ok(state) = serde_json::from_str::<VersionlessStoredBootstrapState>(raw) {
        return Ok(normalize_state_version(StoredBootstrapState {
            state_version: STATE_VERSION,
            materialization: state.materialization,
            personalization: state.personalization,
            health: state.health,
            repair_history: state.repair_history,
        }));
    }

    let legacy = serde_json::from_str::<LegacyBootstrapState>(raw)?;
    Ok(normalize_state_version(StoredBootstrapState {
        state_version: STATE_VERSION,
        materialization: MaterializationState {
            shell_installed: true,
            opencode_installed: legacy
                .service_health_snapshot
                .iter()
                .find(|service| service.name == "opencode")
                .map(|service| service.status == "healthy")
                .unwrap_or(false),
            ollama_mode: legacy.observed_machine_state.ollama_mode,
            bootstrap_phase: if legacy.materialization_progress.ready {
                "ready".to_string()
            } else {
                legacy.materialization_progress.phase
            },
        },
        personalization: PersonalizationState {
            workspace_path: legacy
                .desired_personalization
                .as_ref()
                .map(|value| value.workspace_path.clone()),
            model_tier: legacy
                .desired_personalization
                .as_ref()
                .map(|value| value.model_tier.clone()),
            privacy_mode: legacy
                .desired_personalization
                .as_ref()
                .map(|value| value.privacy_mode.clone()),
            submitted_at: legacy
                .desired_personalization
                .and_then(|value| value.submitted_at),
        },
        health: HealthState {
            overall: if legacy.materialization_progress.ready {
                "healthy".to_string()
            } else {
                "degraded".to_string()
            },
            services: legacy
                .service_health_snapshot
                .into_iter()
                .map(|service| (service.name, service.status))
                .collect(),
            last_error: legacy.last_error,
            checked_at: now_iso(),
        },
        repair_history: Vec::new(),
    }))
}

fn default_state() -> StoredBootstrapState {
    let mut services = BTreeMap::new();
    services.insert("ollama".to_string(), "healthy".to_string());
    services.insert("opencode".to_string(), "missing".to_string());
    StoredBootstrapState {
        state_version: STATE_VERSION,
        materialization: MaterializationState {
            shell_installed: true,
            opencode_installed: false,
            ollama_mode: "reused".to_string(),
            bootstrap_phase: "awaiting_personalization".to_string(),
        },
        personalization: PersonalizationState {
            workspace_path: None,
            model_tier: None,
            privacy_mode: None,
            submitted_at: None,
        },
        health: HealthState {
            overall: "degraded".to_string(),
            services,
            last_error: Some("opencode_not_materialized".to_string()),
            checked_at: now_iso(),
        },
        repair_history: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::*;

    fn assert_snapshot(name: &str, actual: String) {
        let snapshot_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/app_control/snapshots")
            .join(format!("{name}.json"));
        let expected =
            fs::read_to_string(&snapshot_path).expect("snapshot fixture should be readable");
        assert_eq!(
            actual,
            expected,
            "snapshot mismatch: {}",
            snapshot_path.display()
        );
    }

    fn sample_path() -> PathBuf {
        PathBuf::from("/Users/test/.ato/apps/desky/bootstrap-state.json")
    }

    fn sample_state() -> StoredBootstrapState {
        let mut services = BTreeMap::new();
        services.insert("ollama".to_string(), "healthy".to_string());
        services.insert("opencode".to_string(), "missing".to_string());
        StoredBootstrapState {
            state_version: STATE_VERSION,
            materialization: MaterializationState {
                shell_installed: true,
                opencode_installed: false,
                ollama_mode: "reused".to_string(),
                bootstrap_phase: "personalization_finalized".to_string(),
            },
            personalization: PersonalizationState {
                workspace_path: Some("/Users/test/Workspace".to_string()),
                model_tier: Some("balanced".to_string()),
                privacy_mode: Some("strict".to_string()),
                submitted_at: Some("2026-04-02T00:00:00+00:00".to_string()),
            },
            health: HealthState {
                overall: "degraded".to_string(),
                services,
                last_error: Some("opencode_not_materialized".to_string()),
                checked_at: "2026-04-02T00:00:01+00:00".to_string(),
            },
            repair_history: vec![RepairRecord {
                action: "restart-services".to_string(),
                status: "applied".to_string(),
                detail: "Re-ran stub service health checks for Desky control-plane state."
                    .to_string(),
                recorded_at: "2026-04-02T00:00:02+00:00".to_string(),
            }],
        }
    }

    #[test]
    fn deserializes_current_control_plane_shape() {
        let state = default_state();
        let json = serde_json::to_string(&state).expect("serialize current state");
        let parsed = deserialize_state(&json).expect("parse current state");
        assert_eq!(parsed, state);
    }

    #[test]
    fn migrates_versionless_control_plane_shape() {
        let raw = r#"{
          "materialization": {
            "shell_installed": true,
            "opencode_installed": false,
            "ollama_mode": "reused",
            "bootstrap_phase": "awaiting_personalization"
          },
          "personalization": {
            "workspace_path": null,
            "model_tier": null,
            "privacy_mode": null,
            "submitted_at": null
          },
          "health": {
            "overall": "degraded",
            "services": {
              "ollama": "healthy",
              "opencode": "missing"
            },
            "last_error": "opencode_not_materialized",
            "checked_at": "2026-04-02T00:00:00+00:00"
          },
          "repair_history": []
        }"#;
        let parsed = deserialize_state(raw).expect("parse versionless current state");
        assert_eq!(parsed.state_version, STATE_VERSION);
        assert_eq!(
            parsed.materialization.bootstrap_phase,
            "awaiting_personalization"
        );
    }

    #[test]
    fn migrates_legacy_desky_shape() {
        let raw = r#"{
          "desired_personalization": {
            "workspacePath": "~/Workspace",
            "modelTier": "balanced",
            "privacyMode": "strict",
            "submittedAt": "2026-04-02T00:00:00Z"
          },
          "observed_machine_state": {
            "ollamaMode": "reused"
          },
          "materialization_progress": {
            "phase": "finalize",
            "ready": true
          },
          "service_health_snapshot": [
            {"name": "ollama", "status": "healthy"},
            {"name": "opencode", "status": "healthy"}
          ],
          "last_error": null
        }"#;

        let parsed = deserialize_state(raw).expect("migrate legacy state");
        assert_eq!(parsed.state_version, STATE_VERSION);
        assert_eq!(parsed.materialization.bootstrap_phase, "ready");
        assert_eq!(
            parsed.personalization.model_tier.as_deref(),
            Some("balanced")
        );
        assert_eq!(
            parsed.health.services.get("opencode").map(String::as_str),
            Some("healthy")
        );
    }

    #[test]
    fn status_json_matches_snapshot() {
        let actual = serde_json::to_string_pretty(&build_status_envelope(
            DESKY_PACKAGE_ID,
            &sample_path(),
            sample_state(),
        ))
        .expect("serialize status snapshot")
            + "\n";
        assert_snapshot("status", actual);
    }

    #[test]
    fn bootstrap_json_matches_snapshot() {
        let actual = serde_json::to_string_pretty(&build_bootstrap_envelope(
            DESKY_PACKAGE_ID,
            &sample_path(),
            sample_state(),
        ))
        .expect("serialize bootstrap snapshot")
            + "\n";
        assert_snapshot("bootstrap", actual);
    }

    #[test]
    fn repair_json_matches_snapshot() {
        let actual = serde_json::to_string_pretty(&build_repair_envelope(
            DESKY_PACKAGE_ID,
            &sample_path(),
            RepairActionArg::RestartServices,
            sample_state(),
        ))
        .expect("serialize repair snapshot")
            + "\n";
        assert_snapshot("repair", actual);
    }

    #[test]
    fn build_install_bootstrap_state_marks_materialized_services_ready() {
        let environment = DeliveryEnvironment {
            strategy: "ato-managed".to_string(),
            target: Some("desktop".to_string()),
            services: vec![
                capsule_core::ato_lock::DeliveryService {
                    name: "ollama".to_string(),
                    from: "dependency:ollama".to_string(),
                    lifecycle: "managed".to_string(),
                    depends_on: Vec::new(),
                    healthcheck: None,
                },
                capsule_core::ato_lock::DeliveryService {
                    name: "opencode".to_string(),
                    from: "dependency:opencode".to_string(),
                    lifecycle: "on-demand".to_string(),
                    depends_on: vec!["ollama".to_string()],
                    healthcheck: None,
                },
            ],
            bootstrap: None,
            repair: None,
        };

        let state = build_install_bootstrap_state(&environment, true, true);
        assert_eq!(state.materialization.bootstrap_phase, "shell_projected");
        assert!(state.materialization.opencode_installed);
        assert_eq!(state.materialization.ollama_mode, "managed");
        assert_eq!(state.health.overall, "healthy");
    }
}
