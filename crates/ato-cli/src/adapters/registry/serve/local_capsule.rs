use super::*;

pub(super) async fn handle_run_local_capsule(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<RunCapsuleRequest>,
) -> impl IntoResponse {
    if !request.confirmed {
        return json_error(
            StatusCode::BAD_REQUEST,
            "confirmation_required",
            "confirmed=true is required",
        );
    }
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    if request.port.is_some_and(|port| port == 0) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_port",
            "port must be between 1 and 65535",
        );
    }

    let requested_target = request
        .target
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let requested_env_overrides = request
        .env
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| {
            let trimmed = key.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some((trimmed.to_string(), value))
            }
        })
        .collect::<HashMap<_, _>>();

    let _guard = state.lock.lock().await;
    let index = match load_index(&state.data_dir) {
        Ok(index) => index,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "index_read_failed",
                &err.to_string(),
            );
        }
    };
    let Some(capsule) = index
        .capsules
        .iter()
        .find(|capsule| capsule.publisher == publisher && capsule.slug == slug)
    else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    };

    let local_artifact = resolve_run_artifact_path(&state.data_dir, capsule);
    let saved_runtime_config = load_runtime_config(&state.data_dir)
        .ok()
        .and_then(|cfg| get_runtime_config_entry(&cfg, &publisher, &slug).cloned());

    let mut effective_target = requested_target.clone().or_else(|| {
        saved_runtime_config
            .as_ref()
            .and_then(|cfg| normalize_optional_string(cfg.selected_target.clone()))
    });
    let mut effective_port = request.port;
    let mut effective_permission_mode = request.permission_mode;
    let mut env_overrides = HashMap::new();

    if let Some(saved) = saved_runtime_config.as_ref() {
        let saved_target_config = effective_target
            .as_deref()
            .and_then(|label| saved.targets.get(label))
            .or_else(|| {
                saved
                    .selected_target
                    .as_deref()
                    .and_then(|label| saved.targets.get(label))
            });
        if let Some(target_config) = saved_target_config {
            if effective_port.is_none() {
                effective_port = target_config.port;
            }
            if effective_permission_mode.is_none() {
                effective_permission_mode = target_config.permission_mode;
            }
            for (key, value) in &target_config.env {
                let normalized = key.trim();
                if !normalized.is_empty() {
                    env_overrides.insert(normalized.to_string(), value.clone());
                }
            }
            if effective_target.is_none() {
                effective_target = saved
                    .selected_target
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string);
            }
        }
    }
    env_overrides.extend(requested_env_overrides);

    if !env_overrides.contains_key("ATO_CONTROL_PLANE_PORT") {
        if let Some(port) = allocate_loopback_port() {
            env_overrides.insert("ATO_CONTROL_PLANE_PORT".to_string(), port.to_string());
        }
    }
    drop(_guard);
    let Some(local_artifact) = local_artifact else {
        return json_error(
            StatusCode::NOT_FOUND,
            "artifact_not_found",
            "artifact is missing in local registry storage",
        );
    };
    if !local_artifact.exists() {
        return json_error(
            StatusCode::NOT_FOUND,
            "artifact_not_found",
            "resolved artifact is missing in local registry storage",
        );
    }

    let scoped_id = format!("{}/{}", publisher, slug);
    let request_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let registry_url =
        normalize_registry_base_url_for_local_run(&request_base_url, &state.listen_url);
    let ato_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_error",
                &format!("failed to resolve ato binary path: {}", err),
            );
        }
    };
    let run_target = local_artifact
        .canonicalize()
        .unwrap_or_else(|_| local_artifact.clone());
    let mut consent_manifest_tmpdir = None;
    let consent_manifest_path = if run_target
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("capsule"))
    {
        let bytes = match std::fs::read(&run_target) {
            Ok(bytes) => bytes,
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "run_plan_invalid",
                    &format!(
                        "failed to read local artifact for consent planning: {}",
                        err
                    ),
                );
            }
        };
        let manifest_raw = match extract_manifest_from_capsule(&bytes) {
            Ok(raw) => raw,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "run_plan_invalid",
                    &format!("failed to extract capsule.toml from artifact: {}", err),
                );
            }
        };
        let temp_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "run_plan_invalid",
                    &format!("failed to create consent planning workspace: {}", err),
                );
            }
        };
        let manifest_path = temp_dir.path().join("capsule.toml");
        if let Err(err) = std::fs::write(&manifest_path, manifest_raw) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_plan_invalid",
                &format!("failed to prepare consent planning manifest: {}", err),
            );
        }
        consent_manifest_tmpdir = Some(temp_dir);
        manifest_path
    } else {
        run_target.clone()
    };
    let compiled = match capsule_core::execution_plan::derive::compile_execution_plan(
        &consent_manifest_path,
        capsule_core::router::ExecutionProfile::Dev,
        effective_target.as_deref(),
    ) {
        Ok(compiled) => Some(compiled),
        Err(err) => {
            let manifest_text = std::fs::read_to_string(&consent_manifest_path).unwrap_or_default();
            if !manifest_is_web_static(&manifest_text) {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "run_plan_invalid",
                    &format!("failed to prepare execution plan: {}", err),
                );
            }
            None
        }
    };
    let _ = consent_manifest_tmpdir.as_ref();
    if let Some(compiled) = compiled {
        if let Err(err) = crate::consent_store::seed_consent(&compiled.execution_plan) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_consent_seed_failed",
                &format!("failed to seed execution consent: {}", err),
            );
        }
    }
    let mut cmd = std::process::Command::new(ato_path);
    cmd.arg("run")
        .arg(&run_target)
        .arg("--registry")
        .arg(&registry_url)
        .arg("--yes")
        .env("ATO_UI_SCOPED_ID", &scoped_id)
        .stdin(Stdio::null());
    if let Some(target) = effective_target.as_deref() {
        cmd.arg("--target").arg(target);
    }
    if let Some(port) = effective_port {
        cmd.env("ATO_UI_OVERRIDE_PORT", port.to_string());
    }
    match effective_permission_mode {
        Some(RunPermissionMode::Sandbox) => {
            cmd.arg("--sandbox");
        }
        Some(RunPermissionMode::Dangerous) => {
            cmd.arg("--dangerously-skip-permissions")
                .env("CAPSULE_ALLOW_UNSAFE", "1");
        }
        None => {}
    }
    if !env_overrides.is_empty() {
        let env_json = match serde_json::to_string(&env_overrides) {
            Ok(value) => value,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_env",
                    &format!("failed to serialize env overrides: {}", err),
                );
            }
        };
        cmd.env("ATO_UI_OVERRIDE_ENV_JSON", env_json);
    }

    let now = Utc::now();
    let nonce = now
        .timestamp_nanos_opt()
        .unwrap_or_else(|| now.timestamp_millis() * 1_000_000);
    let process_id = format!("capsule-{}-{}", nonce, std::process::id());
    let log_path = process_log_path(&process_id);
    if let Some(parent) = log_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to prepare log directory: {}", err),
            );
        }
    }
    let log_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => file,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to open process log file: {}", err),
            );
        }
    };
    let log_file_err = match log_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to clone process log handle: {}", err),
            );
        }
    };
    cmd.stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));

    #[cfg(unix)]
    unsafe {
        // Isolate spawned runtime into its own process group so stop can terminate the full tree.
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to spawn `ato run`: {}", err),
            );
        }
    };

    let pid = child.id() as i32;
    let process_info = ProcessInfo {
        id: process_id.clone(),
        name: slug.clone(),
        pid,
        workload_pid: None,
        status: ProcessStatus::Running,
        runtime: "ato-run".to_string(),
        start_time: std::time::SystemTime::now(),
        manifest_path: Some(run_target.clone()),
        scoped_id: Some(scoped_id.clone()),
        target_label: effective_target.clone(),
        requested_port: effective_port,
        log_path: Some(process_log_path(&process_id)),
        ready_at: Some(std::time::SystemTime::now()),
        last_event: Some("spawned".to_string()),
        last_error: None,
        exit_code: None,
    };
    let process_manager = match ProcessManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            let _ = child.kill();
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to initialize process manager: {}", err),
            );
        }
    };
    if let Err(err) = process_manager.write_pid(&process_info) {
        let _ = child.kill();
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run_spawn_failed",
            &format!("failed to persist process record: {}", err),
        );
    }
    let registered_service_binding_ids =
        match binding::sync_service_bindings_for_process(&process_id) {
            Ok(records) => records
                .into_iter()
                .map(|record| record.binding_id)
                .collect(),
            Err(err) => {
                let _ = child.kill();
                let _ = process_manager.delete_pid(&process_id);
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "service_binding_register_failed",
                    &err.to_string(),
                );
            }
        };
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    (
        StatusCode::ACCEPTED,
        Json(RunCapsuleResponse {
            accepted: true,
            scoped_id,
            requested_target: effective_target,
            requested_port: effective_port,
            registered_service_binding_ids,
        }),
    )
        .into_response()
}

fn manifest_is_web_static(manifest_text: &str) -> bool {
    let Ok(value) = toml::from_str::<toml::Value>(manifest_text) else {
        return false;
    };
    let runtime = value
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let driver = value
        .get("driver")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    if runtime.as_deref() == Some("web/static")
        || (runtime.as_deref() == Some("web") && driver.as_deref() == Some("static"))
    {
        return true;
    }
    let target_label = value
        .get("default_target")
        .and_then(toml::Value::as_str)
        .unwrap_or("app");
    let Some(target) = value
        .get("targets")
        .and_then(toml::Value::as_table)
        .and_then(|targets| targets.get(target_label))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    let runtime = target
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let driver = target
        .get("driver")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    runtime.as_deref() == Some("web/static")
        || (runtime.as_deref() == Some("web") && driver.as_deref() == Some("static"))
}

pub(super) async fn handle_delete_local_capsule(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    Query(query): Query<DeleteCapsuleQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    if !query.confirmed.unwrap_or(false) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "confirmation_required",
            "confirmed=true is required",
        );
    }
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    let delete_version = query
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if let Some(version) = delete_version.as_deref() {
        if let Err(err) = validate_version(version) {
            return json_error(StatusCode::BAD_REQUEST, "invalid_version", &err.to_string());
        }
    }

    let scoped_id = format!("{}/{}", publisher, slug);
    let process_manager = match ProcessManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_manager_error",
                &err.to_string(),
            )
        }
    };
    let processes = match process_manager.list_processes() {
        Ok(processes) => processes,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_list_failed",
                &err.to_string(),
            )
        }
    };
    if processes.iter().any(|process| {
        process.status.is_active() && process.scoped_id.as_deref() == Some(scoped_id.as_str())
    }) {
        return json_error(
            StatusCode::CONFLICT,
            "capsule_running",
            "capsule is running; stop active process before delete",
        );
    }
    let inactive_processes = processes
        .into_iter()
        .filter(|process| process.scoped_id.as_deref() == Some(scoped_id.as_str()))
        .collect::<Vec<_>>();

    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            );
        }
    };
    let now = Utc::now().to_rfc3339();
    let outcome =
        match store.delete_registry_capsule(&publisher, &slug, delete_version.as_deref(), &now) {
            Ok(crate::registry::store::RegistryDeleteOutcome::CapsuleNotFound) => {
                return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
            }
            Ok(crate::registry::store::RegistryDeleteOutcome::VersionNotFound(version)) => {
                return json_error(
                    StatusCode::NOT_FOUND,
                    "version_not_found",
                    &format!("Version '{}' not found", version),
                );
            }
            Ok(crate::registry::store::RegistryDeleteOutcome::Deleted(outcome)) => outcome,
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "delete_failed",
                    &err.to_string(),
                );
            }
        };

    let mut removed_service_binding_ids = Vec::new();
    if outcome.removed_capsule {
        if let Err(err) = store.delete_store_metadata(&scoped_id) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_metadata_delete_failed",
                &err.to_string(),
            );
        }
        let mut runtime_config = load_runtime_config(&state.data_dir).unwrap_or_default();
        runtime_config
            .entries
            .remove(&runtime_config_key(&publisher, &slug));
        if let Err(err) = write_runtime_config(&state.data_dir, &runtime_config) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_config_write_failed",
                &err.to_string(),
            );
        }
        for process in &inactive_processes {
            match binding::cleanup_service_bindings_for_process_info(process) {
                Ok(records) => removed_service_binding_ids
                    .extend(records.into_iter().map(|record| record.binding_id)),
                Err(err) => {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "service_binding_cleanup_failed",
                        &err.to_string(),
                    );
                }
            }
            if let Err(err) = process_manager.delete_pid(&process.id) {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "process_cleanup_failed",
                    &err.to_string(),
                );
            }
        }
    }

    cleanup_removed_artifacts(
        &state.data_dir,
        &publisher,
        &slug,
        &outcome.removed_releases,
    );
    (
        StatusCode::OK,
        Json(DeleteCapsuleResponse {
            deleted: true,
            scoped_id,
            removed_capsule: outcome.removed_capsule,
            removed_versions: outcome.removed_releases.len(),
            removed_version: outcome.removed_version,
            removed_service_binding_ids,
        }),
    )
        .into_response()
}

pub(super) fn process_log_path(id: &str) -> PathBuf {
    let home = capsule_core::common::paths::home_dir_or_workspace_tmp();
    home.join(".ato").join("logs").join(format!("{id}.log"))
}

pub(super) fn read_process_log_lines(path: &Path, tail: usize) -> Vec<String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let all_lines = reader.lines().map_while(Result::ok).collect::<Vec<_>>();
    if all_lines.len() <= tail {
        return all_lines;
    }
    all_lines[all_lines.len() - tail..].to_vec()
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct DeleteCapsuleResult {
    pub(super) removed_capsule: bool,
    pub(super) removed_version: Option<String>,
}

#[cfg(test)]
#[derive(Debug)]
pub(super) enum DeleteCapsuleOutcome {
    CapsuleNotFound,
    VersionNotFound(String),
    Deleted(DeleteCapsuleResult),
}

#[cfg(test)]
pub(super) fn delete_capsule_from_index(
    index: &mut RegistryIndex,
    publisher: &str,
    slug: &str,
    version: Option<&str>,
    now: &str,
) -> DeleteCapsuleOutcome {
    let Some(capsule_pos) = index
        .capsules
        .iter()
        .position(|capsule| capsule.publisher == publisher && capsule.slug == slug)
    else {
        return DeleteCapsuleOutcome::CapsuleNotFound;
    };

    if let Some(version) = version {
        let capsule = &mut index.capsules[capsule_pos];
        let Some(release_pos) = capsule
            .releases
            .iter()
            .position(|release| release.version == version)
        else {
            return DeleteCapsuleOutcome::VersionNotFound(version.to_string());
        };

        let removed = capsule.releases.remove(release_pos);
        if capsule.releases.is_empty() {
            index.capsules.remove(capsule_pos);
            return DeleteCapsuleOutcome::Deleted(DeleteCapsuleResult {
                removed_capsule: true,
                removed_version: Some(removed.version.clone()),
            });
        }

        if capsule.latest_version == removed.version {
            if let Some(last) = capsule.releases.last() {
                capsule.latest_version = last.version.clone();
            }
        }
        capsule.updated_at = now.to_string();
        return DeleteCapsuleOutcome::Deleted(DeleteCapsuleResult {
            removed_capsule: false,
            removed_version: Some(removed.version.clone()),
        });
    }

    index.capsules.remove(capsule_pos);
    DeleteCapsuleOutcome::Deleted(DeleteCapsuleResult {
        removed_capsule: true,
        removed_version: None,
    })
}

pub(super) fn cleanup_removed_artifacts(
    data_dir: &Path,
    publisher: &str,
    slug: &str,
    releases: &[crate::registry::store::RegistryReleaseRecord],
) {
    for release in releases {
        let path = artifact_path(
            data_dir,
            publisher,
            slug,
            &release.version,
            &release.file_name,
        );
        if !path.exists() {
            continue;
        }
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!(
                "local registry failed to remove artifact file path={} error={}",
                path.display(),
                err
            );
        }
    }
}

#[cfg(test)]
pub(super) fn truncate_for_error(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_string();
    }
    let head = message.chars().take(max_chars).collect::<String>();
    format!("{}...", head)
}
