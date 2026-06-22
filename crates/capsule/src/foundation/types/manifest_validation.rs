//! Manifest validation rules and state/service helper logic.
#![allow(clippy::items_after_test_module)]

use super::*;

impl CapsuleManifest {
    /// Validate the manifest
    pub fn validate(&self) -> Result<(), Vec<super::ValidationError>> {
        self.validate_for_mode(ValidationMode::Strict)
    }

    pub fn validate_for_mode(
        &self,
        mode: ValidationMode,
    ) -> Result<(), Vec<super::ValidationError>> {
        let mut errors = Vec::new();

        if self
            .state_owner_scope
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push(ValidationError::InvalidState(
                "state_owner_scope".to_string(),
                "state_owner_scope cannot be empty".to_string(),
            ));
        }

        if self
            .service_binding_scope
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push(ValidationError::InvalidService(
                "service_binding_scope".to_string(),
                "service_binding_scope cannot be empty".to_string(),
            ));
        }

        if let Err(message) = self.validate_host_capabilities() {
            errors.push(ValidationError::InvalidHostCapability(message));
        }

        // Reject capabilities that have no production host execution path. These
        // are schema-only (currently `open-editor`, see #468): declaring one
        // would otherwise pass validation and convert into a grant that nothing
        // ever executes — an inert capability. Fail closed in every mode rather
        // than silently no-op. Remove this once host execution + consent exist.
        for spec in &self.host_capabilities {
            if !spec.name.is_host_supported() {
                errors.push(ValidationError::UnsupportedHostCapability {
                    name: spec.name.to_string(),
                });
            }
        }

        let schema_is_v03 = self.schema_version.trim() == "0.3";

        if !is_supported_schema_version(&self.schema_version) {
            errors.push(ValidationError::InvalidSchemaVersion(
                self.schema_version.clone(),
            ));
        }

        if !is_kebab_case(&self.name) {
            errors.push(ValidationError::InvalidName(self.name.clone()));
        }

        if !(3..=64).contains(&self.name.len()) {
            errors.push(ValidationError::InvalidName(self.name.clone()));
        }

        if !self.version.trim().is_empty() && !is_semver(&self.version) {
            errors.push(ValidationError::InvalidVersion(self.version.clone()));
        }

        if let Some(pack) = &self.pack {
            if pack.include.iter().any(|pattern| pattern.trim().is_empty()) {
                errors.push(ValidationError::InvalidTarget(
                    "pack.include must not contain empty patterns".to_string(),
                ));
            }
            if pack.exclude.iter().any(|pattern| pattern.trim().is_empty()) {
                errors.push(ValidationError::InvalidTarget(
                    "pack.exclude must not contain empty patterns".to_string(),
                ));
            }
        }

        if let Some(v) = &self.requirements.vram_min
            && parse_memory_string(v).is_err()
        {
            errors.push(ValidationError::InvalidMemoryString {
                field: "requirements.vram_min",
                value: v.clone(),
            });
        }
        if let Some(v) = &self.requirements.vram_recommended
            && parse_memory_string(v).is_err()
        {
            errors.push(ValidationError::InvalidMemoryString {
                field: "requirements.vram_recommended",
                value: v.clone(),
            });
        }
        if let Some(v) = &self.requirements.disk
            && parse_memory_string(v).is_err()
        {
            errors.push(ValidationError::InvalidMemoryString {
                field: "requirements.disk",
                value: v.clone(),
            });
        }

        if self.capsule_type == CapsuleType::Inference && self.capabilities.is_none() {
            errors.push(ValidationError::MissingCapabilities);
        }

        if self.capsule_type == CapsuleType::Inference && self.model.is_none() {
            errors.push(ValidationError::MissingModelConfig);
        }

        let is_v03_library = schema_is_v03 && self.capsule_type == CapsuleType::Library;
        let is_tool = self.capsule_type == CapsuleType::Tool;
        let named_targets = self
            .targets
            .as_ref()
            .map(|t| t.named_targets())
            .cloned()
            .unwrap_or_default();
        if self.capsule_type == CapsuleType::Job
            && self
                .targets
                .as_ref()
                .and_then(|targets| targets.port)
                .is_some()
        {
            errors.push(ValidationError::InvalidTarget(
                "capsule type 'job' must not declare top-level port".to_string(),
            ));
        }

        if is_tool {
            if self.platforms.is_empty() {
                errors.push(ValidationError::ToolMissingPlatforms);
            }
            for (platform_key, artifact) in &self.platforms {
                if !is_valid_platform_key(platform_key) {
                    errors.push(ValidationError::ToolInvalidPlatformKey(
                        platform_key.clone(),
                    ));
                }
                if artifact.artifact.trim().is_empty() {
                    errors.push(ValidationError::ToolMissingArtifact(platform_key.clone()));
                }
                if !is_valid_sha256(&artifact.sha256) {
                    errors.push(ValidationError::ToolInvalidArtifactSha256(
                        platform_key.clone(),
                    ));
                }
            }
            if let Some(exports) = &self.exports {
                for (alias, path) in &exports.binaries {
                    if !is_valid_relative_export_path(path) {
                        errors.push(ValidationError::ToolExportPathInvalid {
                            kind: "binaries",
                            alias: alias.clone(),
                            path: path.clone(),
                        });
                    }
                }
                for (alias, path) in &exports.paths {
                    if !is_valid_relative_export_path(path) {
                        errors.push(ValidationError::ToolExportPathInvalid {
                            kind: "paths",
                            alias: alias.clone(),
                            path: path.clone(),
                        });
                    }
                }
            }
            if self.services.as_ref().is_some_and(|s| !s.is_empty()) {
                errors.push(ValidationError::ToolMustNotDeclareServices);
            }
            if !self.dependencies.is_empty() {
                errors.push(ValidationError::ToolMustNotDeclareServiceDependencies);
            }
        } else if !self.platforms.is_empty() {
            errors.push(ValidationError::PlatformsRequiresToolType);
        }

        for (alias, spec) in &self.tool_dependencies {
            for (export, env_name) in &spec.bind_env {
                if !is_valid_env_var_name(env_name) {
                    errors.push(ValidationError::ToolDependencyInvalidEnvVar {
                        alias: alias.clone(),
                        export: export.clone(),
                        env_name: env_name.clone(),
                    });
                }
            }
        }

        if !is_v03_library && !is_tool && self.default_target.trim().is_empty() {
            errors.push(ValidationError::MissingDefaultTarget);
        }
        if !is_v03_library && !is_tool && named_targets.is_empty() {
            errors.push(ValidationError::MissingTargets);
        } else if !self.default_target.trim().is_empty()
            && !named_targets.contains_key(self.default_target.trim())
        {
            errors.push(ValidationError::DefaultTargetNotFound(
                self.default_target.clone(),
            ));
        }

        if let Some(exports) = self.exports.as_ref() {
            for (export_name, export) in &exports.cli {
                if !is_kebab_case(export_name) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} must be kebab-case",
                        export_name
                    )));
                }

                let kind = export.kind.trim().to_ascii_lowercase();
                if kind != "python-tool" {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} kind '{}' is not supported; expected 'python-tool'",
                        export_name, export.kind
                    )));
                    continue;
                }

                let target_label = export.target.trim();
                if target_label.is_empty() {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} target is required",
                        export_name
                    )));
                    continue;
                }

                let Some(target) = named_targets.get(target_label) else {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} references missing target '{}'",
                        export_name, target_label
                    )));
                    continue;
                };

                let (runtime, runtime_driver) = split_runtime_driver(&target.runtime);
                if runtime.as_deref() != Some("source") {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} must reference a runtime=source target",
                        export_name
                    )));
                }

                let driver = runtime_driver.or_else(|| infer_source_driver(target));
                if driver.as_deref() != Some("python") {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} must reference a source/python target",
                        export_name
                    )));
                }

                if export.args.iter().any(|arg| arg.trim().is_empty()) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "exports.cli.{} args must not contain empty values",
                        export_name
                    )));
                }
            }
        }

        let has_services = self
            .services
            .as_ref()
            .map(|services| !services.is_empty())
            .unwrap_or(false);
        let has_target_services = self
            .services
            .as_ref()
            .map(|services| {
                services.values().any(|service| {
                    service
                        .target
                        .as_ref()
                        .map(|target| !target.trim().is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        let mut requires_web_services_validation = false;

        for (label, target) in &named_targets {
            let runtime_raw = target.runtime.trim().to_ascii_lowercase();
            // Split compound selectors (e.g. "web/node" → base="web", compound_driver=Some("node"))
            let (runtime, compound_driver) =
                if let Some((base, suffix)) = runtime_raw.split_once('/') {
                    (
                        base.to_string(),
                        if suffix.is_empty() {
                            None
                        } else {
                            Some(suffix.to_string())
                        },
                    )
                } else {
                    (runtime_raw, None)
                };
            let entrypoint = target.entrypoint.trim();
            let has_run_command = target
                .run_command
                .as_ref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
            let target_is_library = schema_is_v03
                && target
                    .package_type
                    .as_deref()
                    .map(|value| value.eq_ignore_ascii_case("library"))
                    .unwrap_or(is_v03_library);

            if target_is_library {
                if has_run_command
                    || !entrypoint.is_empty()
                    || target
                        .image
                        .as_deref()
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false)
                    || !target.cmd.is_empty()
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "library target '{}' must not define a run command",
                        label
                    )));
                }
                continue;
            }

            if label.trim().is_empty()
                || runtime.is_empty()
                || !matches!(
                    runtime.as_str(),
                    "source" | "wasm" | "oci" | "web" | "native-inference"
                )
            {
                errors.push(ValidationError::InvalidTarget(label.clone()));
                continue;
            }

            if runtime == "native-inference" {
                let nonempty = |v: &Option<String>| {
                    v.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false)
                };
                let has_engine_path = nonempty(&target.engine_path);
                // Engine is either a local `engine_path` OR a managed engine
                // (`engine` + `engine_version`, e.g. llama.cpp build tag).
                let has_managed_engine =
                    nonempty(&target.engine) && nonempty(&target.engine_version);
                if !has_engine_path && !has_managed_engine {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{label}': runtime=native-inference requires either `engine_path` \
                         (a local engine binary) or `engine` + `engine_version` (managed)"
                    )));
                }
                // A managed `engine_version` flows into download URLs + cache
                // paths, so it must be path/URL-safe (no traversal/separators).
                if let Some(version) = target.engine_version.as_deref()
                    && !version.trim().is_empty()
                    && !crate::foundation::types::manifest::is_safe_engine_version(version)
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{label}': `engine_version` must be a build tag / version id \
                         (alphanumeric, `.`/`_`/`-`; no path separators or `..`)"
                    )));
                }
                // Model is either a local `model` path OR a managed model
                // (`model_url` + `model_sha256`, content-addressed cache).
                let has_local_model = nonempty(&target.model);
                let has_managed_model =
                    nonempty(&target.model_url) && nonempty(&target.model_sha256);
                if !has_local_model && !has_managed_model {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{label}': runtime=native-inference requires either `model` \
                         (a local file) or `model_url` + `model_sha256` (managed)"
                    )));
                }
                if let Some(url) = target.model_url.as_deref()
                    && !url.trim().is_empty()
                    && !crate::foundation::types::manifest::is_safe_model_url(url)
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{label}': `model_url` must be a plain http(s):// URL"
                    )));
                }
                // The sha256 is the cache key + integrity check — it must be exact.
                if let Some(sha) = target.model_sha256.as_deref()
                    && !sha.trim().is_empty()
                    && crate::foundation::types::manifest::normalize_model_sha256(sha).is_none()
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{label}': `model_sha256` must be a 64-char hex SHA-256 \
                         (optionally `sha256:`-prefixed)"
                    )));
                }
                // A managed model needs its integrity hash.
                if nonempty(&target.model_url) && !nonempty(&target.model_sha256) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{label}': `model_url` requires `model_sha256`"
                    )));
                }
                continue;
            }

            if self.capsule_type == CapsuleType::Job {
                if target.port.is_some() {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}' declares port but capsule type 'job' must not expose ingress",
                        label
                    )));
                }

                if runtime == "web" {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}' uses runtime=web but capsule type 'job' must not expose ingress",
                        label
                    )));
                }
            }

            if runtime == "source" {
                // v0.3 collapses `runtime = "web/deno"` to `runtime = source`
                // + `driver = deno`. When that target also wires up
                // \[services\] (web-services mode), the per-target entrypoint
                // is optional — each service supplies its own.
                let driver_str = target
                    .driver
                    .as_deref()
                    .map(str::trim)
                    .map(str::to_ascii_lowercase);
                let is_web_services_mode_v03 =
                    driver_str.as_deref() == Some("deno") && has_services && target.port.is_some();
                if entrypoint.is_empty() && !has_run_command && !is_web_services_mode_v03 {
                    errors.push(ValidationError::InvalidTarget(label.clone()));
                    continue;
                }
                if is_web_services_mode_v03 {
                    requires_web_services_validation = true;
                }
                let effective_driver = split_runtime_driver(&target.runtime)
                    .1
                    .or_else(|| infer_source_driver(target));
                if !schema_is_v03
                    && matches!(
                        effective_driver.as_deref(),
                        Some("deno") | Some("node") | Some("python")
                    )
                    && target
                        .runtime_version
                        .as_ref()
                        .map(|v| v.trim().is_empty())
                        .unwrap_or(true)
                    && !matches!(mode, ValidationMode::Preview)
                {
                    errors.push(ValidationError::MissingRuntimeVersion(
                        label.clone(),
                        effective_driver.unwrap_or_else(|| "unknown".to_string()),
                    ));
                }
            }

            if runtime == "web" {
                if !target.public.is_empty() {
                    errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "public is no longer supported for runtime=web".to_string(),
                    ));
                }

                if target.port.is_none() && !matches!(mode, ValidationMode::Preview) {
                    errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "port is required for runtime=web".to_string(),
                    ));
                } else if target.port == Some(0) {
                    errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "port must be between 1 and 65535".to_string(),
                    ));
                }

                let mut normalized_driver: Option<String> = None;
                let effective_driver = target.driver.as_deref().or(compound_driver.as_deref());
                match effective_driver {
                    None => errors.push(ValidationError::InvalidWebTarget(
                        label.clone(),
                        "driver is required for runtime=web (static|node|deno|python)".to_string(),
                    )),
                    Some(driver) => {
                        let normalized = driver.trim().to_ascii_lowercase();
                        if matches!(normalized.as_str(), "browser_static" | "browser-static") {
                            errors.push(ValidationError::InvalidWebTarget(
                                label.clone(),
                                "driver 'browser_static' has been removed; use 'static'"
                                    .to_string(),
                            ));
                        } else if !matches!(
                            normalized.as_str(),
                            "static" | "node" | "deno" | "python"
                        ) {
                            errors.push(ValidationError::InvalidTargetDriver(
                                label.clone(),
                                driver.to_string(),
                            ));
                        } else {
                            normalized_driver = Some(normalized);
                        }
                    }
                }

                let web_services_mode =
                    matches!(normalized_driver.as_deref(), Some("deno")) && has_services;
                if web_services_mode {
                    requires_web_services_validation = true;
                    if std::path::Path::new(entrypoint)
                        .file_name()
                        .and_then(|v| v.to_str())
                        .map(|v| v.eq_ignore_ascii_case("ato-entry.ts"))
                        .unwrap_or(false)
                    {
                        errors.push(ValidationError::InvalidWebTarget(
                            label.clone(),
                            "entrypoint='ato-entry.ts' is deprecated. Define top-level [services] and remove ato-entry.ts orchestrator."
                                .to_string(),
                        ));
                    }
                } else {
                    if entrypoint.is_empty() && !has_run_command {
                        errors.push(ValidationError::InvalidTarget(label.clone()));
                        continue;
                    }
                    if matches!(
                        normalized_driver.as_deref(),
                        Some("node") | Some("deno") | Some("python")
                    ) && !has_run_command
                        && entrypoint.split_whitespace().count() > 1
                    {
                        errors.push(ValidationError::InvalidWebTarget(
                            label.clone(),
                            "entrypoint must be a script file path (shell command strings are not allowed)"
                                .to_string(),
                        ));
                    }
                }
                continue;
            }

            if runtime == "oci" {
                let image = target.image.as_deref().map(str::trim).unwrap_or("");
                // v0.3 stores the OCI image reference under `run_command`
                // (from `run = "ghcr.io/..."`). Treat that as equivalent to
                // an explicit `image` for validation purposes.
                if entrypoint.is_empty() && image.is_empty() && !has_run_command {
                    errors.push(ValidationError::InvalidTarget(label.clone()));
                    continue;
                }
            } else if entrypoint.is_empty() && !has_run_command && !requires_web_services_validation
            {
                errors.push(ValidationError::InvalidTarget(label.clone()));
                continue;
            }

            if let Some(probe) = target.readiness_probe.as_ref() {
                let has_http_get = probe
                    .http_get
                    .as_ref()
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                let has_tcp_connect = probe
                    .tcp_connect
                    .as_ref()
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false);
                let has_exec = probe.exec.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                // port is required for HTTP/TCP probes; exec probes do not need it.
                if (has_http_get || has_tcp_connect)
                    && !has_exec
                    && probe
                        .port
                        .as_deref()
                        .map(|s| s.trim().is_empty())
                        .unwrap_or(true)
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                            "target '{}': readiness_probe.port must be a non-empty placeholder name for http_get/tcp_connect probes",
                            label
                        )));
                }
                if !has_http_get && !has_tcp_connect && !has_exec {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe must define http_get, tcp_connect, or exec",
                        label
                    )));
                }
                if has_exec && probe.exec.as_ref().map(|v| v.is_empty()).unwrap_or(false) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe.exec must be a non-empty command list",
                        label
                    )));
                }
                if probe.timeout_seconds == 0 {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe.timeout_seconds must be > 0",
                        label
                    )));
                }
                if probe.interval_seconds == 0 {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe.interval_seconds must be > 0",
                        label
                    )));
                }
                if probe.initial_delay_seconds >= probe.timeout_seconds && probe.timeout_seconds > 0
                {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe.initial_delay_seconds ({}) must be less than timeout_seconds ({})",
                        label, probe.initial_delay_seconds, probe.timeout_seconds
                    )));
                }
            }

            for (key, contract) in &target.external_injection {
                if !is_valid_external_injection_key(key) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': external_injection key '{}' must be an uppercase shell-safe identifier",
                        label, key
                    )));
                }
                if !matches!(
                    contract.injection_type.as_str(),
                    "file" | "directory" | "string"
                ) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': external_injection.{} type '{}' is unsupported",
                        label, key, contract.injection_type
                    )));
                }
            }

            if let Some(driver) = target.driver.as_ref() {
                let normalized = driver.trim().to_ascii_lowercase();
                if !matches!(
                    normalized.as_str(),
                    "static" | "deno" | "node" | "python" | "wasmtime" | "native"
                ) {
                    errors.push(ValidationError::InvalidTargetDriver(
                        label.clone(),
                        driver.clone(),
                    ));
                    continue;
                }
                if normalized == "static" {
                    errors.push(ValidationError::InvalidTargetDriver(
                        label.clone(),
                        driver.clone(),
                    ));
                    continue;
                }
            }
        }

        if schema_is_v03 {
            let package_dependencies = named_targets
                .iter()
                .map(|(label, target)| (label.clone(), target.package_dependencies.clone()))
                .collect::<HashMap<_, _>>();

            for (label, dependencies) in &package_dependencies {
                for dependency in dependencies {
                    if dependency == label {
                        errors.push(ValidationError::InvalidTarget(format!(
                            "target '{}' must not depend on itself",
                            label
                        )));
                    } else if !named_targets.contains_key(dependency) {
                        errors.push(ValidationError::InvalidTarget(format!(
                            "target '{}' depends on unknown workspace package '{}'",
                            label, dependency
                        )));
                    }
                }

                let target = named_targets
                    .get(label)
                    .expect("package_dependencies keys must exist in named_targets");
                if target.outputs.iter().any(|value| value.trim().is_empty()) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': outputs must not contain empty patterns",
                        label
                    )));
                }
                if target.build_env.iter().any(|value| value.trim().is_empty()) {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': build_env must not contain empty variable names",
                        label
                    )));
                }
            }

            if let Err(err) = startup_order_from_dependencies(&package_dependencies) {
                errors.push(ValidationError::InvalidTarget(err.to_string()));
            }
        }

        validate_dependency_contracts(self, &named_targets, &mut errors);

        if has_target_services {
            let services = self.services.as_ref().cloned().unwrap_or_default();
            if services.is_empty() {
                errors.push(ValidationError::InvalidService(
                    "main".to_string(),
                    "top-level [services] must define at least one service for orchestration mode"
                        .to_string(),
                ));
            } else {
                if !services.contains_key("main") {
                    errors.push(ValidationError::InvalidService(
                        "main".to_string(),
                        "services.main is required for orchestration mode".to_string(),
                    ));
                }

                let mut dependencies = HashMap::new();
                let mut resolved_runtimes = HashMap::new();

                for (name, service) in &services {
                    let target_name = service.target.as_deref().map(str::trim).unwrap_or("");
                    let has_target = !target_name.is_empty();
                    let has_entrypoint = !service.entrypoint.trim().is_empty();

                    if has_target && has_entrypoint {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "target and entrypoint are mutually exclusive".to_string(),
                        ));
                    }

                    let effective_target = if has_target {
                        Some(target_name.to_string())
                    } else if name == "main" && !has_entrypoint {
                        Some(self.default_target.trim().to_string())
                    } else {
                        None
                    };

                    let target_label = match effective_target {
                        Some(target_label) => target_label,
                        None => {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "target is required for orchestration mode".to_string(),
                            ));
                            dependencies.insert(
                                name.to_string(),
                                service.depends_on.clone().unwrap_or_default(),
                            );
                            continue;
                        }
                    };

                    let Some(target) = self
                        .targets
                        .as_ref()
                        .and_then(|targets| targets.named_target(&target_label))
                    else {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            format!("target '{}' does not exist under [targets]", target_label),
                        ));
                        dependencies.insert(
                            name.to_string(),
                            service.depends_on.clone().unwrap_or_default(),
                        );
                        continue;
                    };

                    let runtime = target.runtime.trim().to_ascii_lowercase();
                    if runtime == "wasm" {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "runtime=wasm is not supported in orchestration mode".to_string(),
                        ));
                    }

                    if service
                        .network
                        .as_ref()
                        .map(|network| {
                            network.aliases.iter().any(|alias| alias.trim().is_empty())
                                || network
                                    .allow_from
                                    .iter()
                                    .any(|value| value.trim().is_empty())
                        })
                        .unwrap_or(false)
                    {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "network aliases and allow_from must not contain empty values"
                                .to_string(),
                        ));
                    }

                    if let Some(probe) = service.readiness_probe.as_ref() {
                        let has_http_get = probe
                            .http_get
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        let has_tcp_connect = probe
                            .tcp_connect
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        let has_exec_svc =
                            probe.exec.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                        // port is required for HTTP/TCP probes; exec probes do not need it.
                        if (has_http_get || has_tcp_connect)
                            && !has_exec_svc
                            && probe
                                .port
                                .as_deref()
                                .map(|s| s.trim().is_empty())
                                .unwrap_or(true)
                        {
                            errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    "readiness_probe.port must be a non-empty placeholder name for http_get/tcp_connect probes"
                                        .to_string(),
                                ));
                        }
                        if !has_http_get && !has_tcp_connect && !has_exec_svc {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe must define http_get, tcp_connect, or exec"
                                    .to_string(),
                            ));
                        }
                        if probe.timeout_seconds == 0 {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.timeout_seconds must be > 0".to_string(),
                            ));
                        }
                        if probe.interval_seconds == 0 {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.interval_seconds must be > 0".to_string(),
                            ));
                        }
                        if probe.initial_delay_seconds >= probe.timeout_seconds
                            && probe.timeout_seconds > 0
                        {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!(
                                    "readiness_probe.initial_delay_seconds ({}) must be less than timeout_seconds ({})",
                                    probe.initial_delay_seconds, probe.timeout_seconds
                                ),
                            ));
                        }
                    }

                    let deps = service.depends_on.clone().unwrap_or_default();
                    for dep in &deps {
                        if !services.contains_key(dep) {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!("depends_on references unknown service '{}'", dep),
                            ));
                        }
                    }

                    if let Some(network) = service.network.as_ref() {
                        for allowed in &network.allow_from {
                            if !services.contains_key(allowed) {
                                errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    format!("allow_from references unknown service '{}'", allowed),
                                ));
                            }
                        }
                    }

                    dependencies.insert(name.to_string(), deps);
                    resolved_runtimes.insert(name.to_string(), runtime);
                }

                for (name, service) in &services {
                    let Some(runtime) = resolved_runtimes.get(name) else {
                        continue;
                    };
                    for dep in service.depends_on.clone().unwrap_or_default() {
                        let Some(dep_runtime) = resolved_runtimes.get(&dep) else {
                            continue;
                        };
                        if runtime == "oci" && dep_runtime != "oci" {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!(
                                    "OCI service '{}' cannot depend on non-OCI service '{}'",
                                    name, dep
                                ),
                            ));
                        }
                        if let Some(network) =
                            services.get(&dep).and_then(|svc| svc.network.as_ref())
                            && !network.allow_from.is_empty()
                            && !network.allow_from.iter().any(|value| value == name)
                        {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!(
                                    "service '{}' is not allowed to connect to '{}'",
                                    name, dep
                                ),
                            ));
                        }
                    }
                }

                if let Err(err) = startup_order_from_dependencies(&dependencies) {
                    errors.push(ValidationError::InvalidService(
                        "services".to_string(),
                        err.to_string(),
                    ));
                }
            }
        } else if requires_web_services_validation {
            let services = self.services.as_ref().cloned().unwrap_or_default();
            if services.is_empty() {
                errors.push(ValidationError::InvalidService(
                    "main".to_string(),
                    "top-level [services] must define at least one service for web/deno services mode".to_string(),
                ));
            } else {
                if !services.contains_key("main") {
                    errors.push(ValidationError::InvalidService(
                        "main".to_string(),
                        "services.main is required for web/deno services mode".to_string(),
                    ));
                }

                for (name, service) in &services {
                    if service.entrypoint.trim().is_empty() {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "entrypoint is required".to_string(),
                        ));
                    }

                    if service
                        .expose
                        .as_ref()
                        .is_some_and(|ports| !ports.is_empty())
                    {
                        errors.push(ValidationError::InvalidService(
                            name.to_string(),
                            "expose is not supported yet in web/deno services mode".to_string(),
                        ));
                    }

                    if let Some(probe) = service.readiness_probe.as_ref() {
                        let has_http_get = probe
                            .http_get
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        let has_tcp_connect = probe
                            .tcp_connect
                            .as_ref()
                            .map(|v| !v.trim().is_empty())
                            .unwrap_or(false);
                        let has_exec_svc =
                            probe.exec.as_ref().map(|v| !v.is_empty()).unwrap_or(false);
                        // port is required for HTTP/TCP probes; exec probes do not need it.
                        if (has_http_get || has_tcp_connect)
                            && !has_exec_svc
                            && probe
                                .port
                                .as_deref()
                                .map(|s| s.trim().is_empty())
                                .unwrap_or(true)
                        {
                            errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    "readiness_probe.port must be a non-empty placeholder name for http_get/tcp_connect probes"
                                        .to_string(),
                                ));
                        }
                        if !has_http_get && !has_tcp_connect && !has_exec_svc {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe must define http_get, tcp_connect, or exec"
                                    .to_string(),
                            ));
                        }
                        if probe.timeout_seconds == 0 {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.timeout_seconds must be > 0".to_string(),
                            ));
                        }
                        if probe.interval_seconds == 0 {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.interval_seconds must be > 0".to_string(),
                            ));
                        }
                        if probe.initial_delay_seconds >= probe.timeout_seconds
                            && probe.timeout_seconds > 0
                        {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                format!(
                                    "readiness_probe.initial_delay_seconds ({}) must be less than timeout_seconds ({})",
                                    probe.initial_delay_seconds, probe.timeout_seconds
                                ),
                            ));
                        }
                    }
                }

                for (name, service) in &services {
                    if let Some(deps) = service.depends_on.as_ref() {
                        for dep in deps {
                            if !services.contains_key(dep) {
                                errors.push(ValidationError::InvalidService(
                                    name.to_string(),
                                    format!("depends_on references unknown service '{}'", dep),
                                ));
                            }
                        }
                    }
                }

                if let Err(cycle) = detect_service_cycle(&services) {
                    errors.push(ValidationError::InvalidService(
                        "services".to_string(),
                        format!("circular dependency detected: {}", cycle),
                    ));
                }
            }
        }

        let has_oci_target = self.targets.as_ref().is_some_and(|targets| {
            targets
                .named_targets()
                .values()
                .any(|t| t.runtime.eq_ignore_ascii_case("oci"))
                || targets.oci.is_some()
        });
        if !self.storage.volumes.is_empty() {
            if !has_oci_target {
                errors.push(ValidationError::StorageOnlyForDocker);
            }

            let mut names = std::collections::HashSet::new();
            for vol in &self.storage.volumes {
                if vol.name.trim().is_empty() {
                    errors.push(ValidationError::InvalidStorageVolume);
                    continue;
                }
                if !names.insert(vol.name.trim().to_string()) {
                    errors.push(ValidationError::InvalidStorageVolume);
                }
                let mp = vol.mount_path.trim();
                if mp.is_empty() || !mp.starts_with('/') || mp.contains("..") {
                    errors.push(ValidationError::InvalidStorageVolume);
                }
            }
        }

        if !self.state.is_empty() {
            if self
                .services
                .as_ref()
                .map(|services| {
                    services.is_empty()
                        || !services
                            .values()
                            .any(|service| !service.state_bindings.is_empty())
                })
                .unwrap_or(true)
            {
                errors.push(ValidationError::InvalidState(
                    "state".to_string(),
                    "services.*.state_bindings are required when [state] is declared".to_string(),
                ));
            }

            let mut shared_state_bindings = HashMap::new();
            for (state_name, requirement) in &self.state {
                let trimmed_name = state_name.trim();
                if trimmed_name.is_empty() || !is_kebab_case(trimmed_name) {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "state name must be kebab-case".to_string(),
                    ));
                }

                if requirement.purpose.trim().is_empty() {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "purpose is required".to_string(),
                    ));
                }
                if requirement
                    .producer
                    .as_deref()
                    .is_some_and(|producer| producer.trim().is_empty())
                {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "producer cannot be empty".to_string(),
                    ));
                }

                if requirement.kind != StateKind::Filesystem {
                    errors.push(ValidationError::InvalidState(
                        state_name.clone(),
                        "only kind=\"filesystem\" is supported in this PoC".to_string(),
                    ));
                }

                if requirement.durability == StateDurability::Persistent {
                    if requirement.attach != StateAttach::Explicit {
                        errors.push(ValidationError::InvalidState(
                            state_name.clone(),
                            "persistent state requires attach=\"explicit\"".to_string(),
                        ));
                    }
                    if requirement
                        .schema_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .is_none()
                    {
                        errors.push(ValidationError::InvalidState(
                            state_name.clone(),
                            "persistent state requires schema_id".to_string(),
                        ));
                    }
                }
            }

            if let Some(services) = self.services.as_ref() {
                for (service_name, service) in services {
                    if service.state_bindings.is_empty() {
                        continue;
                    }

                    let Some(target_label) = service
                        .target
                        .as_ref()
                        .map(|value| value.trim())
                        .filter(|value| !value.is_empty())
                    else {
                        errors.push(ValidationError::InvalidStateBinding(
                            service_name.clone(),
                            "state_bindings require target-based services".to_string(),
                        ));
                        continue;
                    };

                    if let Some(target) = named_targets.get(target_label)
                        && !target.runtime.eq_ignore_ascii_case("oci")
                    {
                        errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!(
                                    "state_bindings are only supported for OCI targets in this PoC (target '{}')",
                                    target_label
                                ),
                            ));
                    }

                    let mut bound_states = std::collections::HashSet::new();
                    let mut bound_targets = std::collections::HashSet::new();
                    for binding in &service.state_bindings {
                        let state_name = binding.state.trim();
                        let target = binding.target.trim();

                        if state_name.is_empty() {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                "binding.state is required".to_string(),
                            ));
                        } else {
                            if !bound_states.insert(state_name.to_string()) {
                                errors.push(ValidationError::InvalidStateBinding(
                                    service_name.clone(),
                                    format!("state '{}' is bound more than once", state_name),
                                ));
                            }

                            if let Some(previous_service) = shared_state_bindings
                                .insert(state_name.to_string(), service_name.clone())
                                && previous_service != *service_name
                            {
                                let state_requirement = self.state.get(state_name);
                                match state_requirement {
                                    Some(req) if req.sharing == StateSharing::SameCapsule => {
                                        if req
                                            .schema_id
                                            .as_deref()
                                            .map(str::trim)
                                            .filter(|v| !v.is_empty())
                                            .is_none()
                                        {
                                            errors.push(
                                                ValidationError::StateSharedRequiresSchemaId(
                                                    state_name.to_string(),
                                                ),
                                            );
                                        }
                                    }
                                    Some(_) => {
                                        errors.push(ValidationError::StateSharedRequiresPolicy {
                                            state: state_name.to_string(),
                                            first_service: previous_service,
                                            second_service: service_name.clone(),
                                        });
                                    }
                                    None => {
                                        errors.push(ValidationError::StateKeyUndeclared(
                                            state_name.to_string(),
                                        ));
                                    }
                                }
                            }

                            match self.state.get(state_name) {
                                Some(_) => {}
                                None => errors.push(ValidationError::InvalidStateBinding(
                                    service_name.clone(),
                                    format!("state '{}' is not declared under [state]", state_name),
                                )),
                            }
                        }

                        if !is_valid_mount_path(target) {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!("target '{}' must be an absolute path", binding.target),
                            ));
                        } else if !bound_targets.insert(target.to_string()) {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!("target '{}' is bound more than once", target),
                            ));
                        }
                    }
                }
            }
        }

        if let Some(ingress) = &self.ingress {
            validate_ingress(self, ingress, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn supports_current_platform(&self) -> bool {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::DarwinArm64)
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::DarwinX86_64)
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::LinuxAmd64)
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            self.requirements.platform.is_empty()
                || self.requirements.platform.contains(&Platform::LinuxArm64)
        }
        #[cfg(not(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64")
        )))]
        {
            false
        }
    }

    pub fn display_name(&self) -> &str {
        self.metadata.display_name.as_deref().unwrap_or(&self.name)
    }

    pub fn is_inference(&self) -> bool {
        self.capsule_type == CapsuleType::Inference
    }

    pub fn can_fallback_to_cloud(&self) -> bool {
        self.routing.fallback_to_cloud && self.routing.cloud_capsule.is_some()
    }

    pub fn ephemeral_state_source_path(&self, state_name: &str) -> Result<String, CapsuleError> {
        let state_name = state_name.trim();
        if !is_kebab_case(state_name) {
            return Err(CapsuleError::ValidationError(format!(
                "state '{}' must be kebab-case before deriving an ephemeral state path",
                state_name
            )));
        }

        Ok(format!(
            "{}/{}/{}",
            default_ephemeral_state_base().trim_end_matches('/'),
            self.name,
            state_name
        ))
    }

    pub fn state_source_path(
        &self,
        state_name: &str,
        requirement: &StateRequirement,
        overrides: Option<&HashMap<String, String>>,
    ) -> Result<String, CapsuleError> {
        if let Some(path) = overrides
            .and_then(|entries| entries.get(state_name.trim()))
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            return Ok(path.to_string());
        }

        match requirement.durability {
            StateDurability::Ephemeral => self.ephemeral_state_source_path(state_name),
            StateDurability::Persistent => Err(CapsuleError::ValidationError(format!(
                "state '{}' requires an explicit persistent binding before it can be attached",
                state_name.trim()
            ))),
        }
    }

    pub fn state_producer(&self, state_name: &str) -> Option<String> {
        self.state
            .get(state_name.trim())
            .and_then(|requirement| requirement.producer.as_deref())
            .map(str::trim)
            .filter(|producer| !producer.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let name = self.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
    }

    pub fn persistent_state_owner_scope(&self) -> Option<String> {
        self.state_owner_scope
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let name = self.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
    }

    pub fn host_service_binding_scope(&self) -> Option<String> {
        self.service_binding_scope
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                let name = self.name.trim();
                (!name.is_empty()).then(|| name.to_string())
            })
    }
}

fn validate_ingress(
    manifest: &CapsuleManifest,
    ingress: &IngressConfig,
    errors: &mut Vec<ValidationError>,
) {
    if let Err(err) = ingress.mode.validate_v1() {
        errors.push(ValidationError::InvalidTarget(err.to_string()));
    }

    let service_names: std::collections::HashSet<&str> = manifest
        .services
        .as_ref()
        .map(|s| s.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();

    let mut seen_aliases: HashMap<&str, &str> = HashMap::new();
    let mut root_route: Option<&str> = None;

    for (route_name, route) in &ingress.routes {
        if route.root && route.alias.is_some() {
            errors.push(ValidationError::InvalidTarget(
                IngressError::RootWithAlias {
                    route: route_name.clone(),
                }
                .to_string(),
            ));
        }

        if route.root {
            if let Some(previous) = root_route {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::MultipleRootRoutes {
                        route_a: previous.to_string(),
                        route_b: route_name.clone(),
                    }
                    .to_string(),
                ));
            }
            root_route = Some(route_name);
        }

        if !route.root {
            let effective_alias = route
                .alias
                .as_deref()
                .map(str::trim)
                .filter(|a| !a.is_empty());
            if effective_alias.is_none() && route.alias.is_some() {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::NonRootWithoutAlias {
                        route: route_name.clone(),
                    }
                    .to_string(),
                ));
            }
        }

        if let Some(alias) = route.alias.as_deref() {
            if !is_valid_ingress_alias(alias) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::InvalidAlias {
                        alias: alias.to_string(),
                        reason: "alias must be a URL-safe path segment (lowercase alphanumeric, hyphens, underscores; no '/', '..', '%2f', '%5c', or percent-encoded characters)"
                            .to_string(),
                    }
                    .to_string(),
                ));
            }
            if let Some(previous) = seen_aliases.get(alias) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::DuplicateAlias {
                        alias: alias.to_string(),
                        route_a: previous.to_string(),
                        route_b: route_name.clone(),
                    }
                    .to_string(),
                ));
            }
            seen_aliases.insert(alias, route_name);
        } else if !route.root {
            if !is_valid_ingress_alias(route_name) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::InvalidAlias {
                        alias: route_name.clone(),
                        reason: "route name used as fallback alias must be URL-safe (lowercase alphanumeric, hyphens, underscores; no '/', '..', '%2f', '%5c', or percent-encoded characters)"
                            .to_string(),
                    }
                    .to_string(),
                ));
            }
            if let Some(previous) = seen_aliases.get(route_name.as_str()) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::DuplicateAlias {
                        alias: route_name.clone(),
                        route_a: previous.to_string(),
                        route_b: route_name.clone(),
                    }
                    .to_string(),
                ));
            }
            seen_aliases.insert(route_name.as_str(), route_name);
        }

        if !service_names.contains(route.target.as_str()) {
            errors.push(ValidationError::InvalidTarget(
                IngressError::MissingService {
                    route: route_name.clone(),
                    target: route.target.clone(),
                }
                .to_string(),
            ));
        }

        if route.port == 0 {
            errors.push(ValidationError::InvalidTarget(
                IngressError::InvalidPort {
                    route: route_name.clone(),
                    port: route.port,
                }
                .to_string(),
            ));
        }

        if let Some(prefix) = route.upstream_path_prefix.as_deref() {
            if !route.strip_prefix {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::UpstreamPrefixWithoutStrip {
                        route: route_name.clone(),
                    }
                    .to_string(),
                ));
            }
            if !prefix.starts_with('/') {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::UpstreamPrefixMissingSlash {
                        route: route_name.clone(),
                        prefix: prefix.to_string(),
                    }
                    .to_string(),
                ));
            }
            if let Some(reason) = validate_upstream_prefix_segments(prefix) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::InvalidUpstreamPrefix {
                        route: route_name.clone(),
                        prefix: prefix.to_string(),
                        reason,
                    }
                    .to_string(),
                ));
            }
        }
    }

    for (target, env_vars) in &ingress.env_inject {
        if !service_names.contains(target.as_str()) {
            errors.push(ValidationError::InvalidTarget(
                IngressError::EnvInjectTargetMissing {
                    target: target.clone(),
                }
                .to_string(),
            ));
        }

        for (env_name, template) in env_vars {
            if !is_valid_env_var_name(env_name) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::InvalidEnvVarName {
                        name: env_name.clone(),
                    }
                    .to_string(),
                ));
            }

            for route_ref in extract_ingress_route_refs(template) {
                if !ingress.routes.contains_key(&route_ref) {
                    errors.push(ValidationError::InvalidTarget(
                        IngressError::EnvInjectMissingRoute {
                            target: target.clone(),
                            env_name: env_name.clone(),
                            route_name: route_ref,
                            template: template.clone(),
                        }
                        .to_string(),
                    ));
                }
            }
            if let Some(field) = extract_invalid_template_field(template) {
                errors.push(ValidationError::InvalidTarget(
                    IngressError::EnvInjectUnknownField {
                        target: target.clone(),
                        env_name: env_name.clone(),
                        template: template.clone(),
                        field,
                    }
                    .to_string(),
                ));
            }
        }
    }
}

fn is_valid_ingress_alias(alias: &str) -> bool {
    if alias.is_empty() {
        return false;
    }
    if alias.contains('/') || alias.contains("..") {
        return false;
    }
    let lower = alias.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return false;
    }
    if alias.as_bytes().contains(&b'%') {
        return false;
    }
    alias
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn extract_ingress_route_refs(template: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut search_from = 0;
    let prefix = "{{ingress.routes.";
    while let Some(start) = template[search_from..].find(prefix) {
        let abs_start = search_from + start;
        let rest = &template[abs_start + prefix.len()..];
        if let Some(end) = rest.find("}}") {
            let route_name = rest[..end].trim();
            if let Some(dot_pos) = route_name.find('.') {
                refs.push(route_name[..dot_pos].to_string());
            } else {
                refs.push(route_name.to_string());
            }
            search_from = abs_start + prefix.len() + end + 2;
        } else {
            break;
        }
    }
    refs
}

fn validate_dependency_contracts(
    manifest: &CapsuleManifest,
    named_targets: &HashMap<String, NamedTarget>,
    errors: &mut Vec<ValidationError>,
) {
    for (alias, dependency) in &manifest.dependencies {
        if alias.trim().is_empty() {
            errors.push(ValidationError::InvalidTarget(
                "dependencies keys must not be empty".to_string(),
            ));
        }
        if dependency.capsule.0.trim().is_empty() {
            errors.push(ValidationError::InvalidTarget(format!(
                "dependencies.{} capsule is required",
                alias
            )));
        }
        if let Some(state) = dependency.state.as_ref() {
            if state.name.trim().is_empty() {
                errors.push(ValidationError::InvalidTarget(format!(
                    "dependencies.{}.state.name is required",
                    alias
                )));
            } else if !is_kebab_case(state.name.trim()) {
                errors.push(ValidationError::InvalidTarget(format!(
                    "dependencies.{}.state.name must be kebab-case",
                    alias
                )));
            }
        }
    }

    for (label, target) in named_targets {
        for need in &target.needs {
            let need = need.trim();
            if need.is_empty() {
                errors.push(ValidationError::InvalidTarget(format!(
                    "target '{}' needs entries must not be empty",
                    label
                )));
            } else if !manifest.dependencies.contains_key(need) && !named_targets.contains_key(need)
            {
                errors.push(ValidationError::InvalidTarget(format!(
                    "target '{}' needs unknown dep alias '{}'",
                    label, need
                )));
            }
        }
    }

    for (contract_id, contract) in &manifest.contracts {
        if let Err(err) = ContractRef::parse(contract_id) {
            errors.push(ValidationError::InvalidTarget(format!(
                "contracts.{} is invalid: {}",
                contract_id, err
            )));
        }

        let target = contract.target.trim();
        if target.is_empty() {
            errors.push(ValidationError::InvalidTarget(format!(
                "contracts.{} target is required",
                contract_id
            )));
        } else if !named_targets.contains_key(target) {
            errors.push(ValidationError::InvalidTarget(format!(
                "contracts.{} references missing target '{}'",
                contract_id, target
            )));
        }

        for (credential, schema) in &contract.credentials {
            if schema.default.is_some() {
                errors.push(ValidationError::InvalidTarget(format!(
                    "contracts.{}.credentials.{} must not declare a default",
                    contract_id, credential
                )));
            }
        }

        for key in contract.identity_exports.keys() {
            if contract.runtime_exports.contains_key(key) {
                errors.push(ValidationError::InvalidTarget(format!(
                    "contracts.{} export '{}' cannot be declared in both identity_exports and runtime_exports",
                    contract_id, key
                )));
            }
        }

        if let Some(state) = contract
            .state
            .as_ref()
            .and_then(|state| state.mount.as_deref())
            && !is_valid_mount_path(state)
        {
            errors.push(ValidationError::InvalidTarget(format!(
                "contracts.{}.state.mount must be an absolute mount path",
                contract_id
            )));
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("Invalid schema_version '{0}', expected '0.3'")]
    InvalidSchemaVersion(String),
    #[error("Invalid name '{0}', must be kebab-case")]
    InvalidName(String),
    #[error("Invalid memory string for {field}: '{value}'")]
    InvalidMemoryString { field: &'static str, value: String },
    #[error("Invalid version '{0}', must be semver (e.g., 1.0.0)")]
    InvalidVersion(String),
    #[error("Inference Capsule must have capabilities defined")]
    MissingCapabilities,
    #[error("Inference Capsule must have model config defined")]
    MissingModelConfig,
    #[error("Invalid port {0}")]
    InvalidPort(u16),
    #[error("Storage volumes are only supported for execution.runtime=docker")]
    StorageOnlyForDocker,
    #[error("Invalid storage volume (requires unique name and absolute mount_path)")]
    InvalidStorageVolume,
    #[error("default_target is required")]
    MissingDefaultTarget,
    #[error("At least one [targets.<label>] entry is required")]
    MissingTargets,
    #[error("default_target '{0}' does not exist under [targets]")]
    DefaultTargetNotFound(String),
    #[error("Invalid target: {0}")]
    InvalidTarget(String),
    #[error(
        "Invalid target '{0}': unsupported driver '{1}' (allowed: static|deno|node|python|wasmtime|native)"
    )]
    InvalidTargetDriver(String, String),
    #[error("Invalid target '{0}': runtime_version is required for runtime=source driver='{1}'")]
    MissingRuntimeVersion(String, String),
    #[error("Invalid web target '{0}': {1}")]
    InvalidWebTarget(String, String),
    #[error("Invalid service '{0}': {1}")]
    InvalidService(String, String),
    #[error("Invalid state '{0}': {1}")]
    InvalidState(String, String),
    #[error("Invalid state binding for service '{0}': {1}")]
    InvalidStateBinding(String, String),
    #[error(
        "State '{state}' is bound by both service '{first_service}' and '{second_service}'; shared mutable state requires sharing=\"same-capsule\" on the state declaration"
    )]
    StateSharedRequiresPolicy {
        state: String,
        first_service: String,
        second_service: String,
    },
    #[error("Shared state '{0}' requires schema_id to be set")]
    StateSharedRequiresSchemaId(String),
    #[error("Shared state '{0}' has conflicting mount modes across services")]
    StateSharedConflictingMountMode(String),
    #[error("Shared state across different capsules is forbidden: state '{0}'")]
    StateSharedCrossCapsuleForbidden(String),
    #[error("Shared state '{0}' cannot use absolute host path bind mounts")]
    StateSharedHostBindForbidden(String),
    #[error("State key '{0}' is not declared under [state]")]
    StateKeyUndeclared(String),
    #[error("type='tool' capsule must declare at least one [platforms.<os>-<arch>] entry")]
    ToolMissingPlatforms,
    #[error(
        "Invalid platforms key '{0}': expected '<os>-<arch>' (e.g. darwin-arm64, linux-x86_64)"
    )]
    ToolInvalidPlatformKey(String),
    #[error("platforms.{0}.artifact must be a non-empty filename or URL")]
    ToolMissingArtifact(String),
    #[error("platforms.{0}.sha256 must be 64 hex characters")]
    ToolInvalidArtifactSha256(String),
    #[error(
        "exports.{kind}.{alias} = '{path}' must be a relative path under the tool root (no '/', no '..')"
    )]
    ToolExportPathInvalid {
        kind: &'static str,
        alias: String,
        path: String,
    },
    #[error("type='tool' capsule must not declare [services]")]
    ToolMustNotDeclareServices,
    #[error(
        "type='tool' capsule must not declare [dependencies] (use [tool_dependencies] for nested tool capsules)"
    )]
    ToolMustNotDeclareServiceDependencies,
    #[error("[platforms] is only valid on type='tool' capsules")]
    PlatformsRequiresToolType,
    #[error(
        "tool_dependencies.{alias}.bind_env.{export} = '{env_name}' is not a valid POSIX env-var name"
    )]
    ToolDependencyInvalidEnvVar {
        alias: String,
        export: String,
        env_name: String,
    },
    #[error("Invalid host capability: {0}")]
    InvalidHostCapability(String),
    #[error(
        "Unsupported host capability '{name}': this Ato build does not yet implement a host \
         execution path or consent UI for it. Remove the [[host_capabilities]] entry or track \
         https://github.com/ato-run/ato/issues/468 for implementation."
    )]
    UnsupportedHostCapability { name: String },
}

pub(crate) fn is_kebab_case(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let chars: Vec<char> = s.chars().collect();
    if !chars[0].is_ascii_lowercase() && !chars[0].is_ascii_digit() {
        return false;
    }
    if !chars.last().unwrap().is_ascii_lowercase() && !chars.last().unwrap().is_ascii_digit() {
        return false;
    }
    chars
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
}

pub(crate) fn is_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let version_part = parts[0];
    let version_nums: Vec<&str> = version_part.split('.').collect();

    if version_nums.len() != 3 {
        return false;
    }

    version_nums.iter().all(|n| n.parse::<u32>().is_ok())
}

pub(crate) fn is_valid_mount_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    !rest.is_empty()
        && !rest.contains('\\')
        && rest
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

#[cfg(test)]
mod tests {
    use super::is_valid_mount_path;

    #[test]
    fn mount_paths_are_unix_container_paths() {
        assert!(is_valid_mount_path("/app/backend/data"));
        assert!(is_valid_mount_path("/var/lib/postgresql/data"));

        assert!(!is_valid_mount_path(""));
        assert!(!is_valid_mount_path("/"));
        assert!(!is_valid_mount_path("relative/path"));
        assert!(!is_valid_mount_path("C:\\data"));
        assert!(!is_valid_mount_path("/app\\data"));
        assert!(!is_valid_mount_path("/app/../data"));
        assert!(!is_valid_mount_path("/app//data"));
    }

    #[test]
    fn recipe_state_binding_paths_accepted_on_windows() {
        assert!(is_valid_mount_path("/var/opt/memos"));
        assert!(is_valid_mount_path("/app/data"));
        assert!(is_valid_mount_path("/pb_data"));
        assert!(is_valid_mount_path("/app/config"));
        assert!(is_valid_mount_path("/app/public/icons"));
        assert!(is_valid_mount_path("/var/lib/postgresql/data"));
        assert!(is_valid_mount_path("/home/node/.n8n"));
        assert!(is_valid_mount_path("/shiori"));
        assert!(is_valid_mount_path("/database"));
        assert!(is_valid_mount_path("/srv"));
        assert!(is_valid_mount_path("/app/backend/data"));
    }

    #[test]
    fn relative_container_target_data_is_rejected() {
        assert!(!is_valid_mount_path("data"));
        assert!(!is_valid_mount_path("relative/path"));
        assert!(!is_valid_mount_path("./data"));
    }

    fn native_inference_manifest(engine_version: &str) -> String {
        format!(
            "schema_version = \"0.3\"\nname = \"x\"\nversion = \"0.1.0\"\ntype = \"app\"\n\
             [targets.app]\nruntime = \"native-inference\"\nengine = \"llama.cpp\"\n\
             engine_version = \"{engine_version}\"\nmodel = \"./m.gguf\"\n"
        )
    }

    fn engine_version_error(engine_version: &str) -> bool {
        let toml = native_inference_manifest(engine_version);
        let manifest: crate::foundation::types::manifest::CapsuleManifest =
            toml::from_str(&toml).expect("parse manifest");
        match manifest.validate() {
            Ok(()) => false,
            Err(errs) => errs
                .iter()
                .any(|e| e.to_string().contains("engine_version")),
        }
    }

    #[test]
    fn native_inference_rejects_unsafe_engine_version() {
        assert!(engine_version_error("../evil"));
        assert!(engine_version_error("b9754/../../x"));
        assert!(engine_version_error("a/b"));
    }

    #[test]
    fn native_inference_accepts_safe_engine_version() {
        assert!(!engine_version_error("b9754"));
    }

    fn validate_native_model(model_block: &str) -> Result<(), Vec<super::ValidationError>> {
        let toml = format!(
            "schema_version = \"0.3\"\nname = \"native-llama\"\nversion = \"0.1.0\"\ntype = \"app\"\n\
             default_target = \"app\"\n\
             [targets.app]\nruntime = \"native-inference\"\nengine_path = \"./llama-server\"\n\
             {model_block}"
        );
        let manifest: crate::foundation::types::manifest::CapsuleManifest =
            toml::from_str(&toml).expect("parse manifest");
        manifest.validate()
    }

    fn has_err(result: &Result<(), Vec<super::ValidationError>>, needle: &str) -> bool {
        matches!(result, Err(errs) if errs.iter().any(|e| e.to_string().contains(needle)))
    }

    #[test]
    fn native_inference_accepts_managed_model() {
        let hex = "a".repeat(64);
        let r = validate_native_model(&format!(
            "model_url = \"https://example.com/m.gguf\"\nmodel_sha256 = \"{hex}\"\n"
        ));
        assert!(r.is_ok(), "managed model should validate: {r:?}");
    }

    #[test]
    fn native_inference_rejects_bad_model_url_and_sha() {
        let hex = "a".repeat(64);
        // non-http(s) url
        assert!(has_err(
            &validate_native_model(&format!(
                "model_url = \"hf://repo/m\"\nmodel_sha256 = \"{hex}\"\n"
            )),
            "model_url"
        ));
        // bad sha256
        assert!(has_err(
            &validate_native_model(
                "model_url = \"https://e.com/m.gguf\"\nmodel_sha256 = \"deadbeef\"\n"
            ),
            "model_sha256"
        ));
        // model_url without sha256
        assert!(has_err(
            &validate_native_model("model_url = \"https://e.com/m.gguf\"\n"),
            "model_sha256"
        ));
        // neither model nor model_url
        assert!(has_err(&validate_native_model(""), "model"));
    }
}

pub(crate) fn is_valid_platform_key(s: &str) -> bool {
    let Some((os, arch)) = s.split_once('-') else {
        return false;
    };
    matches!(os, "darwin" | "linux" | "windows") && matches!(arch, "arm64" | "x86_64")
}

pub(crate) fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub(crate) fn is_valid_relative_export_path(s: &str) -> bool {
    if s.trim().is_empty() {
        return false;
    }
    let path = Path::new(s);
    if path.is_absolute() {
        return false;
    }
    path.components().all(|c| {
        !matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    })
}

pub(crate) fn is_valid_env_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn infer_source_driver(target: &NamedTarget) -> Option<String> {
    if let Some(driver) = target.driver.as_ref() {
        let normalized = driver.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    if let Some(language) = target.language.as_ref() {
        let normalized = language.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    None
}

fn split_runtime_driver(runtime: &str) -> (Option<String>, Option<String>) {
    let normalized = runtime.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return (None, None);
    }
    if let Some((base, driver)) = normalized.split_once('/') {
        let base = (!base.trim().is_empty()).then(|| base.trim().to_string());
        let driver = (!driver.trim().is_empty()).then(|| driver.trim().to_string());
        return (base, driver);
    }
    (Some(normalized), None)
}

fn detect_service_cycle(services: &HashMap<String, ServiceSpec>) -> Result<(), String> {
    fn visit(
        current: &str,
        services: &HashMap<String, ServiceSpec>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Result<(), String> {
        if visited.contains(current) {
            return Ok(());
        }
        if visiting.contains(current) {
            stack.push(current.to_string());
            return Err(stack.join(" -> "));
        }

        visiting.insert(current.to_string());
        stack.push(current.to_string());
        if let Some(spec) = services.get(current)
            && let Some(deps) = spec.depends_on.as_ref()
        {
            for dep in deps {
                if services.contains_key(dep) {
                    visit(dep, services, visiting, visited, stack)?;
                }
            }
        }
        stack.pop();
        visiting.remove(current);
        visited.insert(current.to_string());
        Ok(())
    }

    let mut names: Vec<&String> = services.keys().collect();
    names.sort();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for name in names {
        let mut stack = Vec::new();
        visit(name, services, &mut visiting, &mut visited, &mut stack)?;
    }
    Ok(())
}

fn validate_upstream_prefix_segments(prefix: &str) -> Option<String> {
    if prefix.contains("..") {
        return Some("must not contain parent-traversal component '..'".to_string());
    }
    if prefix.contains('\\') {
        return Some("must not contain backslash".to_string());
    }
    let lower = prefix.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return Some("must not contain percent-encoded slash or backslash".to_string());
    }
    if prefix.as_bytes().contains(&b'%') {
        return Some("must not contain percent-encoded characters".to_string());
    }
    None
}

fn extract_invalid_template_field(template: &str) -> Option<String> {
    let allowed: &[&str] = &["url", "base_url", "path", "origin"];
    let mut search_from = 0;
    let prefix = "{{ingress.routes.";
    while let Some(start) = template[search_from..].find(prefix) {
        let abs_start = search_from + start;
        let rest = &template[abs_start + prefix.len()..];
        if let Some(end) = rest.find("}}") {
            let full_ref = &rest[..end];
            if let Some(dot_pos) = full_ref.find('.') {
                let field = &full_ref[dot_pos + 1..];
                if !allowed.contains(&field) {
                    return Some(field.to_string());
                }
            }
            search_from = abs_start + prefix.len() + end + 2;
        } else {
            break;
        }
    }
    None
}
