use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::common::paths::manifest_dir;
use crate::error::CapsuleError;
use crate::types::ValidationMode;

pub fn validate_manifest_for_build(
    manifest_path: &Path,
    target_label: &str,
) -> Result<(), CapsuleError> {
    validate_manifest_for_build_with_mode(manifest_path, target_label, ValidationMode::Strict)
}

pub fn validate_manifest_for_build_with_mode(
    manifest_path: &Path,
    target_label: &str,
    validation_mode: ValidationMode,
) -> Result<(), CapsuleError> {
    let loaded =
        crate::manifest::load_manifest_with_validation_mode(manifest_path, validation_mode)?;
    let raw = loaded.raw;
    let original_raw: toml::Value = toml::from_str(&loaded.raw_text).map_err(|err| {
        manifest_err(
            manifest_path,
            format!("failed to parse original manifest TOML: {err}"),
        )
    })?;
    validate_pack_config(manifest_path, &raw)?;

    let target = raw
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(target_label))
        .and_then(|v| v.as_table())
        .ok_or_else(|| manifest_err(manifest_path, format!("targets.{target_label} is missing")))?;
    let manifest_dir = manifest_dir(manifest_path);
    let target_manifest_dir = target
        .get("working_dir")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| manifest_dir.join(value))
        .unwrap_or_else(|| manifest_dir.clone());

    let runtime = target
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let driver = target
        .get("driver")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase());

    let entrypoint = target
        .get("entrypoint")
        .and_then(|v| v.as_str())
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    let run_command = target
        .get("run_command")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let has_services = raw
        .get("services")
        .and_then(|v| v.as_table())
        .map(|services| !services.is_empty())
        .unwrap_or(false);
    // v0.3 collapses `runtime = "web/deno"` to `runtime = source` + `driver = deno`,
    // so detect the web-services validation context from the original
    // (un-normalized) runtime selector or the structured marker as well.
    let original_runtime = original_raw
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let originally_web_runtime =
        original_runtime == "web" || original_runtime.starts_with("web/");
    let web_services_mode = (runtime == "web" || originally_web_runtime)
        && driver.as_deref() == Some("deno")
        && has_services;

    if runtime == "web" || (originally_web_runtime && runtime == "source") {
        let driver = driver.ok_or_else(|| {
            manifest_err(
                manifest_path,
                format!(
                    "targets.{target_label}.driver is required for runtime=web (static|node|deno|python)"
                ),
            )
        })?;
        if matches!(driver.as_str(), "browser_static" | "browser-static") {
            return Err(manifest_err(
                manifest_path,
                format!(
                    "targets.{target_label}.driver='{}' is not supported. Use 'static'",
                    driver
                ),
            ));
        }
        if !matches!(driver.as_str(), "static" | "node" | "deno" | "python") {
            return Err(manifest_err(
                manifest_path,
                format!(
                    "targets.{target_label}.driver='{}' is invalid for runtime=web (allowed: static|node|deno|python)",
                    driver
                ),
            ));
        }
        if target.get("public").is_some() {
            return Err(manifest_err(
                manifest_path,
                format!("targets.{target_label}.public is no longer supported"),
            ));
        }

        if let Some(port_raw) = target.get("port") {
            let port = port_raw.as_integer().ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("targets.{target_label}.port must be an integer"),
                )
            })?;
            if !(1..=65535).contains(&port) {
                return Err(manifest_err(
                    manifest_path,
                    format!("targets.{target_label}.port must be between 1 and 65535"),
                ));
            }
        } else if !matches!(validation_mode, ValidationMode::Preview) {
            return Err(manifest_err(
                manifest_path,
                format!("targets.{target_label}.port is required for runtime=web"),
            ));
        }

        let runtime_tools = read_runtime_tools_map(manifest_path, target_label, target)?;
        if web_services_mode {
            if let Some(entrypoint) = entrypoint {
                if is_deprecated_ato_entrypoint(entrypoint) {
                    return Err(manifest_err(
                        manifest_path,
                        format!(
                            "targets.{target_label}.entrypoint='ato-entry.ts' is deprecated. Define top-level [services] and remove ato-entry.ts orchestrator."
                        ),
                    ));
                }
            }

            validate_web_services_mode(manifest_path, target_label, &raw, &runtime_tools)?;
        } else {
            if let Some(rc) = run_command {
                if matches!(driver.as_str(), "node" | "deno" | "python") {
                    // Apply the same diagnostic checks that the entrypoint
                    // path uses, but on the run_command shorthand. Multi-token
                    // shell commands are rejected because the build pipeline
                    // expects a script file path it can introspect.
                    if rc.split_whitespace().count() > 1 {
                        return Err(manifest_err(
                            manifest_path,
                            format!(
                                "targets.{target_label}.entrypoint must be a script file path (shell command strings are not allowed)"
                            ),
                        ));
                    }
                    if driver == "deno" && is_deprecated_ato_entrypoint(rc) {
                        return Err(manifest_err(
                            manifest_path,
                            format!(
                                "targets.{target_label}.entrypoint='ato-entry.ts' is deprecated. Use top-level [services] mode instead."
                            ),
                        ));
                    }
                    return Ok(());
                }
            }
            let entrypoint = entrypoint.ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("targets.{target_label}.entrypoint is required"),
                )
            })?;
            let clean_entrypoint = entrypoint.trim_start_matches("./");
            if !is_safe_relative_path(clean_entrypoint) {
                return Err(manifest_err(
                    manifest_path,
                    format!(
                        "targets.{target_label}.entrypoint='{}' must be a safe relative path",
                        entrypoint
                    ),
                ));
            }
            if driver == "deno" && is_deprecated_ato_entrypoint(entrypoint) {
                return Err(manifest_err(
                    manifest_path,
                    format!(
                        "targets.{target_label}.entrypoint='ato-entry.ts' is deprecated. Use top-level [services] mode instead."
                    ),
                ));
            }

            let path_in_root = target_manifest_dir.join(clean_entrypoint);
            let path_in_source = target_manifest_dir.join("source").join(clean_entrypoint);
            match driver.as_str() {
                "static" => {
                    if !path_in_root.exists() || !path_in_root.is_dir() {
                        return Err(manifest_err(
                            manifest_path,
                            format!(
                                "targets.{target_label}.entrypoint='{}' must be an existing directory under project root ('{}')",
                                entrypoint,
                                path_in_root.display()
                            ),
                        ));
                    }
                }
                "node" | "deno" | "python" => {
                    if entrypoint.split_whitespace().count() > 1 {
                        return Err(manifest_err(
                            manifest_path,
                            format!(
                                "targets.{target_label}.entrypoint must be a script file path (shell command strings are not allowed)"
                            ),
                        ));
                    }
                    if (!path_in_root.exists() || !path_in_root.is_file())
                        && (!path_in_source.exists() || !path_in_source.is_file())
                    {
                        return Err(manifest_err(
                            manifest_path,
                            format!(
                                "entrypoint file not found: targets.{target_label}.entrypoint='{}'. Checked '{}' and '{}'",
                                entrypoint,
                                path_in_root.display(),
                                path_in_source.display()
                            ),
                        ));
                    }
                }
                _ => unreachable!(),
            }
        }
    } else {
        let image = target
            .get("image")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if runtime == "source" && run_command.is_some() {
            // Shell-native source targets are validated by the manifest parser; they do not
            // require an entrypoint file on disk when run_command is present.
        } else if runtime == "oci" && entrypoint.is_none() && image.is_some() {
            // OCI targets may boot from image metadata and optionally use run_command/cmd.
        } else {
            let entrypoint = entrypoint.ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("targets.{target_label}.entrypoint is required"),
                )
            })?;
            let clean_entrypoint = entrypoint.trim_start_matches("./");
            if clean_entrypoint.contains('/') || clean_entrypoint.contains('\\') {
                let path_in_root = target_manifest_dir.join(clean_entrypoint);
                let path_in_source = target_manifest_dir.join("source").join(clean_entrypoint);
                if !path_in_root.exists() && !path_in_source.exists() {
                    return Err(manifest_err(
                    manifest_path,
                    format!(
                        "entrypoint not found: targets.{target_label}.entrypoint='{}'. Checked '{}' and '{}'",
                        entrypoint,
                        path_in_root.display(),
                        path_in_source.display()
                    ),
                ));
                }
            }
        }

        if let Some(port_raw) = target.get("port") {
            let port = port_raw.as_integer().ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("targets.{target_label}.port must be an integer"),
                )
            })?;
            if !(1..=65535).contains(&port) {
                return Err(manifest_err(
                    manifest_path,
                    format!("targets.{target_label}.port must be between 1 and 65535"),
                ));
            }
        }
    }

    let original_target = original_raw
        .get("targets")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(target_label))
        .and_then(|v| v.as_table());

    // Flat v0.3 manifests carry the smoke block at the top level (it gets
    // migrated into `targets.<default_target>` during normalization). Detect
    // either form so the smoke validator runs in both layouts.
    let has_smoke_block = original_target
        .and_then(|target| target.get("smoke"))
        .is_some()
        || original_raw.get("smoke").is_some();

    if has_smoke_block {
        crate::smoke::parse_smoke_options(&original_raw, target_label)
            .map_err(|err| manifest_err(manifest_path, err.to_string()))?;
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct ServiceDiagnosticSpec {
    depends_on: Vec<String>,
}

fn read_runtime_tools_map(
    manifest_path: &Path,
    target_label: &str,
    target: &toml::value::Table,
) -> Result<HashMap<String, String>, CapsuleError> {
    let mut tools = HashMap::new();
    let Some(runtime_tools) = target.get("runtime_tools") else {
        return Ok(tools);
    };
    let tools_table = runtime_tools.as_table().ok_or_else(|| {
        manifest_err(
            manifest_path,
            format!("targets.{target_label}.runtime_tools must be a table"),
        )
    })?;

    for (tool, version) in tools_table {
        let version = version.as_str().map(str::trim).ok_or_else(|| {
            manifest_err(
                manifest_path,
                format!("targets.{target_label}.runtime_tools.{tool} must be a non-empty string"),
            )
        })?;
        if version.is_empty() {
            return Err(manifest_err(
                manifest_path,
                format!("targets.{target_label}.runtime_tools.{tool} must be a non-empty string"),
            ));
        }
        tools.insert(tool.to_ascii_lowercase(), version.to_string());
    }
    Ok(tools)
}

fn validate_web_services_mode(
    manifest_path: &Path,
    target_label: &str,
    raw: &toml::Value,
    runtime_tools: &HashMap<String, String>,
) -> Result<(), CapsuleError> {
    let services = raw
        .get("services")
        .and_then(|v| v.as_table())
        .ok_or_else(|| {
            manifest_err(
                manifest_path,
                "top-level [services] is required for web/deno services mode".to_string(),
            )
        })?;
    if services.is_empty() {
        return Err(manifest_err(
            manifest_path,
            "top-level [services] must define at least one service".to_string(),
        ));
    }
    if !services.contains_key("main") {
        return Err(manifest_err(
            manifest_path,
            "services.main is required for web/deno services mode".to_string(),
        ));
    }

    let mut parsed: HashMap<String, ServiceDiagnosticSpec> = HashMap::new();
    let mut referenced_tools: HashSet<String> = HashSet::new();

    for (name, value) in services {
        let service = value.as_table().ok_or_else(|| {
            manifest_err(manifest_path, format!("services.{name} must be a table"))
        })?;

        let entrypoint = service
            .get("entrypoint")
            .or_else(|| service.get("command"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("services.{name}.entrypoint is required"),
                )
            })?
            .to_string();

        if let Some(expose) = service.get("expose") {
            let expose = expose.as_array().ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("services.{name}.expose must be an array"),
                )
            })?;
            if !expose.is_empty() {
                return Err(manifest_err(
                    manifest_path,
                    format!(
                        "services.{name}.expose is not supported yet in web/deno services mode"
                    ),
                ));
            }
        }

        if let Some(probe) = service.get("readiness_probe") {
            let probe = probe.as_table().ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("services.{name}.readiness_probe must be a table"),
                )
            })?;
            let port = probe
                .get("port")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    manifest_err(
                        manifest_path,
                        format!("services.{name}.readiness_probe.port must be a non-empty string"),
                    )
                })?;
            let _ = port;

            let has_http_get = probe
                .get("http_get")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_some();
            let has_tcp_connect = probe
                .get("tcp_connect")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .is_some();
            if !has_http_get && !has_tcp_connect {
                return Err(manifest_err(
                    manifest_path,
                    format!("services.{name}.readiness_probe must define http_get or tcp_connect"),
                ));
            }
        }

        let depends_on = if let Some(depends_on) = service.get("depends_on") {
            let depends_on = depends_on.as_array().ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("services.{name}.depends_on must be an array"),
                )
            })?;
            let mut deps = Vec::new();
            for (idx, dep) in depends_on.iter().enumerate() {
                let dep = dep
                    .as_str()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| {
                        manifest_err(
                            manifest_path,
                            format!("services.{name}.depends_on[{idx}] must be a non-empty string"),
                        )
                    })?;
                deps.push(dep.to_string());
            }
            deps
        } else {
            Vec::new()
        };

        if let Some(head) = parse_command_head(&entrypoint).map_err(|err| {
            manifest_err(
                manifest_path,
                format!("services.{name}.entrypoint is invalid: {err}"),
            )
        })? {
            if matches!(head.as_str(), "node" | "python" | "uv") {
                referenced_tools.insert(head);
            }
        }

        parsed.insert(name.to_string(), ServiceDiagnosticSpec { depends_on });
    }

    for (name, service) in &parsed {
        for dep in &service.depends_on {
            if !parsed.contains_key(dep) {
                return Err(manifest_err(
                    manifest_path,
                    format!("services.{name}.depends_on references unknown service '{dep}'"),
                ));
            }
        }
    }

    detect_service_cycle(&parsed).map_err(|cycle| {
        manifest_err(
            manifest_path,
            format!("services has circular dependency: {}", cycle),
        )
    })?;

    for tool in referenced_tools {
        if !runtime_tools.contains_key(&tool) {
            return Err(manifest_err(
                manifest_path,
                format!(
                    "targets.{target_label}.runtime_tools.{tool} is required when services command references '{tool}'"
                ),
            ));
        }
    }

    Ok(())
}

fn detect_service_cycle(services: &HashMap<String, ServiceDiagnosticSpec>) -> Result<(), String> {
    fn visit(
        current: &str,
        services: &HashMap<String, ServiceDiagnosticSpec>,
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
        if let Some(service) = services.get(current) {
            for dep in &service.depends_on {
                visit(dep, services, visiting, visited, stack)?;
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

fn parse_command_head(command: &str) -> Result<Option<String>, String> {
    let tokens = shell_words::split(command).map_err(|err| err.to_string())?;
    let first = tokens
        .first()
        .map(|token| token.trim().to_ascii_lowercase());
    Ok(first.filter(|token| !token.is_empty()))
}

fn is_deprecated_ato_entrypoint(entrypoint: &str) -> bool {
    std::path::Path::new(entrypoint.trim())
        .file_name()
        .and_then(|v| v.to_str())
        .map(|v| v.eq_ignore_ascii_case("ato-entry.ts"))
        .unwrap_or(false)
}

fn manifest_err(path: &Path, message: String) -> CapsuleError {
    CapsuleError::Manifest(path.to_path_buf(), message)
}

fn is_safe_relative_path(path: &str) -> bool {
    use std::path::Component;
    !Path::new(path).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) && !path.starts_with('~')
}

fn validate_pack_config(manifest_path: &Path, raw: &toml::Value) -> Result<(), CapsuleError> {
    let Some(pack) = raw.get("pack") else {
        return Ok(());
    };
    let pack = pack
        .as_table()
        .ok_or_else(|| manifest_err(manifest_path, "pack must be a table".to_string()))?;

    for field in ["include", "exclude"] {
        let Some(value) = pack.get(field) else {
            continue;
        };
        let arr = value.as_array().ok_or_else(|| {
            manifest_err(
                manifest_path,
                format!("pack.{field} must be an array of strings"),
            )
        })?;

        for (idx, pattern) in arr.iter().enumerate() {
            let pattern = pattern.as_str().ok_or_else(|| {
                manifest_err(
                    manifest_path,
                    format!("pack.{field}[{idx}] must be a non-empty string"),
                )
            })?;
            if pattern.trim().is_empty() {
                return Err(manifest_err(
                    manifest_path,
                    format!("pack.{field}[{idx}] must be a non-empty string"),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_smoke_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "cli-smoke"
version = "0.1.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11.9"
startup_timeout_ms = 0
run = "main.py"
[smoke]"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("main.py"), "print('ok')").unwrap();

        assert!(validate_manifest_for_build(&manifest_path, "app").is_err());
    }

    #[test]
    fn accepts_valid_smoke_block() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "cli-smoke"
version = "0.1.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11.9"
startup_timeout_ms = 1500
check_commands = ["python -V"]
run = "main.py"
[smoke]"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("main.py"), "print('ok')").unwrap();

        assert!(validate_manifest_for_build(&manifest_path, "app").is_ok());
    }

    #[test]
    fn web_static_requires_existing_directory_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-static"
version = "0.1.0"
type = "app"

runtime = "web/static"
port = 8080
run = "dist""#,
        )
        .unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err
            .to_string()
            .contains("must be an existing directory under project root"));

        std::fs::create_dir_all(dir.path().join("dist")).unwrap();
        assert!(validate_manifest_for_build(&manifest_path, "app").is_ok());
    }

    #[test]
    fn web_static_rejects_home_alias_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-static"
version = "0.1.0"
type = "app"

runtime = "web/static"
port = 8080
run = "~/dist""#,
        )
        .unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err.to_string().contains("must be a safe relative path"));
    }

    #[test]
    fn preview_web_static_allows_missing_port() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-static"
version = "0.1.0"
type = "app"

runtime = "web/static"
run = "dist""#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.path().join("dist")).unwrap();
        let result = validate_manifest_for_build_with_mode(
            &manifest_path,
            "app",
            ValidationMode::Preview,
        );
        assert!(result.is_ok(), "{:?}", result.err());
    }

    #[test]
    fn web_dynamic_rejects_shell_style_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-node"
version = "0.1.0"
type = "app"

runtime = "web/node"
port = 3000
run = "npm run start""#,
        )
        .unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err
            .to_string()
            .contains("entrypoint must be a script file path"));
    }

    #[test]
    fn web_deno_services_allows_missing_target_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-services"
version = "0.1.0"
type = "app"

runtime = "web/deno"
port = 4173
runtime_tools = { node = "20.11.0" }
[services.main]
entrypoint = "node apps/dashboard/server.js"
"#,
        )
        .unwrap();

        assert!(validate_manifest_for_build(&manifest_path, "app").is_ok());
    }

    #[test]
    fn web_deno_services_requires_main() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-services"
version = "0.1.0"
type = "app"

runtime = "web/deno"
port = 4173
[services.api]
entrypoint = "python apps/api/main.py"
"#,
        )
        .unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err.to_string().contains("services.main is required"));
    }

    #[test]
    fn web_deno_services_requires_runtime_tool_for_referenced_command() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-services"
version = "0.1.0"
type = "app"

runtime = "web/deno"
port = 4173
[services.main]
entrypoint = "node apps/dashboard/server.js"
"#,
        )
        .unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err.to_string().contains("runtime_tools.node is required"));
    }

    #[test]
    fn web_deno_rejects_deprecated_ato_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "web-deno"
version = "0.1.0"
type = "app"

runtime = "web/deno"
port = 4173
run = "ato-entry.ts""#,
        )
        .unwrap();
        std::fs::write(dir.path().join("ato-entry.ts"), "console.log('ok');").unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err.to_string().contains("deprecated"));
    }

    #[test]
    fn rejects_empty_pack_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("capsule.toml");
        std::fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "pack-test"
version = "0.1.0"
type = "app"

runtime = "web/deno"
port = 4173
run = "ato-entry.ts"
[pack]
include = ["", "apps/**"]
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("ato-entry.ts"), "console.log('ok');").unwrap();

        let err = validate_manifest_for_build(&manifest_path, "app").unwrap_err();
        assert!(err.to_string().contains("pack.include"));
    }
}
