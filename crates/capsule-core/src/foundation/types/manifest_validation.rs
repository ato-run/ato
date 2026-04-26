//! Manifest validation rules and state/service helper logic.

use super::*;

impl CapsuleManifest {
    /// Validate the manifest
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        self.validate_for_mode(ValidationMode::Strict)
    }

    pub fn validate_for_mode(&self, mode: ValidationMode) -> Result<(), Vec<ValidationError>> {
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

        if let Some(v) = &self.requirements.vram_min {
            if parse_memory_string(v).is_err() {
                errors.push(ValidationError::InvalidMemoryString {
                    field: "requirements.vram_min",
                    value: v.clone(),
                });
            }
        }
        if let Some(v) = &self.requirements.vram_recommended {
            if parse_memory_string(v).is_err() {
                errors.push(ValidationError::InvalidMemoryString {
                    field: "requirements.vram_recommended",
                    value: v.clone(),
                });
            }
        }
        if let Some(v) = &self.requirements.disk {
            if parse_memory_string(v).is_err() {
                errors.push(ValidationError::InvalidMemoryString {
                    field: "requirements.disk",
                    value: v.clone(),
                });
            }
        }

        if self.capsule_type == CapsuleType::Inference && self.capabilities.is_none() {
            errors.push(ValidationError::MissingCapabilities);
        }

        if self.capsule_type == CapsuleType::Inference && self.model.is_none() {
            errors.push(ValidationError::MissingModelConfig);
        }

        let is_v03_library = schema_is_v03 && self.capsule_type == CapsuleType::Library;
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
        if !is_v03_library && self.default_target.trim().is_empty() {
            errors.push(ValidationError::MissingDefaultTarget);
        }
        if !is_v03_library && named_targets.is_empty() {
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
                || !matches!(runtime.as_str(), "source" | "wasm" | "oci" | "web")
            {
                errors.push(ValidationError::InvalidTarget(label.clone()));
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
                if probe.port.trim().is_empty() {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe.port must be a non-empty placeholder name",
                        label
                    )));
                }
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
                if !has_http_get && !has_tcp_connect {
                    errors.push(ValidationError::InvalidTarget(format!(
                        "target '{}': readiness_probe must define http_get or tcp_connect",
                        label
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
                        if probe.port.trim().is_empty() {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.port must be a non-empty placeholder name"
                                    .to_string(),
                            ));
                        }
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
                        if !has_http_get && !has_tcp_connect {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe must define http_get or tcp_connect".to_string(),
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
                        {
                            if !network.allow_from.is_empty()
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
                        if probe.port.trim().is_empty() {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe.port must be a non-empty placeholder name"
                                    .to_string(),
                            ));
                        }
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
                        if !has_http_get && !has_tcp_connect {
                            errors.push(ValidationError::InvalidService(
                                name.to_string(),
                                "readiness_probe must define http_get or tcp_connect".to_string(),
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

                    if let Some(target) = named_targets.get(target_label) {
                        if !target.runtime.eq_ignore_ascii_case("oci") {
                            errors.push(ValidationError::InvalidStateBinding(
                                service_name.clone(),
                                format!(
                                    "state_bindings are only supported for OCI targets in this PoC (target '{}')",
                                    target_label
                                ),
                            ));
                        }
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
                            {
                                if previous_service != *service_name {
                                    errors.push(ValidationError::InvalidStateBinding(
                                        service_name.clone(),
                                        format!(
                                            "state '{}' is already bound by service '{}'; shared mutable state is not supported in this PoC",
                                            state_name, previous_service
                                        ),
                                    ));
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
    #[error("Invalid target '{0}': unsupported driver '{1}' (allowed: static|deno|node|python|wasmtime|native)")]
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
    let path = Path::new(path);
    path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
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
        if let Some(spec) = services.get(current) {
            if let Some(deps) = spec.depends_on.as_ref() {
                for dep in deps {
                    if services.contains_key(dep) {
                        visit(dep, services, visiting, visited, stack)?;
                    }
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
