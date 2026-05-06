use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use capsule_core::ato_lock::DeliveryEnvironment;
use capsule_core::ccp::SCHEMA_VERSION;
use capsule_core::types::ServiceSpec;
use chrono::Utc;
use dirs::home_dir;
use serde::{Deserialize, Serialize};

use crate::application::services::{
    ServiceGraphPlan, ServicePhaseCoordinator, ServicePhaseRuntime,
};
use crate::cli::{ModelTierArg, PrivacyModeArg, RepairActionArg};

mod guest_contract;
mod latest;
mod resolve;
mod session;
mod session_runner;

pub use latest::fetch_latest;
pub use resolve::resolve_handle;
pub use session::{start_session, stop_session};

// App Session Materialization (RFC: APP_SESSION_MATERIALIZATION) consumes a
// few in-module helpers from session. Re-exported as pub(crate) so the
// materialization layer can share storage / probe primitives without
// flipping the entire `session` module to public. The session-record
// schema itself now lives in `ato-session-core` (RFC §3.2 PR 4A.0);
// `session::StoredSessionInfo` is a re-export of that shared type.
pub(crate) use session::{http_get_ok, session_root, StoredSessionInfo};

/// Canonical package identifier for the ato-desktop control-plane envelope.
/// Renamed from the legacy `DESKY_PACKAGE_ID` (= "ato/desky") in line with the
/// brand-wide rename to `ato-desktop`. The wire `package_id` field is
/// informational only — `tolerance.rs` validates `schema_version`, not
/// `package_id` — so changing the value here is safe for existing desktop
/// builds.
const ATO_DESKTOP_PACKAGE_ID: &str = "ato/ato-desktop";
/// Legacy wire identifier preserved for documentation. The actual back-compat
/// checks live in `application/engine/install/mod.rs::materialize_ato_managed_environment`
/// (which accepts both this and [`ATO_DESKTOP_PACKAGE_ID`]) and in the curated
/// install alias map (`cli/dispatch/install.rs::CURATED_INSTALL_ALIASES`).
#[allow(dead_code)]
const LEGACY_DESKY_PACKAGE_ID: &str = "ato/desky";
// CCP wire version is owned by `capsule_core::ccp::SCHEMA_VERSION` so the
// Desktop consumer and the CLI producer share one source of truth. See
// `docs/monorepo-consolidation-plan.md` §M4.
const STATE_VERSION: u32 = 1;
const MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR: &str = "managed_environment_not_materialized";
const UNMATERIALIZED_SERVICE_STATUS: &str = "unmaterialized";

#[derive(Debug, Clone)]
pub struct InstallBootstrapStateWriteResult {
    pub state_path: PathBuf,
    pub service_root: PathBuf,
    pub state: StoredBootstrapState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MaterializedServiceRecord {
    name: String,
    source: String,
    lifecycle: String,
    depends_on: Vec<String>,
    healthcheck_url: Option<String>,
    helper_command: Option<String>,
    pid: Option<u32>,
    log_path: Option<String>,
    status: String,
    materialized_at: String,
    last_started_at: Option<String>,
    last_stopped_at: Option<String>,
    checked_at: String,
}

#[derive(Debug, Clone)]
struct MaterializedServiceOutcome {
    service_root: PathBuf,
    services: Vec<MaterializedServiceRecord>,
}

#[derive(Debug, Clone)]
struct ManagedServiceRuntime {
    service_root: PathBuf,
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
    ensure_ato_desktop_package(package_id)?;
    let path = bootstrap_state_path();
    let mut materialized = materialize_managed_services(package_id, environment)?;
    materialized.services = load_materialized_service_records(&materialized.service_root)?;
    let state = build_install_bootstrap_state(
        environment,
        &materialized,
        shell_installed,
        projection_performed,
    );
    write_state_to_path(&path, &state)?;
    Ok(InstallBootstrapStateWriteResult {
        state_path: path,
        service_root: materialized.service_root,
        state,
    })
}

pub fn status(package_id: &str, json: bool) -> Result<()> {
    ensure_ato_desktop_package(package_id)?;
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
    ensure_ato_desktop_package(package_id)?;
    if !finalize {
        anyhow::bail!(
            "MVP only supports `ato app bootstrap ato/ato-desktop --finalize` right now."
        );
    }

    let service_root = ensure_ato_desktop_managed_environment_materialized()?;

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
    let _ = orchestrate_managed_services(&service_root);
    refresh_materialized_service_health(&mut state);
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
    ensure_ato_desktop_package(package_id)?;
    let path = bootstrap_state_path();
    let mut state = load_state_from_path(&path)?;

    let detail = match action {
        RepairActionArg::RestartServices => {
            let service_root = ensure_ato_desktop_managed_environment_materialized()?;
            let _ = stop_managed_services(&service_root);
            let _ = orchestrate_managed_services(&service_root);
            refresh_materialized_service_health(&mut state);
            state.health.last_error = None;
            "Stopped materialized services, then re-ran start and readiness checks for ato-desktop control-plane state.".to_string()
        }
        RepairActionArg::RewriteConfig => {
            state.health.last_error = None;
            if state.personalization.submitted_at.is_some() {
                state.materialization.bootstrap_phase =
                    phase_after_personalization(&state).to_string();
            }
            "Rewrote ato-desktop bootstrap configuration state.".to_string()
        }
        RepairActionArg::SwitchModelTier => {
            state.personalization.model_tier = Some("fallback".to_string());
            state.health.last_error = None;
            "Switched ato-desktop model tier to fallback.".to_string()
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
    materialized: &MaterializedServiceOutcome,
    shell_installed: bool,
    projection_performed: bool,
) -> StoredBootstrapState {
    let mut services = BTreeMap::new();
    let mut opencode_installed = false;
    let mut ollama_mode = "reused".to_string();

    for service in &materialized.services {
        let status = if projection_performed {
            "missing"
        } else {
            service.status.as_str()
        };
        services.insert(service.name.clone(), status.to_string());
        if service.name.eq_ignore_ascii_case("opencode") && status != "missing" {
            opencode_installed = true;
        }
        if service.name.eq_ignore_ascii_case("ollama") {
            if service.lifecycle.eq_ignore_ascii_case("managed") {
                ollama_mode = "managed".to_string();
            }
            if status == "missing" {
                ollama_mode = "missing".to_string();
            }
        }
    }

    if !services.contains_key("ollama") {
        let ollama_status = if environment
            .services
            .iter()
            .any(|service| service.name.eq_ignore_ascii_case("ollama"))
        {
            "pending"
        } else {
            "healthy"
        };
        services.insert("ollama".to_string(), ollama_status.to_string());
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

fn ato_desktop_delivery_environment() -> DeliveryEnvironment {
    DeliveryEnvironment {
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
    }
}

fn managed_service_layout_is_materialized(
    service_root: &Path,
    environment: &DeliveryEnvironment,
) -> bool {
    service_root.exists()
        && environment.services.iter().all(|service| {
            let service_dir = service_root.join(service_dir_name(&service.name));
            let record_exists = service_dir.join("service.json").exists();
            let helper_exists = default_service_helper_command(service)
                .map(|_| service_dir.join("run.sh").exists())
                .unwrap_or(true);
            record_exists && helper_exists
        })
}

fn ensure_ato_desktop_managed_environment_materialized() -> Result<PathBuf> {
    let environment = ato_desktop_delivery_environment();
    let service_root = managed_service_root(ATO_DESKTOP_PACKAGE_ID)?;
    if managed_service_layout_is_materialized(&service_root, &environment) {
        return Ok(service_root);
    }

    let _ = materialize_managed_services(ATO_DESKTOP_PACKAGE_ID, &environment)?;
    Ok(service_root)
}

fn mark_managed_environment_unmaterialized(state: &mut StoredBootstrapState) {
    for service in ato_desktop_delivery_environment().services {
        state
            .health
            .services
            .insert(service.name, UNMATERIALIZED_SERVICE_STATUS.to_string());
    }
    state.materialization.opencode_installed = false;
    state.materialization.ollama_mode = "missing".to_string();
    state.health.overall = "degraded".to_string();
    state.health.last_error = Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR.to_string());
    state.health.checked_at = now_iso();
}

fn recalculate_stub_health(state: &mut StoredBootstrapState) {
    let ollama_status = state
        .health
        .services
        .get("ollama")
        .cloned()
        .unwrap_or_else(|| {
            if state
                .materialization
                .ollama_mode
                .eq_ignore_ascii_case("missing")
            {
                "missing".to_string()
            } else {
                "healthy".to_string()
            }
        });
    let opencode_status = state
        .health
        .services
        .get("opencode")
        .cloned()
        .unwrap_or_else(|| {
            if state.materialization.opencode_installed {
                "healthy".to_string()
            } else {
                "missing".to_string()
            }
        });

    state.materialization.opencode_installed =
        matches!(opencode_status.as_str(), "healthy" | "pending");
    if matches!(
        ollama_status.as_str(),
        "missing" | UNMATERIALIZED_SERVICE_STATUS
    ) {
        state.materialization.ollama_mode = "missing".to_string();
    }

    state
        .health
        .services
        .insert("ollama".to_string(), ollama_status.clone());
    state
        .health
        .services
        .insert("opencode".to_string(), opencode_status.clone());

    if opencode_status == "healthy" && ollama_status == "healthy" {
        state.health.overall = "healthy".to_string();
        if state.health.last_error.as_deref() == Some("opencode_not_materialized")
            || state.health.last_error.as_deref() == Some("ollama_not_ready")
            || state.health.last_error.as_deref()
                == Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR)
        {
            state.health.last_error = None;
        }
    } else {
        state.health.overall = "degraded".to_string();
        if opencode_status == UNMATERIALIZED_SERVICE_STATUS
            || ollama_status == UNMATERIALIZED_SERVICE_STATUS
        {
            state.health.last_error = Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR.to_string());
        } else if opencode_status == "missing" {
            state.health.last_error = Some("opencode_not_materialized".to_string());
        } else if ollama_status != "healthy" {
            state.health.last_error = Some("ollama_not_ready".to_string());
        }
    }
}

fn materialize_managed_services(
    package_id: &str,
    environment: &DeliveryEnvironment,
) -> Result<MaterializedServiceOutcome> {
    let service_root = managed_service_root(package_id)?;
    fs::create_dir_all(&service_root)
        .with_context(|| format!("failed to create {}", service_root.display()))?;

    let mut services = Vec::new();
    for service in &environment.services {
        let name = service.name.trim();
        if name.is_empty() {
            continue;
        }

        let service_dir = service_root.join(service_dir_name(name));
        fs::create_dir_all(&service_dir)
            .with_context(|| format!("failed to create {}", service_dir.display()))?;

        let helper_command = default_service_helper_command(service);
        if let Some(command) = &helper_command {
            write_service_helper(&service_dir.join("run.sh"), command)?;
        }

        let status = evaluate_service_status(service, helper_command.as_deref());
        let record = MaterializedServiceRecord {
            name: name.to_string(),
            source: service.from.clone(),
            lifecycle: service.lifecycle.clone(),
            depends_on: service.depends_on.clone(),
            healthcheck_url: service
                .healthcheck
                .as_ref()
                .and_then(|check| check.url.clone())
                .or_else(|| default_service_healthcheck_url(name)),
            helper_command,
            pid: None,
            log_path: Some(service_dir.join("service.log").display().to_string()),
            status,
            materialized_at: now_iso(),
            last_started_at: None,
            last_stopped_at: None,
            checked_at: now_iso(),
        };
        write_materialized_service_record(&service_root, &record)?;
        services.push(record);
    }

    Ok(MaterializedServiceOutcome {
        service_root,
        services,
    })
}

fn refresh_materialized_service_health(state: &mut StoredBootstrapState) {
    let environment = ato_desktop_delivery_environment();
    let root = managed_service_root(ATO_DESKTOP_PACKAGE_ID).ok();
    let Some(root) = root else {
        mark_managed_environment_unmaterialized(state);
        return;
    };
    if !managed_service_layout_is_materialized(&root, &environment) {
        mark_managed_environment_unmaterialized(state);
        return;
    }

    let Ok(records) = load_materialized_service_records(&root) else {
        mark_managed_environment_unmaterialized(state);
        return;
    };

    for mut record in records {
        record.status = evaluate_materialized_service_status(&record);
        record.checked_at = now_iso();
        let _ = write_materialized_service_record(&root, &record);
        state
            .health
            .services
            .insert(record.name.clone(), record.status);
    }

    recalculate_stub_health(state);
}

fn load_materialized_service_records(root: &Path) -> Result<Vec<MaterializedServiceRecord>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
        let record_path = entry.path().join("service.json");
        if !record_path.exists() {
            continue;
        }
        let raw = fs::read_to_string(&record_path)
            .with_context(|| format!("failed to read {}", record_path.display()))?;
        let record = serde_json::from_str::<MaterializedServiceRecord>(&raw)
            .with_context(|| format!("failed to parse {}", record_path.display()))?;
        records.push(record);
    }
    records.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(records)
}

fn write_materialized_service_record(
    root: &Path,
    record: &MaterializedServiceRecord,
) -> Result<()> {
    let record_path = root
        .join(service_dir_name(&record.name))
        .join("service.json");
    fs::write(&record_path, serde_json::to_string_pretty(record)?)
        .with_context(|| format!("failed to write {}", record_path.display()))
}

fn evaluate_service_status(
    service: &capsule_core::ato_lock::DeliveryService,
    helper_command: Option<&str>,
) -> String {
    let record = MaterializedServiceRecord {
        name: service.name.clone(),
        source: service.from.clone(),
        lifecycle: service.lifecycle.clone(),
        depends_on: service.depends_on.clone(),
        healthcheck_url: service
            .healthcheck
            .as_ref()
            .and_then(|healthcheck| healthcheck.url.clone())
            .or_else(|| default_service_healthcheck_url(&service.name)),
        helper_command: helper_command.map(str::to_string),
        pid: None,
        log_path: None,
        status: "pending".to_string(),
        materialized_at: now_iso(),
        last_started_at: None,
        last_stopped_at: None,
        checked_at: now_iso(),
    };
    evaluate_materialized_service_status(&record)
}

fn evaluate_materialized_service_status(record: &MaterializedServiceRecord) -> String {
    if let Some(pid) = record.pid {
        if !process_is_running(pid) {
            return "missing".to_string();
        }
    }

    if let Some(url) = record.healthcheck_url.as_deref() {
        if probe_url(url) {
            return "healthy".to_string();
        }
        if record.helper_command.is_some() && known_service_binary_exists(&record.name) {
            return "pending".to_string();
        }
        return "missing".to_string();
    }

    if record.helper_command.is_some() && known_service_binary_exists(&record.name) {
        return "pending".to_string();
    }

    if record.lifecycle.eq_ignore_ascii_case("on-demand")
        && known_service_binary_exists(&record.name)
    {
        return "pending".to_string();
    }

    "missing".to_string()
}

fn probe_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let Some(port) = parsed.port_or_known_default() else {
        return false;
    };

    let Ok(addresses) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    addresses
        .into_iter()
        .any(|address| TcpStream::connect_timeout(&address, Duration::from_millis(750)).is_ok())
}

fn default_service_healthcheck_url(service_name: &str) -> Option<String> {
    match service_name.to_ascii_lowercase().as_str() {
        "ollama" => Some("http://127.0.0.1:11434/api/tags".to_string()),
        "opencode" => Some("http://127.0.0.1:4096/session".to_string()),
        _ => None,
    }
}

fn stop_managed_services(service_root: &Path) -> Result<()> {
    let records = load_materialized_service_records(service_root)?;
    for mut record in records.into_iter().rev() {
        if let Some(pid) = record.pid {
            let _ = terminate_process(pid);
        }
        record.pid = None;
        record.last_stopped_at = Some(now_iso());
        record.checked_at = now_iso();
        record.status =
            if record.helper_command.is_some() && known_service_binary_exists(&record.name) {
                "pending".to_string()
            } else {
                "missing".to_string()
            };
        write_materialized_service_record(service_root, &record)?;
    }
    Ok(())
}

fn process_is_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

fn terminate_process(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .with_context(|| format!("failed to send TERM to pid {pid}"))?;
        if !status.success() {
            anyhow::bail!("kill -TERM {} exited with {}", pid, status);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .with_context(|| format!("failed to taskkill pid {pid}"))?;
        if !status.success() {
            anyhow::bail!("taskkill {} exited with {}", pid, status);
        }
        return Ok(());
    }
}

fn orchestrate_managed_services(service_root: &Path) -> Result<()> {
    let records = load_materialized_service_records(service_root)?;
    if records.is_empty() {
        return Ok(());
    }

    let graph = build_managed_service_graph(&records, service_root)?;
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        drop(handle);
        let service_root = service_root.to_path_buf();
        return std::thread::spawn(move || {
            let records = load_materialized_service_records(&service_root)?;
            let graph = build_managed_service_graph(&records, &service_root)?;
            let runtime = ManagedServiceRuntime { service_root };
            let coordinator = ServicePhaseCoordinator::new(&graph);
            let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
            rt.block_on(async move { coordinator.run(runtime).await })
        })
        .join()
        .map_err(|_| anyhow::anyhow!("service orchestration thread panicked"))?;
    }

    let runtime = ManagedServiceRuntime {
        service_root: service_root.to_path_buf(),
    };
    let coordinator = ServicePhaseCoordinator::new(&graph);
    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(async move { coordinator.run(runtime).await })
}

fn build_managed_service_graph(
    records: &[MaterializedServiceRecord],
    service_root: &Path,
) -> Result<ServiceGraphPlan> {
    let mut services = std::collections::HashMap::new();
    for record in records {
        let entrypoint = service_root
            .join(service_dir_name(&record.name))
            .join("run.sh")
            .display()
            .to_string();
        services.insert(
            record.name.clone(),
            ServiceSpec {
                entrypoint,
                target: None,
                depends_on: (!record.depends_on.is_empty()).then_some(record.depends_on.clone()),
                expose: None,
                env: None,
                state_bindings: Vec::new(),
                readiness_probe: None,
                network: None,
            },
        );
    }
    ServiceGraphPlan::from_services(&services)
}

fn load_materialized_service_record(
    service_root: &Path,
    service_name: &str,
) -> Result<Option<MaterializedServiceRecord>> {
    let record_path = service_root
        .join(service_dir_name(service_name))
        .join("service.json");
    if !record_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&record_path)
        .with_context(|| format!("failed to read {}", record_path.display()))?;
    let record = serde_json::from_str::<MaterializedServiceRecord>(&raw)
        .with_context(|| format!("failed to parse {}", record_path.display()))?;
    Ok(Some(record))
}

#[async_trait]
impl ServicePhaseRuntime for ManagedServiceRuntime {
    async fn start_service(&self, service_name: &str) -> Result<()> {
        let Some(mut record) = load_materialized_service_record(&self.service_root, service_name)?
        else {
            return Ok(());
        };
        if record.status == "healthy" {
            return Ok(());
        }

        let service_dir = self.service_root.join(service_dir_name(service_name));
        let helper_path = service_dir.join("run.sh");
        if !helper_path.exists() {
            record.status = "missing".to_string();
            record.checked_at = now_iso();
            write_materialized_service_record(&self.service_root, &record)?;
            anyhow::bail!("missing service helper for {service_name}");
        }

        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(service_dir.join("service.log"))
            .with_context(|| format!("failed to open log for {service_name}"))?;
        let stderr = stdout
            .try_clone()
            .with_context(|| format!("failed to clone log handle for {service_name}"))?;

        match std::process::Command::new(&helper_path)
            .current_dir(&service_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
        {
            Ok(child) => {
                record.pid = Some(child.id());
                record.status = "pending".to_string();
                record.last_started_at = Some(now_iso());
                record.last_stopped_at = None;
                record.checked_at = now_iso();
                write_materialized_service_record(&self.service_root, &record)?;
                // Phase Z: synthesize a v2 execution receipt for the
                // managed-service spawn so it lands in
                // `~/.ato/executions/<id>/` next to `ato run` receipts.
                // Best-effort — failures here only weaken the audit trail.
                if let Err(err) = emit_managed_service_receipt(&record, &service_dir, &helper_path)
                {
                    eprintln!(
                        "ATO-WARN failed to emit managed-service receipt for {}: {err}",
                        service_name
                    );
                }
                Ok(())
            }
            Err(err) => {
                record.pid = None;
                record.status = if known_service_binary_exists(&record.name) {
                    "pending".to_string()
                } else {
                    "missing".to_string()
                };
                record.checked_at = now_iso();
                write_materialized_service_record(&self.service_root, &record)?;
                Err(err).with_context(|| format!("failed to start {service_name}"))
            }
        }
    }

    async fn await_readiness(&self, service_name: String) -> Result<()> {
        for _ in 0..20 {
            let Some(mut record) =
                load_materialized_service_record(&self.service_root, &service_name)?
            else {
                return Ok(());
            };
            if let Some(pid) = record.pid {
                if !process_is_running(pid) {
                    record.pid = None;
                }
            }
            record.status = evaluate_materialized_service_status(&record);
            record.checked_at = now_iso();
            write_materialized_service_record(&self.service_root, &record)?;
            if record.status == "healthy" {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        anyhow::bail!("service {} did not reach readiness", service_name)
    }
}

/// Phase Z helper: emit a v2 execution receipt for a managed-service spawn
/// using `application::managed_service_receipt::synthesize_managed_service_receipt`
/// and the standard `execution_receipts::write_receipt_document_atomic`
/// store. Returns Ok on a successful write so the caller can log only on
/// genuine failure.
fn emit_managed_service_receipt(
    record: &MaterializedServiceRecord,
    service_dir: &std::path::Path,
    helper_path: &std::path::Path,
) -> Result<()> {
    use crate::application::execution_receipts;
    use crate::application::managed_service_receipt::{
        synthesize_managed_service_receipt, ManagedServiceReceiptInput,
    };

    let input = ManagedServiceReceiptInput {
        name: record.name.as_str(),
        service_dir,
        helper_path,
        depends_on: &record.depends_on,
        lifecycle: record.lifecycle.as_str(),
        source_label: record.source.as_str(),
    };
    let (document, _execution_id) = synthesize_managed_service_receipt(&input)?;
    let _path = execution_receipts::write_receipt_document_atomic(&document)?;
    Ok(())
}

fn known_service_binary_exists(name: &str) -> bool {
    let binary = match name.to_ascii_lowercase().as_str() {
        "ollama" => "ollama",
        "opencode" => "opencode",
        _ => return false,
    };

    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(binary).exists()))
}

fn default_service_helper_command(
    service: &capsule_core::ato_lock::DeliveryService,
) -> Option<String> {
    match service.name.to_ascii_lowercase().as_str() {
        "ollama" => Some("ollama serve".to_string()),
        "opencode" => Some("opencode serve".to_string()),
        _ => None,
    }
}

fn write_service_helper(path: &Path, command: &str) -> Result<()> {
    let script = format!("#!/bin/sh\nset -eu\nexec {} \"$@\"\n", command);
    fs::write(path, script).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn managed_service_root(package_id: &str) -> Result<PathBuf> {
    ensure_ato_desktop_package(package_id)?;
    let bootstrap_root = bootstrap_state_path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("bootstrap state path has no parent"))?;
    Ok(bootstrap_root.join("services"))
}

fn service_dir_name(name: &str) -> String {
    let normalized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    normalized.trim_matches('-').to_string()
}

fn ensure_ato_desktop_package(package_id: &str) -> Result<()> {
    if package_id.trim() != ATO_DESKTOP_PACKAGE_ID {
        anyhow::bail!(
            "MVP app control plane currently supports only `{}` (got `{}`).",
            ATO_DESKTOP_PACKAGE_ID,
            package_id
        );
    }
    Ok(())
}

/// Canonical (writable) bootstrap-state location, post-rename.
///
/// `ATO_DESKTOP_BOOTSTRAP_STATE_PATH` is the new override env var;
/// `DESKY_BOOTSTRAP_STATE_PATH` is honoured as a legacy fallback so existing
/// CI / dev scripts keep working until they're updated. Likewise, the on-disk
/// path moved from `~/.ato/apps/desky/bootstrap-state.json` to
/// `~/.ato/apps/ato-desktop/bootstrap-state.json`. See `legacy_bootstrap_state_path`
/// for the read-fallback.
fn bootstrap_state_path() -> PathBuf {
    if let Ok(path) = std::env::var("ATO_DESKTOP_BOOTSTRAP_STATE_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("DESKY_BOOTSTRAP_STATE_PATH") {
        return PathBuf::from(path);
    }

    capsule_core::common::paths::ato_path_or_workspace_tmp(
        "apps/ato-desktop/bootstrap-state.json",
    )
}

/// Pre-rename bootstrap-state location. Returned only when the canonical
/// `bootstrap_state_path()` does not yet exist, so existing users keep their
/// state across the upgrade. Writes always go to the canonical path.
fn legacy_bootstrap_state_path() -> Option<PathBuf> {
    capsule_core::common::paths::ato_path("apps/desky/bootstrap-state.json").ok()
}

fn load_state_from_path(path: &Path) -> Result<StoredBootstrapState> {
    match fs::read_to_string(path) {
        Ok(raw) => {
            let mut state = deserialize_state(&raw).with_context(|| {
                format!(
                    "failed to parse ato-desktop bootstrap state: {}",
                    path.display()
                )
            })?;
            refresh_materialized_service_health(&mut state);
            Ok(state)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Try the pre-rename path so users who upgraded mid-session keep
            // their personalization / health snapshot.
            if let Some(legacy) = legacy_bootstrap_state_path() {
                if legacy != path && legacy.exists() {
                    return load_state_from_path(&legacy);
                }
            }
            Ok(default_state())
        }
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
    use std::sync::{Mutex, OnceLock};

    use super::*;

    struct EnvVarGuard {
        key: String,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &str, value: Option<&str>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self {
                key: key.to_string(),
                previous,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => std::env::set_var(&self.key, value),
                None => std::env::remove_var(&self.key),
            }
        }
    }

    fn test_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn write_fake_binary(dir: &Path, name: &str) {
        let path = dir.join(name);
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write fake binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&path).expect("binary metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("set fake binary permissions");
        }
    }

    fn assert_snapshot(name: &str, actual: String) {
        // CCP envelope fixtures live in `capsule-core` so the consumer
        // (`ato-desktop`) can run the same golden checks against the same
        // bytes. See `docs/monorepo-consolidation-plan.md` §M4.
        let snapshot_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../capsule-core/tests/fixtures/ccp")
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
        services.insert(
            "ollama".to_string(),
            UNMATERIALIZED_SERVICE_STATUS.to_string(),
        );
        services.insert(
            "opencode".to_string(),
            UNMATERIALIZED_SERVICE_STATUS.to_string(),
        );
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
                last_error: Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR.to_string()),
                checked_at: "2026-04-02T00:00:01+00:00".to_string(),
            },
            repair_history: vec![RepairRecord {
                action: "restart-services".to_string(),
                status: "applied".to_string(),
                detail: "Re-ran stub service health checks for ato-desktop control-plane state."
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
    fn migrates_legacy_ato_desktop_shape() {
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
            ATO_DESKTOP_PACKAGE_ID,
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
            ATO_DESKTOP_PACKAGE_ID,
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
            ATO_DESKTOP_PACKAGE_ID,
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
        let environment = ato_desktop_delivery_environment();

        let materialized = MaterializedServiceOutcome {
            service_root: PathBuf::from("/tmp/desky-services"),
            services: vec![
                MaterializedServiceRecord {
                    name: "ollama".to_string(),
                    source: "dependency:ollama".to_string(),
                    lifecycle: "managed".to_string(),
                    depends_on: Vec::new(),
                    healthcheck_url: None,
                    helper_command: Some("ollama serve".to_string()),
                    pid: None,
                    log_path: Some("/tmp/desky-services/ollama/service.log".to_string()),
                    status: "healthy".to_string(),
                    materialized_at: "2026-04-02T00:00:00+00:00".to_string(),
                    last_started_at: None,
                    last_stopped_at: None,
                    checked_at: "2026-04-02T00:00:00+00:00".to_string(),
                },
                MaterializedServiceRecord {
                    name: "opencode".to_string(),
                    source: "dependency:opencode".to_string(),
                    lifecycle: "on-demand".to_string(),
                    depends_on: vec!["ollama".to_string()],
                    healthcheck_url: None,
                    helper_command: Some("opencode serve".to_string()),
                    pid: None,
                    log_path: Some("/tmp/desky-services/opencode/service.log".to_string()),
                    status: "healthy".to_string(),
                    materialized_at: "2026-04-02T00:00:00+00:00".to_string(),
                    last_started_at: None,
                    last_stopped_at: None,
                    checked_at: "2026-04-02T00:00:00+00:00".to_string(),
                },
            ],
        };

        let state = build_install_bootstrap_state(&environment, &materialized, true, false);
        assert_eq!(
            state.materialization.bootstrap_phase,
            "environment_materialized"
        );
        assert!(state.materialization.opencode_installed);
        assert_eq!(state.materialization.ollama_mode, "managed");
        assert_eq!(state.health.overall, "healthy");
    }

    #[test]
    #[serial_test::serial]
    fn load_state_marks_missing_service_root_as_unmaterialized() {
        let _guard = test_env_lock().lock().expect("lock env");
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("bootstrap-state.json");
        let _state_guard = EnvVarGuard::set(
            "DESKY_BOOTSTRAP_STATE_PATH",
            Some(state_path.to_string_lossy().as_ref()),
        );

        let state = sample_state();
        write_state_to_path(&state_path, &state).expect("write sample state");

        let loaded = load_state_from_path(&state_path).expect("load state");
        assert_eq!(
            loaded.health.last_error.as_deref(),
            Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR)
        );
        assert_eq!(
            loaded.health.services.get("ollama").map(String::as_str),
            Some(UNMATERIALIZED_SERVICE_STATUS)
        );
        assert_eq!(
            loaded.health.services.get("opencode").map(String::as_str),
            Some(UNMATERIALIZED_SERVICE_STATUS)
        );
    }

    #[test]
    #[serial_test::serial]
    fn bootstrap_finalize_materializes_missing_service_root() {
        let _guard = test_env_lock().lock().expect("lock env");
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("bootstrap-state.json");
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        write_fake_binary(&bin_dir, "ollama");
        write_fake_binary(&bin_dir, "opencode");
        let _state_guard = EnvVarGuard::set(
            "DESKY_BOOTSTRAP_STATE_PATH",
            Some(state_path.to_string_lossy().as_ref()),
        );
        let _path_guard = EnvVarGuard::set("PATH", Some(bin_dir.to_string_lossy().as_ref()));

        bootstrap(
            ATO_DESKTOP_PACKAGE_ID,
            true,
            Some("/Users/test/Workspace"),
            Some(ModelTierArg::Balanced),
            Some(PrivacyModeArg::Strict),
            true,
        )
        .expect("bootstrap finalize");

        let service_root = managed_service_root(ATO_DESKTOP_PACKAGE_ID).expect("service root");
        assert!(service_root.join("ollama").join("service.json").exists());
        assert!(service_root.join("opencode").join("run.sh").exists());

        let raw = fs::read_to_string(&state_path).expect("read written state");
        let state: StoredBootstrapState = serde_json::from_str(&raw).expect("parse written state");
        assert_eq!(
            state.personalization.workspace_path.as_deref(),
            Some("/Users/test/Workspace")
        );
        assert_ne!(
            state.health.last_error.as_deref(),
            Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR)
        );
    }

    #[test]
    #[serial_test::serial]
    fn repair_restart_services_recreates_missing_service_root() {
        let _guard = test_env_lock().lock().expect("lock env");
        let temp = tempfile::tempdir().expect("tempdir");
        let state_path = temp.path().join("bootstrap-state.json");
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        write_fake_binary(&bin_dir, "ollama");
        write_fake_binary(&bin_dir, "opencode");
        let _state_guard = EnvVarGuard::set(
            "DESKY_BOOTSTRAP_STATE_PATH",
            Some(state_path.to_string_lossy().as_ref()),
        );
        let _path_guard = EnvVarGuard::set("PATH", Some(bin_dir.to_string_lossy().as_ref()));

        let mut state = sample_state();
        state.health.last_error = Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR.to_string());
        write_state_to_path(&state_path, &state).expect("write broken state");

        repair(
            ATO_DESKTOP_PACKAGE_ID,
            RepairActionArg::RestartServices,
            true,
        )
        .expect("repair");

        let service_root = managed_service_root(ATO_DESKTOP_PACKAGE_ID).expect("service root");
        assert!(service_root.join("ollama").join("service.json").exists());
        assert!(service_root.join("opencode").join("run.sh").exists());

        let repaired = load_state_from_path(&state_path).expect("load repaired state");
        assert_ne!(
            repaired.health.last_error.as_deref(),
            Some(MANAGED_ENVIRONMENT_NOT_MATERIALIZED_ERROR)
        );
    }
}
