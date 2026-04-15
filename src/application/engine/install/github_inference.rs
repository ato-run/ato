use super::*;
use anyhow::bail;
use rand::Rng;
use std::net::TcpListener;

const DEFAULT_GITHUB_AUTO_FIX_PORT_RANGE_START: u16 = 18000;
const DEFAULT_GITHUB_AUTO_FIX_PORT_RANGE_END: u16 = 18999;
const ENV_GITHUB_AUTO_FIX_PORT_RANGE_START: &str = "ATO_GITHUB_AUTO_FIX_PORT_RANGE_START";
const ENV_GITHUB_AUTO_FIX_PORT_RANGE_END: &str = "ATO_GITHUB_AUTO_FIX_PORT_RANGE_END";

fn collapse_legacy_required_env_field(table: &mut toml::value::Table) -> bool {
    let legacy_required = table
        .get("env")
        .and_then(|env| env.get("required"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        });

    let Some(legacy_required) = legacy_required else {
        return false;
    };

    let mut merged = table
        .get("required_env")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for value in legacy_required {
        if !merged.iter().any(|existing| existing == &value) {
            merged.push(value);
        }
    }

    table.insert(
        "required_env".to_string(),
        toml::Value::Array(merged.into_iter().map(toml::Value::String).collect()),
    );

    let mut remove_env_table = false;
    if let Some(env_table) = table.get_mut("env").and_then(toml::Value::as_table_mut) {
        env_table.remove("required");
        remove_env_table = env_table.is_empty();
    }
    if remove_env_table {
        table.remove("env");
    }

    true
}

pub(super) fn normalize_github_install_preview_toml(
    checkout_dir: &Path,
    manifest_text: &str,
) -> Result<String> {
    let Ok(mut parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return Ok(manifest_text.to_string());
    };

    if parsed
        .get("schema_version")
        .and_then(toml::Value::as_str)
        .map(|value| value.trim() == "0.3")
        .unwrap_or(false)
        && parsed.get("targets").is_none()
    {
        {
            let table = parsed
                .as_table_mut()
                .expect("normalized GitHub install draft must stay a table");
            collapse_legacy_required_env_field(table);
        }

        let runtime = parsed
            .get("runtime")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());
        let runtime_version_missing = parsed
            .get("runtime_version")
            .and_then(toml::Value::as_str)
            .map(|value| value.trim().is_empty())
            .unwrap_or(true);

        if runtime_version_missing {
            let driver = match runtime.as_deref() {
                Some("source/node") | Some("web/node") => Some("node"),
                Some("source/python") | Some("web/python") => Some("python"),
                Some("source/deno") | Some("web/deno") => Some("deno"),
                _ => None,
            };

            if let Some(driver) = driver {
                if let Some(version) = infer_github_install_runtime_version(checkout_dir, driver) {
                    parsed
                        .as_table_mut()
                        .expect("normalized GitHub install draft must stay a table")
                        .insert("runtime_version".to_string(), toml::Value::String(version));
                }
            }
        }

        if runtime.as_deref() == Some("source/node") {
            normalize_v03_source_node_typescript_run(&mut parsed, checkout_dir)?;
        }

        changed_pack_include_from_checkout(&mut parsed, checkout_dir)?;
        inspect_normalized_github_install_preview_manifest(&parsed, checkout_dir)?;

        return toml::to_string(&parsed)
            .context("Failed to serialize normalized GitHub install draft");
    }

    let Some(targets) = parsed
        .get_mut("targets")
        .and_then(toml::Value::as_table_mut)
    else {
        return Ok(manifest_text.to_string());
    };

    let mut changed = false;
    for (_, target_value) in targets.iter_mut() {
        let Some(target) = target_value.as_table_mut() else {
            continue;
        };
        let runtime = target
            .get("runtime")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if runtime != "source" && runtime != "web" {
            continue;
        }

        let current_driver: Option<String> = target
            .get("driver")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());

        let Some(current_driver) = current_driver else {
            continue;
        };

        let normalized_driver = normalize_github_install_driver(&current_driver);
        if normalized_driver != current_driver {
            target.insert(
                "driver".to_string(),
                toml::Value::String(normalized_driver.clone()),
            );
            changed = true;
        }

        if runtime == "source"
            && matches!(normalized_driver.as_str(), "node" | "python" | "deno")
            && target
                .get("runtime_version")
                .and_then(toml::Value::as_str)
                .map(|value| value.trim().is_empty())
                .unwrap_or(true)
        {
            if let Some(version) =
                infer_github_install_runtime_version(checkout_dir, normalized_driver.as_str())
            {
                target.insert("runtime_version".to_string(), toml::Value::String(version));
                changed = true;
            }
        }
    }

    inspect_normalized_github_install_preview_manifest(&parsed, checkout_dir)?;

    if !changed {
        return Ok(manifest_text.to_string());
    }

    toml::to_string(&parsed).context("Failed to serialize normalized GitHub install draft")
}

pub(super) fn auto_fix_github_install_preview_toml(manifest_text: &str) -> Result<String> {
    rewrite_github_install_preview_toml_port(manifest_text, false)
}

pub(super) fn reassign_github_install_preview_toml_port(manifest_text: &str) -> Result<String> {
    rewrite_github_install_preview_toml_port(manifest_text, true)
}

fn rewrite_github_install_preview_toml_port(
    manifest_text: &str,
    force_port_reassignment: bool,
) -> Result<String> {
    let Ok(mut parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return Ok(manifest_text.to_string());
    };

    let changed = if parsed
        .get("schema_version")
        .and_then(toml::Value::as_str)
        .map(|value| value.trim() == "0.3")
        .unwrap_or(false)
        && parsed.get("targets").is_none()
    {
        let table = parsed
            .as_table_mut()
            .expect("normalized GitHub install draft must stay a table");
        apply_dynamic_web_port_to_table(table, force_port_reassignment)?
    } else {
        let Some(targets) = parsed
            .get_mut("targets")
            .and_then(toml::Value::as_table_mut)
        else {
            return Ok(manifest_text.to_string());
        };

        let mut changed = false;
        for (_, target_value) in targets.iter_mut() {
            let Some(target) = target_value.as_table_mut() else {
                continue;
            };
            changed |= apply_dynamic_web_port_to_table(target, force_port_reassignment)?;
        }
        changed
    };

    if !changed {
        return Ok(manifest_text.to_string());
    }

    toml::to_string(&parsed).context("Failed to serialize auto-fixed GitHub install draft")
}

fn apply_dynamic_web_port_to_table(
    table: &mut toml::value::Table,
    force_port_reassignment: bool,
) -> Result<bool> {
    if !table_runtime_requires_web_port(table) {
        return Ok(false);
    }

    if table.contains_key("port") && !force_port_reassignment {
        return Ok(false);
    }

    let port = allocate_github_auto_fix_port()?;
    table.insert("port".to_string(), toml::Value::Integer(i64::from(port)));
    Ok(true)
}

fn table_runtime_requires_web_port(table: &toml::value::Table) -> bool {
    table
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value.eq_ignore_ascii_case("web") || value.to_ascii_lowercase().starts_with("web/")
        })
        .unwrap_or(false)
}

fn allocate_github_auto_fix_port() -> Result<u16> {
    let (range_start, range_end) = github_auto_fix_port_range()?;
    let span = range_end - range_start + 1;
    let start_offset = rand::thread_rng().gen_range(0..usize::from(span));

    for step in 0..usize::from(span) {
        let offset = (start_offset + step) % usize::from(span);
        let port = range_start + offset as u16;
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    bail!(
        "No available Ato auto-fix port in range {}-{}",
        range_start,
        range_end
    );
}

fn github_auto_fix_port_range() -> Result<(u16, u16)> {
    let range_start = std::env::var(ENV_GITHUB_AUTO_FIX_PORT_RANGE_START)
        .ok()
        .map(|value| parse_auto_fix_port_bound(ENV_GITHUB_AUTO_FIX_PORT_RANGE_START, &value))
        .transpose()?
        .unwrap_or(DEFAULT_GITHUB_AUTO_FIX_PORT_RANGE_START);
    let range_end = std::env::var(ENV_GITHUB_AUTO_FIX_PORT_RANGE_END)
        .ok()
        .map(|value| parse_auto_fix_port_bound(ENV_GITHUB_AUTO_FIX_PORT_RANGE_END, &value))
        .transpose()?
        .unwrap_or(DEFAULT_GITHUB_AUTO_FIX_PORT_RANGE_END);

    if range_start > range_end {
        bail!(
            "{} must be less than or equal to {}",
            ENV_GITHUB_AUTO_FIX_PORT_RANGE_START,
            ENV_GITHUB_AUTO_FIX_PORT_RANGE_END
        );
    }

    Ok((range_start, range_end))
}

fn parse_auto_fix_port_bound(name: &str, value: &str) -> Result<u16> {
    value
        .trim()
        .parse::<u16>()
        .with_context(|| format!("Failed to parse {} as a TCP port", name))
}

fn normalize_v03_source_node_typescript_run(
    parsed: &mut toml::Value,
    checkout_dir: &Path,
) -> Result<()> {
    let Some(run) = parsed
        .get("run")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };

    let run_parts = run.split_whitespace().collect::<Vec<_>>();
    if run_parts.len() < 2 || run_parts[0] != "node" || !run_parts[1].ends_with(".ts") {
        return Ok(());
    }

    let Some(package_json) = read_package_json(checkout_dir) else {
        return Ok(());
    };
    let Some(build_script) = package_json
        .get("scripts")
        .and_then(|value| value.get("build"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let Some(bin_path) = package_json_primary_bin_path(&package_json) else {
        return Ok(());
    };
    if !bin_path.ends_with(".js") {
        return Ok(());
    }

    let package_manager = infer_node_package_manager_command_prefix(checkout_dir, &package_json);
    let build_command = normalize_package_script_command(package_manager, "build", build_script);
    let trailing_args = run_parts
        .iter()
        .skip(2)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let run_command = if trailing_args.is_empty() {
        format!("node {bin_path}")
    } else {
        format!("node {bin_path} {trailing_args}")
    };

    let mut include_entries = vec![recursive_parent_include(&bin_path)];
    if let Some(files) = package_json
        .get("files")
        .and_then(serde_json::Value::as_array)
    {
        for value in files {
            let Some(path) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let normalized = path.trim_start_matches("./");
            if normalized.is_empty() {
                continue;
            }
            let candidate = checkout_dir.join(normalized);
            let include = if candidate.is_dir() {
                format!("{}/**", normalized.trim_end_matches('/'))
            } else {
                normalized.to_string()
            };
            include_entries.push(include);
        }
    }

    let Some(table) = parsed.as_table_mut() else {
        return Ok(());
    };

    table.insert("build".to_string(), toml::Value::String(build_command));
    table.insert("run".to_string(), toml::Value::String(run_command));
    for entry in include_entries {
        ensure_pack_include_entry_in_table(table, entry);
    }
    Ok(())
}

fn ensure_pack_include_entry_in_table(table: &mut toml::value::Table, entry: String) {
    if entry.trim().is_empty() {
        return;
    }

    let pack = table
        .entry("pack".to_string())
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
    let Some(pack_table) = pack.as_table_mut() else {
        return;
    };
    let include = pack_table
        .entry("include".to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let Some(include_array) = include.as_array_mut() else {
        return;
    };

    let already_present = include_array.iter().any(|value| {
        value
            .as_str()
            .map(|existing| existing.trim() == entry)
            .unwrap_or(false)
    });
    if !already_present {
        include_array.push(toml::Value::String(entry));
    }
}

fn recursive_parent_include(path: &str) -> String {
    let trimmed = path.trim().trim_start_matches("./");
    let parent = Path::new(trimmed)
        .parent()
        .map(normalize_relative_path)
        .filter(|value| !value.is_empty());

    match parent {
        Some(parent) => format!("{parent}/**"),
        None => trimmed.to_string(),
    }
}

fn read_package_json(checkout_dir: &Path) -> Option<serde_json::Value> {
    let package_json_path = checkout_dir.join("package.json");
    let raw = std::fs::read_to_string(package_json_path).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw).ok()
}

fn package_json_primary_bin_path(package_json: &serde_json::Value) -> Option<String> {
    if let Some(bin) = package_json.get("bin") {
        if let Some(path) = bin.as_str() {
            let normalized = path.trim().trim_start_matches("./");
            if !normalized.is_empty() {
                return Some(normalized.to_string());
            }
        }
        if let Some(table) = bin.as_object() {
            for value in table.values() {
                if let Some(path) = value.as_str() {
                    let normalized = path.trim().trim_start_matches("./");
                    if !normalized.is_empty() {
                        return Some(normalized.to_string());
                    }
                }
            }
        }
    }
    None
}

fn infer_node_package_manager_command_prefix(
    checkout_dir: &Path,
    package_json: &serde_json::Value,
) -> &'static str {
    let declared_pm = package_json
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();

    if checkout_dir.join("pnpm-lock.yaml").exists() || declared_pm.starts_with("pnpm@") {
        return "pnpm";
    }
    if checkout_dir.join("yarn.lock").exists() || declared_pm.starts_with("yarn@") {
        return "yarn";
    }
    if checkout_dir.join("bun.lock").exists()
        || checkout_dir.join("bun.lockb").exists()
        || declared_pm.starts_with("bun@")
    {
        return "bun";
    }
    if checkout_dir.join("package-lock.json").exists() {
        return "npm";
    }
    "npm"
}

fn normalize_package_script_command(
    package_manager: &str,
    script_name: &str,
    script_body: &str,
) -> String {
    let trimmed = script_body.trim();
    if trimmed == format!("{package_manager} {script_name}")
        || trimmed == format!("{package_manager} run {script_name}")
    {
        return trimmed.to_string();
    }

    match package_manager {
        "pnpm" | "npm" => format!("{package_manager} run {script_name}"),
        "bun" => format!("bun run {script_name}"),
        _ => format!("{package_manager} run {script_name}"),
    }
}

#[derive(Debug)]
struct GitHubInstallPreviewTargetInspection {
    label: String,
    runtime: String,
    driver: String,
    working_dir: Option<String>,
}

fn inspect_normalized_github_install_preview_manifest(
    parsed: &toml::Value,
    checkout_dir: &Path,
) -> Result<()> {
    let manifest_dir = checkout_dir;
    let pack_include = parsed
        .get("pack")
        .and_then(|pack| pack.get("include"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for target in github_install_preview_targets_for_inspection(parsed) {
        let execution_working_directory = target
            .working_dir
            .as_deref()
            .map(|relative| checkout_dir.join(relative))
            .unwrap_or_else(|| checkout_dir.to_path_buf());
        let lockfile_check_paths =
            github_install_lockfile_checks(&target.driver, &execution_working_directory);
        debug!(
            checkout_dir = %checkout_dir.display(),
            manifest_dir = %manifest_dir.display(),
            execution_working_directory = %execution_working_directory.display(),
            target = %target.label,
            runtime = %target.runtime,
            driver = %target.driver,
            lockfile_check_paths = ?lockfile_check_paths,
            pack_include = ?pack_include,
            "GitHub install preview path diagnostics"
        );

        if let Some((_, missing_path, _)) = lockfile_check_paths.iter().find(|(_, path, exists)| {
            *exists
                && path
                    .strip_prefix(checkout_dir)
                    .ok()
                    .map(normalize_relative_path)
                    .map(|relative| !pack_include_covers_path(&pack_include, &relative))
                    .unwrap_or(false)
        }) {
            let relative = normalize_relative_path(
                missing_path
                    .strip_prefix(checkout_dir)
                    .unwrap_or(missing_path.as_path()),
            );
            bail!(
                "GitHub install preview manifest is inconsistent: target '{}' runs from '{}' but pack.include does not cover required lockfile '{}'",
                target.label,
                execution_working_directory.display(),
                relative,
            );
        }
    }

    Ok(())
}

fn github_install_preview_targets_for_inspection(
    parsed: &toml::Value,
) -> Vec<GitHubInstallPreviewTargetInspection> {
    if let Some(targets) = parsed.get("targets").and_then(toml::Value::as_table) {
        return targets
            .iter()
            .filter_map(|(label, value)| {
                let target = value.as_table()?;
                let runtime = target.get("runtime")?.as_str()?.trim().to_string();
                let driver = target
                    .get("driver")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| infer_driver_from_runtime(&runtime));
                Some(GitHubInstallPreviewTargetInspection {
                    label: label.to_string(),
                    runtime,
                    driver,
                    working_dir: target
                        .get("working_dir")
                        .and_then(toml::Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string),
                })
            })
            .collect();
    }

    parsed
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|runtime| {
            vec![GitHubInstallPreviewTargetInspection {
                label: parsed
                    .get("default_target")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("app")
                    .to_string(),
                runtime: runtime.to_string(),
                driver: parsed
                    .get("driver")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| infer_driver_from_runtime(runtime)),
                working_dir: parsed
                    .get("working_dir")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            }]
        })
        .unwrap_or_default()
}

fn infer_driver_from_runtime(runtime: &str) -> String {
    runtime
        .split('/')
        .nth(1)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

fn github_install_lockfile_checks(
    driver: &str,
    execution_working_directory: &Path,
) -> Vec<(&'static str, PathBuf, bool)> {
    match driver.trim().to_ascii_lowercase().as_str() {
        "node" => {
            let package_lock = execution_working_directory.join("package-lock.json");
            let yarn_lock = execution_working_directory.join("yarn.lock");
            let pnpm_lock = execution_working_directory.join("pnpm-lock.yaml");
            let bun_lock = execution_working_directory.join("bun.lock");
            let bun_lockb = execution_working_directory.join("bun.lockb");
            vec![
                (
                    "package-lock.json",
                    package_lock.clone(),
                    package_lock.exists(),
                ),
                ("yarn.lock", yarn_lock.clone(), yarn_lock.exists()),
                ("pnpm-lock.yaml", pnpm_lock.clone(), pnpm_lock.exists()),
                ("bun.lock", bun_lock.clone(), bun_lock.exists()),
                ("bun.lockb", bun_lockb.clone(), bun_lockb.exists()),
            ]
        }
        "python" => {
            let uv_lock = execution_working_directory.join("uv.lock");
            vec![("uv.lock", uv_lock.clone(), uv_lock.exists())]
        }
        "native" => {
            let cargo_lock = execution_working_directory.join("Cargo.lock");
            vec![("Cargo.lock", cargo_lock.clone(), cargo_lock.exists())]
        }
        _ => Vec::new(),
    }
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::CurDir => None,
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn pack_include_covers_path(pack_include: &[String], relative_path: &str) -> bool {
    pack_include
        .iter()
        .any(|pattern| pack_include_pattern_matches(pattern, relative_path))
}

fn pack_include_pattern_matches(pattern: &str, relative_path: &str) -> bool {
    let normalized_pattern = pattern.trim().trim_start_matches("./").replace('\\', "/");
    if normalized_pattern.is_empty() {
        return false;
    }
    if normalized_pattern == relative_path
        || normalized_pattern == "**"
        || normalized_pattern == "*"
    {
        return true;
    }
    if let Some(prefix) = normalized_pattern.strip_suffix("/**") {
        return relative_path == prefix || relative_path.starts_with(&format!("{prefix}/"));
    }
    if !normalized_pattern.contains('*') && !normalized_pattern.contains('?') {
        return false;
    }

    let mut regex_source = String::from("^");
    let chars = normalized_pattern.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '*' if chars.get(index + 1) == Some(&'*') => {
                regex_source.push_str(".*");
                index += 2;
            }
            '*' => {
                regex_source.push_str("[^/]*");
                index += 1;
            }
            '?' => {
                regex_source.push_str("[^/]");
                index += 1;
            }
            ch => {
                regex_source.push_str(&regex::escape(&ch.to_string()));
                index += 1;
            }
        }
    }
    regex_source.push('$');

    Regex::new(&regex_source)
        .map(|regex| regex.is_match(relative_path))
        .unwrap_or(false)
}

fn changed_pack_include_from_checkout(parsed: &mut toml::Value, checkout_dir: &Path) -> Result<()> {
    let Some(pack) = parsed.get_mut("pack").and_then(toml::Value::as_table_mut) else {
        return Ok(());
    };
    let Some(include) = pack.get_mut("include").and_then(toml::Value::as_array_mut) else {
        return Ok(());
    };

    if let Some(import_map) = referenced_deno_import_map(checkout_dir)? {
        let already_present = include.iter().any(|entry| {
            entry
                .as_str()
                .map(|value| value.trim() == import_map)
                .unwrap_or(false)
        });
        if !already_present {
            include.push(toml::Value::String(import_map));
        }
    }

    Ok(())
}

fn referenced_deno_import_map(checkout_dir: &Path) -> Result<Option<String>> {
    let deno_json_path = checkout_dir.join("deno.json");
    if !deno_json_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&deno_json_path)
        .with_context(|| format!("Failed to read {}", deno_json_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", deno_json_path.display()))?;
    let Some(import_map) = parsed
        .get("importMap")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let normalized = import_map.trim_start_matches("./");
    if normalized.is_empty() {
        return Ok(None);
    }
    if checkout_dir.join(normalized).exists() {
        return Ok(Some(normalized.to_string()));
    }

    Ok(None)
}

fn normalize_github_install_driver(driver: &str) -> String {
    match driver.trim().to_ascii_lowercase().as_str() {
        "pip" | "poetry" | "uv" => "python".to_string(),
        "npm" | "pnpm" | "yarn" | "bun" | "nodejs" => "node".to_string(),
        "cargo" | "go" => "native".to_string(),
        other => other.to_string(),
    }
}

fn infer_github_install_runtime_version(checkout_dir: &Path, driver: &str) -> Option<String> {
    match driver {
        "node" => Some(infer_node_runtime_version_for_github_install(checkout_dir)),
        "python" => Some(infer_python_runtime_version_for_github_install(
            checkout_dir,
        )),
        "deno" => None,
        _ => None,
    }
}

fn infer_node_runtime_version_for_github_install(checkout_dir: &Path) -> String {
    let package_json_path = checkout_dir.join("package.json");
    let raw = std::fs::read_to_string(&package_json_path).ok();
    let engine = raw
        .as_deref()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .and_then(|json| {
            json.get("engines")
                .and_then(|engines| engines.get("node"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });

    let Some(engine) = engine else {
        return DEFAULT_GITHUB_DRAFT_NODE_RUNTIME_VERSION.to_string();
    };

    let major = first_numeric_version_component(&engine)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0);
    let Some(major) = major else {
        return DEFAULT_GITHUB_DRAFT_NODE_RUNTIME_VERSION.to_string();
    };

    if major >= 22 {
        format!("{major}.0.0")
    } else if major >= 20 {
        format!("{major}.12.0")
    } else if major >= 18 {
        format!("{major}.20.0")
    } else {
        format!("{major}.0.0")
    }
}

fn infer_python_runtime_version_for_github_install(checkout_dir: &Path) -> String {
    let pyproject = std::fs::read_to_string(checkout_dir.join("pyproject.toml")).ok();
    if let Some(version) = pyproject
        .as_deref()
        .and_then(extract_pyproject_requires_python)
        .as_deref()
        .and_then(normalize_python_runtime_version_string)
    {
        return version;
    }

    let uv_lock = std::fs::read_to_string(checkout_dir.join("uv.lock")).ok();
    if let Some(version) = uv_lock
        .as_deref()
        .and_then(extract_uv_lock_requires_python)
        .as_deref()
        .and_then(normalize_python_runtime_version_string)
    {
        return version;
    }

    for path in [
        checkout_dir.join(".python-version"),
        checkout_dir.join("runtime.txt"),
    ] {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(version) = normalize_python_runtime_version_string(&raw) {
            return version;
        }
    }

    DEFAULT_GITHUB_DRAFT_PYTHON_RUNTIME_VERSION.to_string()
}

fn extract_pyproject_requires_python(raw: &str) -> Option<String> {
    if let Ok(parsed) = toml::from_str::<toml::Value>(raw) {
        if let Some(value) = parsed
            .get("project")
            .and_then(|section| section.get("requires-python"))
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }

    extract_toml_string_value(raw, "project", "requires-python")
}

fn extract_uv_lock_requires_python(raw: &str) -> Option<String> {
    if let Ok(parsed) = toml::from_str::<toml::Value>(raw) {
        if let Some(value) = parsed
            .get("options")
            .and_then(|section| section.get("requires-python"))
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }

    extract_toml_string_value(raw, "options", "requires-python")
}

fn extract_toml_string_value(raw: &str, section: &str, key: &str) -> Option<String> {
    let escaped_section = regex::escape(section);
    let escaped_key = regex::escape(key);
    let section_re = Regex::new(&format!(
        r"(?ms)^\[{escaped_section}\]\s*(.*?)(?=^\[[^\]]+\]\s*$|\z)"
    ))
    .ok()?;
    let section_body = section_re
        .captures(raw)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
        .unwrap_or(raw);
    let key_re = Regex::new(&format!(r#"(?m)^{escaped_key}\s*=\s*["']([^"'\n]+)["']"#)).ok()?;
    key_re
        .captures(section_body)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim().to_string())
}

fn first_numeric_version_component(raw: &str) -> Option<String> {
    static VERSION_RE: OnceLock<Regex> = OnceLock::new();
    VERSION_RE
        .get_or_init(|| Regex::new(r"(\d+)").expect("version regex"))
        .captures(raw)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().to_string())
}

fn normalize_runtime_version_string(raw: &str) -> Option<String> {
    static VERSION_RE: OnceLock<Regex> = OnceLock::new();
    let captures = VERSION_RE
        .get_or_init(|| Regex::new(r"(\d+)(?:\.(\d+))?(?:\.(\d+))?").expect("version regex"))
        .captures(raw)?;

    let major = captures.get(1)?.as_str();
    let minor = captures.get(2).map(|value| value.as_str()).unwrap_or("0");
    let patch = captures.get(3).map(|value| value.as_str()).unwrap_or("0");
    Some(format!("{major}.{minor}.{patch}"))
}

fn normalize_python_runtime_version_string(raw: &str) -> Option<String> {
    let normalized = normalize_runtime_version_string(raw)?;
    let mut parts = normalized.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    if major < 3 {
        return None;
    }
    Some(normalized)
}
