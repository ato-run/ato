//! v0.3/CHML manifest normalization and workspace expansion helpers.

use super::*;

fn normalize_v03_capsule_type(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

#[derive(Debug, Clone, Default, Deserialize)]
struct V03PackageSurface {
    #[serde(rename = "type", default)]
    package_type: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    build: Option<String>,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    build_env: Vec<String>,
    #[serde(default)]
    run: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    required_env: Vec<String>,
    #[serde(default)]
    runtime_version: Option<String>,
    #[serde(default)]
    runtime_tools: HashMap<String, String>,
    #[serde(default)]
    readiness_probe: Option<toml::Value>,
    #[serde(default)]
    driver: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(default)]
    public: Vec<String>,
}

fn parse_v03_package_surface(
    package_name: &str,
    table: &Table,
) -> Result<V03PackageSurface, CapsuleError> {
    let normalized = normalize_v03_legacy_env_required(package_name, table)?;

    toml::Value::Table(normalized).try_into().map_err(|error| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 package '{}' could not be parsed: {}",
            package_name, error
        ))
    })
}

fn normalize_v03_legacy_env_required(
    package_name: &str,
    table: &Table,
) -> Result<Table, CapsuleError> {
    let Some(env_table) = table.get("env").and_then(toml::Value::as_table) else {
        return Ok(table.clone());
    };
    let Some(legacy_required) = env_table.get("required") else {
        return Ok(table.clone());
    };
    let Some(legacy_required) = legacy_required.as_array() else {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 package '{}' could not be parsed: invalid type in env.required; expected an array of strings",
            package_name,
        )));
    };

    let mut normalized = table.clone();
    let mut merged_required_env = normalized
        .get("required_env")
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        CapsuleError::ParseError(format!(
                            "schema_version=0.3 package '{}' could not be parsed: invalid type in required_env; expected an array of strings",
                            package_name,
                        ))
                    })
                })
                .collect::<Result<Vec<_>, CapsuleError>>()
        })
        .transpose()?
        .unwrap_or_default();

    for value in legacy_required {
        let value = value.as_str().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 package '{}' could not be parsed: invalid type in env.required; expected an array of strings",
                package_name,
            ))
        })?;

        if !merged_required_env.iter().any(|existing| existing == value) {
            merged_required_env.push(value.to_string());
        }
    }

    normalized.insert(
        "required_env".to_string(),
        toml::Value::Array(
            merged_required_env
                .into_iter()
                .map(toml::Value::String)
                .collect(),
        ),
    );

    let mut remove_env_table = false;
    if let Some(env_table) = normalized
        .get_mut("env")
        .and_then(toml::Value::as_table_mut)
    {
        env_table.remove("required");
        remove_env_table = env_table.is_empty();
    }
    if remove_env_table {
        normalized.remove("env");
    }

    Ok(normalized)
}

fn is_non_empty_legacy_value(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(s) => !s.trim().is_empty(),
        toml::Value::Array(a) => !a.is_empty(),
        _ => true,
    }
}

fn reject_v03_legacy_fields(table: &Table, context: &str) -> Result<(), CapsuleError> {
    for field in ["entrypoint", "cmd"] {
        if table
            .get(field)
            .is_some_and(is_non_empty_legacy_value)
        {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 {context} must not use legacy field '{}'; use 'run' instead",
                field
            )));
        }
    }

    if let Some(targets) = table.get("targets").and_then(toml::Value::as_table) {
        for (target_name, target_value) in targets {
            let Some(target_table) = target_value.as_table() else {
                continue;
            };
            for field in ["entrypoint", "cmd"] {
                if target_table
                    .get(field)
                    .is_some_and(is_non_empty_legacy_value)
                {
                    return Err(CapsuleError::ParseError(format!(
                        "schema_version=0.3 target '{}' must not use legacy field '{}'; use 'run' instead",
                        target_name, field
                    )));
                }
            }
        }
    }

    Ok(())
}

fn shallow_merge_v03_tables(defaults: &Table, package: &Table) -> Table {
    let mut merged = defaults.clone();
    for (key, value) in package {
        match (merged.get_mut(key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(overlay)) => {
                for (child_key, child_value) in overlay {
                    base.insert(child_key.clone(), child_value.clone());
                }
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

fn normalize_v03_runtime_selector(value: &str) -> (String, Option<String>) {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "web/static" => ("web".to_string(), Some("static".to_string())),
        "web/node" | "source/node" => ("source".to_string(), Some("node".to_string())),
        "web/deno" | "source/deno" => ("source".to_string(), Some("deno".to_string())),
        "web/python" | "source/python" => ("source".to_string(), Some("python".to_string())),
        "source/native" => ("source".to_string(), Some("native".to_string())),
        "source/go" => ("source".to_string(), Some("native".to_string())),
        "source" | "web" | "oci" | "wasm" => (normalized, None),
        other => {
            if let Some((runtime, driver)) = other.split_once('/') {
                let runtime = runtime.trim();
                let driver = driver.trim();
                let runtime = if runtime == "web" && driver != "static" {
                    "source"
                } else {
                    runtime
                };
                let driver = (!driver.is_empty()).then(|| driver.to_string());
                (runtime.to_string(), driver)
            } else {
                (other.to_string(), None)
            }
        }
    }
}

fn infer_v03_language_from_driver(driver: Option<&str>) -> Option<String> {
    match driver.map(|value| value.trim().to_ascii_lowercase()) {
        Some(driver) if matches!(driver.as_str(), "node" | "python" | "deno" | "bun") => {
            Some(driver)
        }
        _ => None,
    }
}

fn normalize_v03_web_static_entrypoint(run: &str) -> String {
    let trimmed = run.trim();
    let raw = Path::new(trimmed);
    let entrypoint = match raw.file_name() {
        Some(_) => raw.parent().unwrap_or_else(|| Path::new(".")),
        None => raw,
    };

    let normalized = entrypoint
        .components()
        .filter_map(|component| match component {
            Component::CurDir => None,
            Component::Normal(part) => Some(part.to_string_lossy().to_string()),
            _ => Some(component.as_os_str().to_string_lossy().to_string()),
        })
        .collect::<Vec<_>>()
        .join("/");

    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn apply_v03_readiness_probe(target_table: &mut Table, readiness_probe: toml::Value) {
    match readiness_probe {
        toml::Value::String(value) => {
            let mut probe = Table::new();
            probe.insert("http_get".to_string(), toml::Value::String(value.clone()));
            probe.insert("port".to_string(), toml::Value::String("PORT".to_string()));
            target_table.insert("readiness_probe".to_string(), toml::Value::Table(probe));
            target_table.insert("health_check".to_string(), toml::Value::String(value));
        }
        toml::Value::Table(probe_table) => {
            let mut normalized_probe = Table::new();
            if let Some(http_get) = probe_table
                .get("http_get")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized_probe.insert(
                    "http_get".to_string(),
                    toml::Value::String(http_get.to_string()),
                );
                target_table.insert(
                    "health_check".to_string(),
                    toml::Value::String(http_get.to_string()),
                );
            } else if probe_table
                .get("type")
                .and_then(toml::Value::as_str)
                .map(|value| value.eq_ignore_ascii_case("http"))
                .unwrap_or(false)
            {
                if let Some(target) = probe_table
                    .get("target")
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    normalized_probe.insert(
                        "http_get".to_string(),
                        toml::Value::String(target.to_string()),
                    );
                    target_table.insert(
                        "health_check".to_string(),
                        toml::Value::String(target.to_string()),
                    );
                }
            }
            if let Some(tcp_connect) = probe_table
                .get("tcp_connect")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized_probe.insert(
                    "tcp_connect".to_string(),
                    toml::Value::String(tcp_connect.to_string()),
                );
            }
            if let Some(port) = probe_table
                .get("port")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized_probe.insert("port".to_string(), toml::Value::String(port.to_string()));
            } else {
                normalized_probe
                    .insert("port".to_string(), toml::Value::String("PORT".to_string()));
            }
            if !normalized_probe.is_empty() {
                target_table.insert(
                    "readiness_probe".to_string(),
                    toml::Value::Table(normalized_probe),
                );
            }
        }
        _ => {}
    }
}

fn normalize_v03_target_table(package_name: &str, table: &Table) -> Result<Table, CapsuleError> {
    reject_v03_legacy_fields(table, &format!("package '{}'", package_name))?;
    let V03PackageSurface {
        package_type,
        runtime,
        build,
        outputs,
        build_env,
        run,
        port,
        required_env,
        runtime_version,
        runtime_tools,
        readiness_probe,
        driver,
        language,
        image,
        env,
        public,
    } = parse_v03_package_surface(package_name, table)?;
    let mut target_table = Table::new();

    if let Some(package_type) = package_type.as_deref() {
        target_table.insert(
            "package_type".to_string(),
            toml::Value::String(normalize_v03_capsule_type(package_type)),
        );
    }

    let mut normalized_driver = driver
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    if let Some(runtime_selector) = runtime
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let (runtime, driver) = normalize_v03_runtime_selector(runtime_selector);
        target_table.insert("runtime".to_string(), toml::Value::String(runtime));
        if let Some(driver) = driver {
            normalized_driver = Some(driver.clone());
            target_table.insert("driver".to_string(), toml::Value::String(driver));
        } else if let Some(driver) = normalized_driver.as_ref() {
            // runtime selector didn't contain a driver, but one was specified explicitly
            target_table.insert("driver".to_string(), toml::Value::String(driver.clone()));
        }
    } else if let Some(driver) = normalized_driver.as_ref() {
        target_table.insert("driver".to_string(), toml::Value::String(driver.clone()));
    }

    if let Some(language) = language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "language".to_string(),
            toml::Value::String(language.to_string()),
        );
    }

    if let Some(run_command) = run
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let runtime = target_table
            .get("runtime")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .map(|value| value.to_ascii_lowercase());
        let driver = target_table
            .get("driver")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .map(|value| value.to_ascii_lowercase());

        if matches!(runtime.as_deref(), Some("web")) && matches!(driver.as_deref(), Some("static"))
        {
            target_table.insert(
                "entrypoint".to_string(),
                toml::Value::String(normalize_v03_web_static_entrypoint(run_command)),
            );
        } else {
            target_table.insert(
                "run_command".to_string(),
                toml::Value::String(run_command.to_string()),
            );
            if let Some(language) = infer_v03_language_from_driver(normalized_driver.as_deref()) {
                target_table
                    .entry("language".to_string())
                    .or_insert_with(|| toml::Value::String(language));
            }
        }
    }

    if let Some(build_command) = build
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "build_command".to_string(),
            toml::Value::String(build_command.to_string()),
        );
    }

    if !outputs.is_empty() {
        target_table.insert(
            "outputs".to_string(),
            toml::Value::Array(outputs.into_iter().map(toml::Value::String).collect()),
        );
    }
    if !build_env.is_empty() {
        target_table.insert(
            "build_env".to_string(),
            toml::Value::Array(build_env.into_iter().map(toml::Value::String).collect()),
        );
    }

    if let Some(port) = port {
        target_table.insert("port".to_string(), toml::Value::Integer(i64::from(port)));
    }
    if !required_env.is_empty() {
        target_table.insert(
            "required_env".to_string(),
            toml::Value::Array(required_env.into_iter().map(toml::Value::String).collect()),
        );
    }
    if let Some(runtime_version) = runtime_version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert(
            "runtime_version".to_string(),
            toml::Value::String(runtime_version.to_string()),
        );
    }
    if !runtime_tools.is_empty() {
        target_table.insert(
            "runtime_tools".to_string(),
            toml::Value::try_from(runtime_tools).unwrap(),
        );
    }
    if let Some(readiness_probe) = readiness_probe {
        apply_v03_readiness_probe(&mut target_table, readiness_probe);
    }

    if let Some(image) = image
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_table.insert("image".to_string(), toml::Value::String(image.to_string()));
    }
    if !env.is_empty() {
        target_table.insert("env".to_string(), toml::Value::try_from(env).unwrap());
    }
    if !public.is_empty() {
        target_table.insert(
            "public".to_string(),
            toml::Value::Array(public.into_iter().map(toml::Value::String).collect()),
        );
    }

    Ok(target_table)
}

#[derive(Debug, Clone)]
struct V03WorkspaceTarget {
    label: String,
    target_table: Table,
}

#[derive(Debug, Clone, Default)]
struct V03WorkspaceContext {
    package_dirs_by_label: HashMap<String, PathBuf>,
    labels_by_relative_path: HashMap<String, String>,
}

fn seed_v03_workspace_context_labels(packages: &Table) -> V03WorkspaceContext {
    let mut context = V03WorkspaceContext::default();
    for label in packages.keys() {
        context
            .package_dirs_by_label
            .insert(label.clone(), PathBuf::new());
    }
    context
}

pub(super) fn default_external_injection_required() -> bool {
    true
}

#[derive(Debug, Clone, Default)]
struct V03NormalizedDependencies {
    workspace_dependencies: Vec<String>,
    external_dependencies: Vec<ExternalCapsuleDependency>,
}

fn normalize_workspace_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_workspace_relative_reference(
    package_name: &str,
    alias: &str,
    raw: &str,
) -> Result<String, CapsuleError> {
    let path = Path::new(raw.trim());
    if path.is_absolute() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must use a relative workspace path",
            package_name, alias
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must not escape the workspace root",
            package_name, alias
        )));
    }

    let normalized = normalize_workspace_relative_path(path);
    if normalized.is_empty() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must reference a workspace package or path",
            package_name, alias
        )));
    }
    Ok(normalized)
}

fn workspace_members_globset(
    manifest_path: &Path,
    members: &[String],
) -> Result<Option<GlobSet>, CapsuleError> {
    if members.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in members {
        let glob = Glob::new(pattern).map_err(|err| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 workspace.members contains invalid glob '{}': {}",
                pattern, err
            ))
        })?;
        builder.add(glob);
    }

    builder.build().map(Some).map_err(|err| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 workspace.members could not build globset for '{}': {}",
            manifest_path.display(),
            err
        ))
    })
}

fn should_prune_workspace_member_walk_entry(workspace_root: &Path, entry: &DirEntry) -> bool {
    const IGNORED_DIR_NAMES: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        ".next",
        ".nuxt",
        ".output",
        ".svelte-kit",
        ".turbo",
        ".wrangler",
        "node_modules",
        "dist",
        "build",
        "target",
        "coverage",
        ".tmp",
        "tmp",
    ];

    if !entry.file_type().is_dir() {
        return false;
    }

    let Ok(relative) = entry.path().strip_prefix(workspace_root) else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return false;
    }

    relative.components().any(|component| match component {
        Component::Normal(value) => {
            let name = value.to_string_lossy();
            name.starts_with('.') || IGNORED_DIR_NAMES.contains(&name.as_ref())
        }
        _ => false,
    })
}

fn discover_v03_workspace_member_dirs(
    manifest_path: &Path,
    workspace_members: &[String],
) -> Result<HashMap<String, PathBuf>, CapsuleError> {
    let Some(globset) = workspace_members_globset(manifest_path, workspace_members)? else {
        return Ok(HashMap::new());
    };
    let workspace_root = manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            manifest_path.display()
        ))
    })?;

    let mut discovered = HashMap::new();
    for entry in WalkDir::new(workspace_root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| !should_prune_workspace_member_walk_entry(workspace_root, entry))
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_dir())
    {
        let dir = entry.into_path();
        if !dir.join("capsule.toml").exists() {
            continue;
        }

        let relative = dir.strip_prefix(workspace_root).map_err(|_| {
            CapsuleError::ParseError(format!(
                "workspace member '{}' must stay inside '{}'",
                dir.display(),
                workspace_root.display()
            ))
        })?;
        if !globset.is_match(relative) {
            continue;
        }

        let label = dir
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "workspace member '{}' must have a terminal directory name",
                    dir.display()
                ))
            })?
            .to_string();
        if let Some(existing) = discovered.insert(label.clone(), dir.clone()) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 workspace.members discovered duplicate package label '{}' at '{}' and '{}'",
                label,
                existing.display(),
                dir.display()
            )));
        }
    }

    Ok(discovered)
}

fn augment_v03_packages_from_members(
    manifest_path: &Path,
    packages: &mut Table,
    member_dirs: &HashMap<String, PathBuf>,
) -> Result<(), CapsuleError> {
    let workspace_root = manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            manifest_path.display()
        ))
    })?;

    for (label, member_dir) in member_dirs {
        if packages.contains_key(label) {
            continue;
        }
        let member_manifest_path = member_dir.join("capsule.toml");
        let claimed_by_explicit_package = packages.values().any(|raw_package| {
            raw_package
                .as_table()
                .and_then(|package_table| package_table.get("capsule_path"))
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .and_then(|capsule_path| {
                    manifest_path_for_capsule_path(manifest_path, capsule_path).ok()
                })
                .map(|path| path == member_manifest_path)
                .unwrap_or(false)
        });
        if claimed_by_explicit_package {
            continue;
        }

        let relative = member_dir.strip_prefix(workspace_root).map_err(|_| {
            CapsuleError::ParseError(format!(
                "workspace member '{}' must stay inside '{}'",
                member_dir.display(),
                workspace_root.display()
            ))
        })?;
        let mut package_table = Table::new();
        package_table.insert(
            "capsule_path".to_string(),
            toml::Value::String(normalize_workspace_relative_path(relative)),
        );
        packages.insert(label.clone(), toml::Value::Table(package_table));
    }

    Ok(())
}

fn build_v03_workspace_context(
    manifest_path: &Path,
    packages: &Table,
    member_dirs: &HashMap<String, PathBuf>,
) -> Result<V03WorkspaceContext, CapsuleError> {
    let workspace_root = manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            manifest_path.display()
        ))
    })?;
    let mut context = V03WorkspaceContext::default();

    for (label, raw_package) in packages {
        let package_table = raw_package.as_table().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{} must be a TOML table",
                label
            ))
        })?;

        let package_dir = if let Some(capsule_path) = package_table
            .get("capsule_path")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            manifest_path_for_capsule_path(manifest_path, capsule_path)?
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| workspace_root.to_path_buf())
        } else if let Some(member_dir) = member_dirs.get(label) {
            member_dir.clone()
        } else {
            workspace_root.to_path_buf()
        };

        let relative = package_dir.strip_prefix(workspace_root).map_err(|_| {
            CapsuleError::ParseError(format!(
                "workspace package '{}' resolved outside workspace root '{}'",
                label,
                workspace_root.display()
            ))
        })?;
        let relative = normalize_workspace_relative_path(relative);

        context
            .package_dirs_by_label
            .insert(label.clone(), package_dir.clone());
        if !relative.is_empty() {
            if let Some(existing) = context
                .labels_by_relative_path
                .insert(relative.clone(), label.clone())
            {
                if existing != *label {
                    return Err(CapsuleError::ParseError(format!(
                        "schema_version=0.3 workspace path '{}' maps to both '{}' and '{}'",
                        relative, existing, label
                    )));
                }
            }
        }
    }

    Ok(context)
}

fn normalize_v03_workspace_dependency(
    package_name: &str,
    alias: &str,
    raw_dependency: &toml::Value,
    workspace_context: &V03WorkspaceContext,
) -> Result<String, CapsuleError> {
    let dependency = raw_dependency.as_str().map(str::trim).ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must be a string",
            package_name, alias
        ))
    })?;

    if dependency.is_empty() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} must not be empty",
            package_name, alias
        )));
    }

    if let Some(target) = dependency.strip_prefix("workspace:") {
        let target = target.trim();
        if target.is_empty() {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} must reference a workspace package",
                package_name, alias
            )));
        }
        if workspace_context.package_dirs_by_label.contains_key(target) {
            return Ok(target.to_string());
        }

        let normalized_path = validate_workspace_relative_reference(package_name, alias, target)?;
        if let Some(label) = workspace_context
            .labels_by_relative_path
            .get(&normalized_path)
        {
            return Ok(label.clone());
        }

        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} references unknown workspace package/path '{}'",
            package_name, alias, target
        )));
    }

    if dependency.starts_with("capsule://") {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies.{} external capsule dependencies are not supported yet",
            package_name, alias
        )));
    }

    Err(CapsuleError::ParseError(format!(
        "schema_version=0.3 packages.{}.dependencies.{} must use workspace: references",
        package_name, alias
    )))
}

fn infer_capsule_dependency_source_type(source: &str) -> Option<&'static str> {
    let source = source.trim();
    if source.starts_with("capsule://store/") {
        Some("store")
    } else if source.starts_with("capsule://github.com/") {
        Some("github")
    } else {
        None
    }
}

fn parse_capsule_dependency_source(
    package_name: &str,
    alias: &str,
    raw_source: &str,
) -> Result<(String, BTreeMap<String, String>), CapsuleError> {
    let source = raw_source.trim();
    let (base_source, query) = source.split_once('?').unwrap_or((source, ""));
    let mut injection_bindings = BTreeMap::new();

    if !query.is_empty() {
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            if key.is_empty() || value.is_empty() {
                return Err(CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{} contains an invalid capsule injection query in '{}'",
                    package_name, alias, raw_source
                )));
            }
            if injection_bindings.insert(key.clone(), value).is_some() {
                return Err(CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{} repeats capsule injection key '{}'",
                    package_name, alias, key
                )));
            }
        }
    }

    Ok((base_source.to_string(), injection_bindings))
}

pub(super) fn is_valid_external_injection_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn normalize_external_injection_type(
    package_name: &str,
    key: &str,
    raw_type: &str,
) -> Result<String, CapsuleError> {
    let normalized = raw_type.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "file" | "directory" | "string") {
        Ok(normalized)
    } else {
        Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.external_injection.{} type '{}' is unsupported",
            package_name, key, raw_type
        )))
    }
}

fn normalize_v03_external_injection_table(
    package_name: &str,
    table: &Table,
) -> Result<Vec<(String, ExternalInjectionSpec)>, CapsuleError> {
    let Some(raw_external_injection) = table.get("external_injection") else {
        return Ok(Vec::new());
    };

    let external_injection_table = raw_external_injection.as_table().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.external_injection must be a TOML table",
            package_name
        ))
    })?;

    let mut contracts = Vec::new();
    for (key, raw_contract) in external_injection_table {
        if !is_valid_external_injection_key(key) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.external_injection key '{}' must be an uppercase shell-safe identifier",
                package_name, key
            )));
        }

        let contract = if let Some(raw_type) = raw_contract.as_str() {
            ExternalInjectionSpec {
                injection_type: normalize_external_injection_type(package_name, key, raw_type)?,
                required: true,
                default: None,
            }
        } else {
            let contract_table = raw_contract.as_table().ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.external_injection.{} must be a string or table",
                    package_name, key
                ))
            })?;
            let injection_type = contract_table
                .get("type")
                .and_then(toml::Value::as_str)
                .map(|value| normalize_external_injection_type(package_name, key, value))
                .transpose()?
                .ok_or_else(|| {
                    CapsuleError::ParseError(format!(
                        "schema_version=0.3 packages.{}.external_injection.{} must include type",
                        package_name, key
                    ))
                })?;
            let required = contract_table
                .get("required")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            let default = contract_table
                .get("default")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            ExternalInjectionSpec {
                injection_type,
                required,
                default,
            }
        };

        contracts.push((key.clone(), contract));
    }

    Ok(contracts)
}

fn normalize_v03_external_dependency(
    package_name: &str,
    alias: &str,
    raw_dependency: &toml::Value,
) -> Result<ExternalCapsuleDependency, CapsuleError> {
    let (source, source_type, injection_bindings) = if let Some(source) = raw_dependency.as_str() {
        let (source, injection_bindings) =
            parse_capsule_dependency_source(package_name, alias, source)?;
        let source_type = infer_capsule_dependency_source_type(&source).ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} uses unsupported capsule source '{}'",
                package_name, alias, source
            ))
        })?;
        (source, source_type.to_string(), injection_bindings)
    } else {
        let table = raw_dependency.as_table().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} must be a string or table",
                package_name, alias
            ))
        })?;
        let raw_source = table
            .get("source")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{} table must include non-empty source",
                    package_name, alias
                ))
            })?;
        let (source, mut injection_bindings) =
            parse_capsule_dependency_source(package_name, alias, raw_source)?;
        let inferred_source_type = infer_capsule_dependency_source_type(&source).ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} uses unsupported capsule source '{}'",
                package_name, alias, source
            ))
        })?;
        let source_type = table
            .get("source_type")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(inferred_source_type)
            .to_ascii_lowercase();
        if source_type != inferred_source_type {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{}.dependencies.{} source_type '{}' does not match source '{}'",
                package_name, alias, source_type, source
            )));
        }

        if let Some(raw_bindings) = table.get("injection_bindings") {
            let binding_table = raw_bindings.as_table().ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{}.dependencies.{}.injection_bindings must be a table",
                    package_name, alias
                ))
            })?;
            for (key, value) in binding_table {
                let value = value
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        CapsuleError::ParseError(format!(
                            "schema_version=0.3 packages.{}.dependencies.{}.injection_bindings.{} must be a non-empty string",
                            package_name, alias, key
                        ))
                    })?;
                if injection_bindings
                    .insert(key.clone(), value.to_string())
                    .is_some()
                {
                    return Err(CapsuleError::ParseError(format!(
                        "schema_version=0.3 packages.{}.dependencies.{} repeats capsule injection key '{}'",
                        package_name, alias, key
                    )));
                }
            }
        }

        (source, source_type, injection_bindings)
    };

    Ok(ExternalCapsuleDependency {
        alias: alias.to_string(),
        source,
        source_type,
        injection_bindings,
    })
}

fn normalize_v03_package_dependencies(
    package_name: &str,
    table: &Table,
    workspace_context: &V03WorkspaceContext,
) -> Result<V03NormalizedDependencies, CapsuleError> {
    let Some(raw_dependencies) = table.get("dependencies") else {
        return Ok(V03NormalizedDependencies::default());
    };

    let dependencies_table = raw_dependencies.as_table().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "schema_version=0.3 packages.{}.dependencies must be a TOML table",
            package_name
        ))
    })?;

    let mut dependencies = V03NormalizedDependencies::default();
    let mut seen_workspace = HashSet::new();
    let mut seen_external = HashSet::new();
    for (alias, raw_dependency) in dependencies_table {
        let external_source = raw_dependency
            .as_str()
            .map(str::trim)
            .filter(|value| value.starts_with("capsule://"))
            .map(str::to_string)
            .or_else(|| {
                raw_dependency
                    .as_table()
                    .and_then(|table| table.get("source"))
                    .and_then(toml::Value::as_str)
                    .map(str::trim)
                    .filter(|value| value.starts_with("capsule://"))
                    .map(str::to_string)
            });

        if external_source.is_some() {
            let dependency =
                normalize_v03_external_dependency(package_name, alias, raw_dependency)?;
            if seen_external.insert((dependency.alias.clone(), dependency.source.clone())) {
                dependencies.external_dependencies.push(dependency);
            }
            continue;
        }

        let dependency = normalize_v03_workspace_dependency(
            package_name,
            alias,
            raw_dependency,
            workspace_context,
        )?;
        if seen_workspace.insert(dependency.clone()) {
            dependencies.workspace_dependencies.push(dependency);
        }
    }
    Ok(dependencies)
}

fn manifest_path_for_capsule_path(
    base_manifest_path: &Path,
    capsule_path: &str,
) -> Result<PathBuf, CapsuleError> {
    let base_dir = base_manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            base_manifest_path.display()
        ))
    })?;
    let candidate = base_dir.join(capsule_path);
    let manifest_path = if candidate.is_dir() {
        candidate.join("capsule.toml")
    } else {
        candidate
    };

    if !manifest_path.exists() {
        return Err(CapsuleError::ParseError(format!(
            "schema_version=0.3 capsule_path '{}' does not exist",
            manifest_path.display()
        )));
    }
    Ok(manifest_path)
}

fn relative_package_working_dir(
    root_manifest_path: &Path,
    package_dir: &Path,
) -> Result<Option<String>, CapsuleError> {
    let root_dir = root_manifest_path.parent().ok_or_else(|| {
        CapsuleError::ParseError(format!(
            "manifest path '{}' has no parent directory",
            root_manifest_path.display()
        ))
    })?;

    if package_dir == root_dir {
        return Ok(None);
    }

    let relative = package_dir.strip_prefix(root_dir).map_err(|_| {
        CapsuleError::ParseError(format!(
            "delegated package directory '{}' must stay inside workspace root '{}'",
            package_dir.display(),
            root_dir.display()
        ))
    })?;

    Ok(Some(relative.to_string_lossy().replace('\\', "/")))
}

fn normalize_workspace_target_from_package(
    root_manifest_path: Option<&Path>,
    package_name: &str,
    package_table: &Table,
    package_dir: Option<&Path>,
    workspace_context: &V03WorkspaceContext,
) -> Result<V03WorkspaceTarget, CapsuleError> {
    let mut target_table = normalize_v03_target_table(package_name, package_table)?;
    let dependencies =
        normalize_v03_package_dependencies(package_name, package_table, workspace_context)?;
    let working_dir = match (root_manifest_path, package_dir) {
        (Some(root_manifest_path), Some(package_dir)) => {
            relative_package_working_dir(root_manifest_path, package_dir)?
        }
        _ => None,
    };

    if let Some(working_dir) = working_dir.as_ref() {
        target_table.insert(
            "working_dir".to_string(),
            toml::Value::String(working_dir.clone()),
        );
    }
    let external_injection = normalize_v03_external_injection_table(package_name, package_table)?;
    if !external_injection.is_empty() {
        let mut table = Table::new();
        for (key, contract) in external_injection {
            table.insert(key, toml::Value::try_from(contract).unwrap());
        }
        target_table.insert("external_injection".to_string(), toml::Value::Table(table));
    }
    if !dependencies.workspace_dependencies.is_empty() {
        target_table.insert(
            "package_dependencies".to_string(),
            toml::Value::Array(
                dependencies
                    .workspace_dependencies
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            ),
        );
    }
    if !dependencies.external_dependencies.is_empty() {
        target_table.insert(
            "external_dependencies".to_string(),
            toml::Value::Array(
                dependencies
                    .external_dependencies
                    .iter()
                    .cloned()
                    .map(|dependency| toml::Value::try_from(dependency).unwrap())
                    .collect(),
            ),
        );
    }

    Ok(V03WorkspaceTarget {
        label: package_name.to_string(),
        target_table,
    })
}

fn normalize_v03_single_manifest_target(
    mut table: Table,
    root_manifest_path: Option<&Path>,
    package_manifest_path: Option<&Path>,
    explicit_label: Option<&str>,
    workspace_context: &V03WorkspaceContext,
) -> Result<V03WorkspaceTarget, CapsuleError> {
    let package_name = explicit_label.map(str::to_string).unwrap_or_else(|| {
        table
            .get("default_target")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("app")
            .to_string()
    });

    if let Some(capsule_type) = table.get("type").and_then(toml::Value::as_str) {
        table.insert(
            "type".to_string(),
            toml::Value::String(normalize_v03_capsule_type(capsule_type)),
        );
    }

    normalize_workspace_target_from_package(
        root_manifest_path,
        &package_name,
        &table,
        package_manifest_path.and_then(Path::parent),
        workspace_context,
    )
}

fn normalize_legacy_manifest_as_workspace_target(
    root_manifest_path: &Path,
    manifest_path: &Path,
    package_name: &str,
) -> Result<V03WorkspaceTarget, CapsuleError> {
    let manifest = CapsuleManifest::load_from_file(manifest_path)?;
    let target = manifest.resolve_default_target()?;
    let mut target_table = toml::Value::try_from(target.clone())
        .map_err(|err| CapsuleError::SerializeError(err.to_string()))?
        .as_table()
        .cloned()
        .ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "legacy delegated manifest '{}' did not normalize to a target table",
                manifest_path.display()
            ))
        })?;

    let working_dir = relative_package_working_dir(
        root_manifest_path,
        manifest_path.parent().unwrap_or(Path::new(".")),
    )?;
    if let Some(working_dir) = working_dir.as_ref() {
        target_table.insert(
            "working_dir".to_string(),
            toml::Value::String(working_dir.clone()),
        );
    }

    Ok(V03WorkspaceTarget {
        label: package_name.to_string(),
        target_table,
    })
}

fn normalize_v03_workspace_targets(
    root_manifest_path: Option<&Path>,
    current_manifest_path: Option<&Path>,
    workspace_defaults: &Table,
    packages: &Table,
    workspace_context: &V03WorkspaceContext,
    visiting: &mut HashSet<PathBuf>,
) -> Result<Vec<V03WorkspaceTarget>, CapsuleError> {
    let mut targets = Vec::new();

    for (package_name, raw_package) in packages {
        let package_table = raw_package.as_table().cloned().ok_or_else(|| {
            CapsuleError::ParseError(format!(
                "schema_version=0.3 packages.{} must be a TOML table",
                package_name
            ))
        })?;

        if let Some(capsule_path) = package_table
            .get("capsule_path")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let current_manifest_path = current_manifest_path.ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "schema_version=0.3 packages.{} capsule_path requires loading from a file path",
                    package_name
                ))
            })?;
            let delegated_manifest_path =
                manifest_path_for_capsule_path(current_manifest_path, capsule_path)?;
            let delegated_canonical = delegated_manifest_path
                .canonicalize()
                .unwrap_or_else(|_| delegated_manifest_path.clone());
            if !visiting.insert(delegated_canonical.clone()) {
                return Err(CapsuleError::ParseError(format!(
                    "circular capsule_path delegation detected at '{}'",
                    delegated_canonical.display()
                )));
            }

            let delegated_text = fs::read_to_string(&delegated_manifest_path).map_err(|err| {
                CapsuleError::ParseError(format!(
                    "failed to read delegated manifest '{}': {}",
                    delegated_manifest_path.display(),
                    err
                ))
            })?;
            let delegated_raw: toml::Value = toml::from_str(&delegated_text).map_err(|err| {
                CapsuleError::ParseError(format!(
                    "failed to parse delegated manifest '{}': {}",
                    delegated_manifest_path.display(),
                    err
                ))
            })?;
            let delegated_table = delegated_raw.as_table().cloned().ok_or_else(|| {
                CapsuleError::ParseError(format!(
                    "delegated manifest '{}' must be a TOML table",
                    delegated_manifest_path.display()
                ))
            })?;

            let delegated_targets = if is_v03_schema(&delegated_raw) {
                if delegated_table.contains_key("packages") {
                    let delegated_defaults = delegated_table
                        .get("workspace")
                        .and_then(|workspace| workspace.get("defaults"))
                        .and_then(toml::Value::as_table)
                        .cloned()
                        .unwrap_or_default();
                    let delegated_packages = delegated_table
                        .get("packages")
                        .and_then(toml::Value::as_table)
                        .cloned()
                        .ok_or_else(|| {
                            CapsuleError::ParseError(format!(
                                "schema_version=0.3 delegated manifest '{}' packages must be a TOML table",
                                delegated_manifest_path.display()
                            ))
                        })?;
                    let mut delegated_workspace_context = workspace_context.clone();
                    let seeded_context = seed_v03_workspace_context_labels(&delegated_packages);
                    for (label, package_dir) in seeded_context.package_dirs_by_label {
                        delegated_workspace_context
                            .package_dirs_by_label
                            .entry(label)
                            .or_insert(package_dir);
                    }
                    normalize_v03_workspace_targets(
                        root_manifest_path,
                        Some(&delegated_manifest_path),
                        &delegated_defaults,
                        &delegated_packages,
                        &delegated_workspace_context,
                        visiting,
                    )?
                } else {
                    vec![normalize_v03_single_manifest_target(
                        delegated_table,
                        root_manifest_path,
                        Some(&delegated_manifest_path),
                        Some(package_name),
                        workspace_context,
                    )?]
                }
            } else {
                vec![normalize_legacy_manifest_as_workspace_target(
                    root_manifest_path.ok_or_else(|| {
                        CapsuleError::ParseError(format!(
                            "schema_version=0.3 packages.{} capsule_path requires a workspace root path",
                            package_name
                        ))
                    })?,
                    &delegated_manifest_path,
                    package_name,
                )?]
            };
            visiting.remove(&delegated_canonical);
            targets.extend(delegated_targets);
            continue;
        }

        let merged = shallow_merge_v03_tables(workspace_defaults, &package_table);
        let package_dir = workspace_context
            .package_dirs_by_label
            .get(package_name)
            .filter(|path| !path.as_os_str().is_empty())
            .map(PathBuf::as_path);
        targets.push(normalize_workspace_target_from_package(
            root_manifest_path,
            package_name,
            &merged,
            package_dir.or_else(|| current_manifest_path.and_then(Path::parent)),
            workspace_context,
        )?);
    }

    Ok(targets)
}

fn normalize_v03_workspace_manifest_with_path(
    mut table: Table,
    manifest_path: Option<&Path>,
    visiting: &mut HashSet<PathBuf>,
) -> Result<toml::Value, CapsuleError> {
    let workspace_config = table
        .get("workspace")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let workspace_defaults = workspace_config
        .get("defaults")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let workspace_members = workspace_config
        .get("members")
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
    let mut packages = table
        .remove("packages")
        .and_then(|value| value.as_table().cloned())
        .ok_or_else(|| {
            CapsuleError::ParseError("schema_version=0.3 packages must be a TOML table".to_string())
        })?;
    let member_dirs = manifest_path
        .map(|path| discover_v03_workspace_member_dirs(path, &workspace_members))
        .transpose()?
        .unwrap_or_default();
    if let Some(path) = manifest_path {
        augment_v03_packages_from_members(path, &mut packages, &member_dirs)?;
    }
    let workspace_context = manifest_path
        .map(|path| build_v03_workspace_context(path, &packages, &member_dirs))
        .transpose()?
        .unwrap_or_else(|| seed_v03_workspace_context_labels(&packages));

    let explicit_default_target = table
        .get("default_target")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let mut targets_table = Table::new();
    let mut first_runnable_target: Option<String> = None;
    let targets = normalize_v03_workspace_targets(
        manifest_path,
        manifest_path,
        &workspace_defaults,
        &packages,
        &workspace_context,
        visiting,
    )?;
    for target in targets {
        let target_table = target.target_table;
        if first_runnable_target.is_none()
            && target_table
                .get("package_type")
                .and_then(toml::Value::as_str)
                .map(|value| !value.eq_ignore_ascii_case("library"))
                .unwrap_or(true)
        {
            first_runnable_target = Some(target.label.clone());
        }
        if targets_table.contains_key(&target.label) {
            return Err(CapsuleError::ParseError(format!(
                "schema_version=0.3 duplicate package name '{}' after capsule_path expansion",
                target.label
            )));
        }
        targets_table.insert(target.label, toml::Value::Table(target_table));
    }

    if !table.contains_key("type") {
        table.insert("type".to_string(), toml::Value::String("app".to_string()));
    }
    table.remove("workspace");
    table.insert("targets".to_string(), toml::Value::Table(targets_table));
    table.insert(
        "default_target".to_string(),
        toml::Value::String(
            explicit_default_target
                .or(first_runnable_target)
                .unwrap_or_else(|| "app".to_string()),
        ),
    );

    Ok(toml::Value::Table(table))
}

pub(super) fn normalize_v03_manifest_value_with_path(
    raw: toml::Value,
    manifest_path: Option<&Path>,
    visiting: &mut HashSet<PathBuf>,
) -> Result<toml::Value, CapsuleError> {
    if !is_v03_like_schema(&raw) {
        return Ok(raw);
    }

    let mut table = raw.as_table().cloned().ok_or_else(|| {
        CapsuleError::ParseError("schema_version=0.3 manifest must be a TOML table".to_string())
    })?;

    if !table.contains_key("schema_version") {
        table.insert(
            "schema_version".to_string(),
            toml::Value::String("0.3".to_string()),
        );
    }

    if table.contains_key("execution") {
        return Err(CapsuleError::ParseError(
            "legacy [execution] section is not supported in schema_version=0.3".to_string(),
        ));
    }

    reject_v03_legacy_fields(&table, "manifest")?;

    if table.contains_key("packages") {
        return normalize_v03_workspace_manifest_with_path(table, manifest_path, visiting);
    }

    if let Some(capsule_type) = table.get("type").and_then(toml::Value::as_str) {
        table.insert(
            "type".to_string(),
            toml::Value::String(normalize_v03_capsule_type(capsule_type)),
        );
    }

    let has_top_level_build_command = table
        .get("build")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();

    if table
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || table
            .get("run")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        || has_top_level_build_command
        || table.contains_key("required_env")
        || table.contains_key("runtime_version")
        || table.contains_key("runtime_tools")
        || table.contains_key("port")
        || table.contains_key("readiness_probe")
        || table.contains_key("outputs")
        || table.contains_key("build_env")
    {
        let default_target = table
            .get("default_target")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("app")
            .to_string();

        let mut targets_table = table
            .remove("targets")
            .and_then(|value| value.as_table().cloned())
            .unwrap_or_default();
        let mut target_table = targets_table
            .remove(&default_target)
            .and_then(|value| value.as_table().cloned())
            .unwrap_or_default();
        let normalized_target = normalize_v03_target_table(&default_target, &table)?;
        target_table = shallow_merge_v03_tables(&target_table, &normalized_target);
        let external_injection = normalize_v03_external_injection_table(&default_target, &table)?;
        if !external_injection.is_empty() {
            let mut normalized_table = Table::new();
            for (key, contract) in external_injection {
                normalized_table.insert(key, toml::Value::try_from(contract).unwrap());
            }
            target_table.insert(
                "external_injection".to_string(),
                toml::Value::Table(normalized_table),
            );
        }
        let dependencies = normalize_v03_package_dependencies(
            &default_target,
            &table,
            &V03WorkspaceContext::default(),
        )?;
        if !dependencies.workspace_dependencies.is_empty() {
            target_table.insert(
                "package_dependencies".to_string(),
                toml::Value::Array(
                    dependencies
                        .workspace_dependencies
                        .into_iter()
                        .map(toml::Value::String)
                        .collect(),
                ),
            );
        }
        if !dependencies.external_dependencies.is_empty() {
            target_table.insert(
                "external_dependencies".to_string(),
                toml::Value::Array(
                    dependencies
                        .external_dependencies
                        .into_iter()
                        .map(|dependency| toml::Value::try_from(dependency).unwrap())
                        .collect(),
                ),
            );
        }

        targets_table.insert(default_target.clone(), toml::Value::Table(target_table));
        table.insert(
            "default_target".to_string(),
            toml::Value::String(default_target),
        );
        table.insert("targets".to_string(), toml::Value::Table(targets_table));

        let build_command = table
            .get("targets")
            .and_then(toml::Value::as_table)
            .and_then(|targets| {
                table
                    .get("default_target")
                    .and_then(toml::Value::as_str)
                    .and_then(|label| targets.get(label))
            })
            .and_then(toml::Value::as_table)
            .and_then(|target| target.get("build_command"))
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned);

        table.remove("runtime");
        table.remove("run");
        table.remove("driver");
        table.remove("language");
        table.remove("port");
        table.remove("required_env");
        table.remove("runtime_version");
        table.remove("runtime_tools");
        table.remove("readiness_probe");
        table.remove("outputs");
        table.remove("build_env");

        if let Some(build_command) = build_command {
            let mut lifecycle = Table::new();
            lifecycle.insert("build".to_string(), toml::Value::String(build_command));
            let mut build = Table::new();
            build.insert("lifecycle".to_string(), toml::Value::Table(lifecycle));
            table.insert("build".to_string(), toml::Value::Table(build));
        }
    }

    Ok(toml::Value::Table(table))
}
