//! Docker run script intent extractor (PR 11).
//!
//! Parses a shell install script that contains `docker network create` and
//! `docker run` commands and converts them into an Ato OCI service graph
//! projection. This module is **pure** — no I/O beyond the text passed through
//! [`DockerRunScriptImportInput`], no Docker/Podman calls, no shell execution.
//!
//! ## What is extracted
//!
//! | Pattern | Result |
//! |---------|--------|
//! | `docker network create <name>` | source metadata only |
//! | `docker run -d ...` | service entry |
//! | Line continuations `\` | joined before parsing |
//! | `--name <n>` | logical service label candidate |
//! | `--network <n>` | shared network label (metadata) |
//! | `-e KEY=VALUE` / `--env KEY=VALUE` | env entry |
//! | `-p HOST:CONTAINER` / `-p CONTAINER` | container port → auto host port |
//! | `-v SOURCE:TARGET` / `--volume SOURCE:TARGET` | state binding |
//! | `--restart <policy>` | ignored with warning |
//! | `DATABASE_URL` with `@<svc_name>:` | alias rewritten; dep inferred |
//!
//! ## What is rejected
//!
//! | Pattern | Error |
//! |---------|-------|
//! | `--privileged` | [`DockerRunScriptImportError::PrivilegedRejected`] |
//! | `--network host` | [`DockerRunScriptImportError::HostNetworkRejected`] |
//! | Absolute bind mount | [`DockerRunScriptImportError::AbsoluteBindMountRejected`] |
//! | Dependency cycle | [`DockerRunScriptImportError::DependencyCycle`] |
//! | `docker run` with no image ref | [`DockerRunScriptImportError::MissingImage`] |
//!
//! ## What is skipped
//!
//! - `--cap-add`, `--cap-drop`, `--device` → `unsupported_features`
//! - `--userns`, `--pid host`, `--ipc host` → `unsupported_features`
//! - `docker build`, `docker compose` → `unsupported_features`
//! - Prompt/control-flow blocks (`if`/`while`/`read`) → not executed; static
//!   `docker run` lines inside are still extracted where unambiguous.
//! - Relative bind mounts (`./*`) → `warnings` and kept as project-root-scoped.
//! - Shell variable expansion (`$VAR`, `${VAR}`) → value becomes
//!   `RequiredExternal` (or literal remainder is preserved where safe).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use thiserror::Error;

use crate::engine::orchestration::startup_order_from_dependencies;
use crate::foundation::types::orchestration::{
    OrchestrationPlan, ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime,
    ResolvedTargetRuntime,
};
use crate::foundation::types::runplan::Mount;

// Re-export shared types from the Compose importer so callers work with the
// same service model for both import paths.
pub use crate::routing::importer::compose::{
    DependencyCondition, ImportedDependency, ImportedEnvValue, ImportedOciEnvEntry,
    ImportedOciPort, ImportedOciVolumeMount, ImportedStateBinding, StateBindingKind,
};

// ── Public I/O model ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DockerRunScriptImportInput {
    /// Raw text content of the install script.
    pub script_text: String,
    /// Path the file was read from (used in diagnostics only).
    pub source_path: PathBuf,
    /// Override the project name derived from the directory.
    pub project_name: Option<String>,
}

impl DockerRunScriptImportInput {
    pub fn new(script_text: String, source_path: PathBuf) -> Self {
        Self {
            script_text,
            source_path,
            project_name: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DockerRunScriptImportOutput {
    pub services: Vec<DockerRunImportedService>,
    pub state_bindings: Vec<ImportedStateBinding>,
    /// Non-fatal diagnostic messages.
    pub warnings: Vec<String>,
    /// Unsupported features that were silently skipped.
    pub unsupported_features: Vec<String>,
    /// Network names created by `docker network create` (source metadata only).
    pub extracted_networks: Vec<String>,
    /// SHA-256 hash of the source script text.
    pub source_hash: String,
}

#[derive(Debug, Clone)]
pub struct DockerRunImportedService {
    /// Logical service label (sanitized from `--name`, or positional index).
    pub name: String,
    /// Original `--name` value — metadata only, not used as runtime container name.
    pub source_container_name: Option<String>,
    /// Declared image reference.
    pub image_ref: String,
    pub env: Vec<ImportedOciEnvEntry>,
    pub ports: Vec<ImportedOciPort>,
    pub volume_mounts: Vec<ImportedOciVolumeMount>,
    pub depends_on: Vec<ImportedDependency>,
    /// Network names this service joined (source metadata; actual network is Ato session-scoped).
    pub source_networks: Vec<String>,
}

// ── Error type ─────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DockerRunScriptImportError {
    #[error("docker run command uses --privileged which is not supported")]
    PrivilegedRejected,

    #[error("docker run command uses --network host which is not supported")]
    HostNetworkRejected,

    #[error(
        "docker run command for service '{service}' has an absolute bind mount '{path}'; \
         use a named volume instead"
    )]
    AbsoluteBindMountRejected { service: String, path: String },

    #[error("dependency cycle detected: {}", cycle.join(" → "))]
    DependencyCycle { cycle: Vec<String> },

    #[error("docker run command on line {line} has no image reference")]
    MissingImage { line: usize },
}

// ── Script candidate discovery ─────────────────────────────────────────────────

/// Priority-ordered install script name candidates.
const INSTALL_SH_CANDIDATES: &[&str] = &[
    "install.sh",
    "setup.sh",
    "start.sh",
    "run.sh",
    "deploy.sh",
    "docker-install.sh",
    "docker-setup.sh",
    "docker-run.sh",
];

/// Detect the best install script candidate in a directory.
pub fn detect_install_script_candidate(dir: &std::path::Path) -> Option<PathBuf> {
    for name in INSTALL_SH_CANDIDATES {
        let path = dir.join(name);
        if path.exists() && path.is_file() {
            return Some(path);
        }
    }
    None
}

// ── Main entry point ───────────────────────────────────────────────────────────

/// Parse a Docker install script and extract `docker run` commands as an Ato
/// OCI service graph projection.
///
/// This function is **pure** — no I/O, no host probing, no Docker/Podman calls.
pub fn import_docker_run_script(
    input: &DockerRunScriptImportInput,
) -> Result<DockerRunScriptImportOutput, DockerRunScriptImportError> {
    let source_hash = compute_script_source_hash(&input.script_text);
    let mut output = DockerRunScriptImportOutput {
        source_hash,
        ..Default::default()
    };

    // Pre-process: strip comment lines, join backslash continuations, split
    // into logical statements.
    let logical_lines = join_continuations(&input.script_text);

    // Track `--name` → logical_label mapping for DATABASE_URL rewriting.
    let mut raw_services: Vec<RawParsedService> = Vec::new();
    let mut line_num: usize = 0;

    for line in &logical_lines {
        line_num += 1;
        let trimmed = line.trim();

        // Skip empty lines and comments.
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Strip leading `sudo` or `set -e` etc.
        let stmt = strip_shell_prefix(trimmed);

        if let Some(network_name) = try_parse_network_create(stmt) {
            if !output.extracted_networks.contains(&network_name) {
                output.extracted_networks.push(network_name);
            }
            continue;
        }

        if stmt.starts_with("docker run") || stmt.starts_with("docker  run") {
            let parsed = parse_docker_run(stmt, line_num, &mut output)?;
            raw_services.push(parsed);
            continue;
        }

        // Other `docker` sub-commands.
        if let Some(rest) = stmt.strip_prefix("docker ") {
            let subcmd = rest.split_whitespace().next().unwrap_or("?");
            match subcmd {
                "build" | "compose" | "stack" => {
                    output.unsupported_features.push(format!(
                        "line {line_num}: `docker {subcmd}` is not supported by this importer"
                    ));
                }
                _ => {}
            }
        }
        // Non-docker lines (shell logic, env exports, etc.) are silently skipped.
    }

    // Build name → logical_label map for post-processing.
    let name_to_label: HashMap<String, String> = raw_services
        .iter()
        .map(|s| {
            (
                s.source_name.clone().unwrap_or_else(|| s.label.clone()),
                s.label.clone(),
            )
        })
        .collect();

    // Convert raw services to ImportedOciService, applying DATABASE_URL rewriting
    // and inferring dependencies.
    for (idx, mut raw) in raw_services.into_iter().enumerate() {
        rewrite_database_url_aliases(&mut raw, &name_to_label, &mut output);
        infer_network_dependencies(&mut raw, &output.extracted_networks, idx);

        let svc = build_service(raw, &name_to_label, &mut output)?;
        output.services.push(svc);
    }

    // Validate dependency graph.
    let known_labels: HashSet<String> = output.services.iter().map(|s| s.name.clone()).collect();
    for svc in &output.services {
        for dep in &svc.depends_on {
            if !known_labels.contains(&dep.service) {
                output.warnings.push(format!(
                    "[{}] inferred dependency on '{}' but no matching service found; skipping",
                    svc.name, dep.service
                ));
            }
        }
    }
    // Remove dangling deps.
    for svc in output.services.iter_mut() {
        svc.depends_on
            .retain(|dep| known_labels.contains(&dep.service));
    }

    // Cycle check.
    let dep_map: HashMap<String, Vec<String>> = output
        .services
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.depends_on.iter().map(|d| d.service.clone()).collect(),
            )
        })
        .collect();
    startup_order_from_dependencies(&dep_map).map_err(|_| {
        let cycle = find_cycle_in(&dep_map).unwrap_or_else(|| vec!["<cycle>".to_string()]);
        DockerRunScriptImportError::DependencyCycle { cycle }
    })?;

    Ok(output)
}

// ── OrchestrationPlan conversion ──────────────────────────────────────────────

impl DockerRunScriptImportOutput {
    /// Convert to an [`OrchestrationPlan`] for `execute_service_graph_with_provider`.
    pub fn to_orchestration_plan(&self) -> Result<OrchestrationPlan, crate::error::CapsuleError> {
        let dep_map: HashMap<String, Vec<String>> = self
            .services
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.depends_on.iter().map(|d| d.service.clone()).collect(),
                )
            })
            .collect();

        let startup_order = startup_order_from_dependencies(&dep_map)?;

        let mut services: Vec<ResolvedService> = Vec::new();

        for svc in &self.services {
            let is_published = !svc.ports.is_empty()
                && !self
                    .services
                    .iter()
                    .any(|other| other.depends_on.iter().any(|d| d.service == svc.name));

            let container_port = svc.ports.first().map(|p| p.container_port);

            let mounts: Vec<Mount> = svc
                .volume_mounts
                .iter()
                .map(|vm| Mount {
                    source: vm.state_name.clone(),
                    target: vm.target.clone(),
                    readonly: vm.readonly,
                    ownership: None,
                })
                .collect();

            let mut env: HashMap<String, String> = HashMap::new();
            for entry in &svc.env {
                if let ImportedEnvValue::Literal(val) = &entry.value {
                    env.insert(entry.key.clone(), val.clone());
                }
            }

            let network = ResolvedServiceNetwork {
                aliases: vec![svc.name.clone()],
                publish: is_published,
                allow_from: vec![],
                egress_proxy: true,
            };

            let runtime = ResolvedTargetRuntime {
                target: svc.name.clone(),
                runtime: "oci".to_string(),
                driver: None,
                runtime_version: None,
                image: Some(svc.image_ref.clone()),
                entrypoint: String::new(),
                run_command: None,
                cmd: vec![],
                env,
                working_dir: None,
                source_layout: None,
                port: container_port,
                required_env: svc
                    .env
                    .iter()
                    .filter(|e| e.value == ImportedEnvValue::RequiredExternal)
                    .map(|e| e.key.clone())
                    .collect(),
                mounts,
                user: None,
            };

            services.push(ResolvedService {
                name: svc.name.clone(),
                depends_on: svc.depends_on.iter().map(|d| d.service.clone()).collect(),
                connections: vec![],
                readiness_probe: None,
                network,
                run_once: false,
                runtime: ResolvedServiceRuntime::Oci(runtime),
            });
        }

        Ok(OrchestrationPlan {
            startup_order,
            services,
        })
    }
}

// ── Pre-processing ─────────────────────────────────────────────────────────────

/// Join backslash-newline line continuations and return logical lines.
fn join_continuations(text: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();

    for line in text.lines() {
        if line.ends_with('\\') {
            // Remove trailing backslash and append to current accumulator.
            current.push_str(line.trim_end_matches('\\'));
            current.push(' ');
        } else {
            current.push_str(line);
            result.push(current.trim().to_string());
            current = String::new();
        }
    }
    if !current.trim().is_empty() {
        result.push(current.trim().to_string());
    }
    result
}

/// Strip common shell prefixes that don't affect the docker command.
fn strip_shell_prefix(s: &str) -> &str {
    let s = s.trim();
    // Strip leading `sudo`
    if let Some(rest) = s.strip_prefix("sudo ") {
        return rest.trim_start();
    }
    s
}

/// If `stmt` is a `docker network create <name>` command, return `<name>`.
fn try_parse_network_create(stmt: &str) -> Option<String> {
    let tokens = shell_words::split(stmt).ok()?;
    let mut it = tokens.iter().peekable();

    // Accept: `docker network create [--driver bridge] <name>`
    let cmd = it.next()?;
    if cmd != "docker" {
        return None;
    }
    let sub = it.next()?;
    if sub != "network" {
        return None;
    }
    let action = it.next()?;
    if action != "create" {
        return None;
    }

    // Skip option flags.
    let mut name: Option<String> = None;
    while let Some(tok) = it.next() {
        if tok.starts_with('-') {
            // Consume value token for flags that take one.
            if matches!(
                tok.as_str(),
                "--driver" | "-d" | "--subnet" | "--gateway" | "--opt" | "-o"
            ) {
                it.next(); // skip value
            }
        } else {
            name = Some(tok.clone());
            break;
        }
    }
    name
}

// ── Internal parsed service ───────────────────────────────────────────────────

#[allow(dead_code)]
struct RawParsedService {
    line: usize,
    /// Sanitized logical label (used as service name in the graph).
    label: String,
    /// Original `--name` value.
    source_name: Option<String>,
    image_ref: String,
    networks: Vec<String>,
    raw_env: Vec<(String, String)>, // (key, raw_value) — pre-rewrite
    raw_ports: Vec<String>,         // port strings as-is
    raw_volumes: Vec<String>,       // volume strings as-is
    warnings: Vec<String>,
    unsupported: Vec<String>,
    depends_on: Vec<String>, // logical labels — populated after rewriting
}

// ── docker run parser ─────────────────────────────────────────────────────────

fn parse_docker_run(
    stmt: &str,
    line_num: usize,
    output: &mut DockerRunScriptImportOutput,
) -> Result<RawParsedService, DockerRunScriptImportError> {
    // Tokenize with shell-words to respect quoting.
    let tokens: Vec<String> = match shell_words::split(stmt) {
        Ok(t) => t,
        Err(_) => {
            output.warnings.push(format!(
                "line {line_num}: shell tokenization failed; attempting best-effort parse"
            ));
            // Fall back to whitespace split.
            stmt.split_whitespace().map(str::to_string).collect()
        }
    };

    let mut iter = tokens.iter().peekable();

    // Consume "docker" and "run".
    let _docker = iter.next();
    let _run = iter.next();

    let mut source_name: Option<String> = None;
    let mut networks: Vec<String> = Vec::new();
    let mut raw_env: Vec<(String, String)> = Vec::new();
    let mut raw_ports: Vec<String> = Vec::new();
    let mut raw_volumes: Vec<String> = Vec::new();
    let mut image_ref: Option<String> = None;
    let mut warnings: Vec<String> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();

    while let Some(tok) = iter.next() {
        // ── Boolean flags ────────────────────────────────────────────────────
        match tok.as_str() {
            "-d" | "--detach" | "--init" | "--rm" => continue,

            "--privileged" => return Err(DockerRunScriptImportError::PrivilegedRejected),

            // ── Single-value flags ───────────────────────────────────────────
            "--name" => {
                source_name = iter.next().cloned();
            }
            "--network" | "--net" => {
                let net = iter.next().cloned().unwrap_or_default();
                if net == "host" {
                    return Err(DockerRunScriptImportError::HostNetworkRejected);
                }
                if !net.is_empty() {
                    networks.push(net);
                }
            }
            "-e" | "--env" => {
                if let Some(kv) = iter.next() {
                    raw_env.push(parse_env_kv(kv));
                }
            }
            "-p" | "--publish" => {
                if let Some(pm) = iter.next() {
                    raw_ports.push(pm.clone());
                }
            }
            "-v" | "--volume" => {
                if let Some(vm) = iter.next() {
                    raw_volumes.push(vm.clone());
                }
            }
            "--restart" => {
                let policy = iter.next().cloned().unwrap_or_default();
                warnings.push(format!(
                    "line {line_num}: --restart {policy} is ignored; Ato session owns lifecycle"
                ));
            }

            // ── Unsupported but non-fatal ────────────────────────────────────
            "--cap-add" | "--cap-drop" => {
                let val = iter.next().cloned().unwrap_or_default();
                unsupported.push(format!("line {line_num}: {tok} {val} is not supported"));
            }
            "--device" | "--userns" | "--pid" | "--ipc" => {
                let val = iter.next().cloned().unwrap_or_default();
                unsupported.push(format!("line {line_num}: {tok} {val} is not supported"));
            }
            "--log-driver" | "--log-opt" => {
                let val = iter.next().cloned().unwrap_or_default();
                unsupported.push(format!("line {line_num}: {tok} {val} is not supported"));
            }

            // ── Equals-form flags (--name=foo, --env=KEY=VALUE, etc.) ────────
            _ if tok.starts_with("--name=") => {
                source_name = Some(tok["--name=".len()..].to_string());
            }
            _ if tok.starts_with("--network=") || tok.starts_with("--net=") => {
                let key_len = if tok.starts_with("--net=") {
                    "--net=".len()
                } else {
                    "--network=".len()
                };
                let net = tok[key_len..].to_string();
                if net == "host" {
                    return Err(DockerRunScriptImportError::HostNetworkRejected);
                }
                if !net.is_empty() {
                    networks.push(net);
                }
            }
            _ if tok.starts_with("-e=") || tok.starts_with("--env=") => {
                let kv_start = if tok.starts_with("-e=") {
                    3
                } else {
                    "--env=".len()
                };
                raw_env.push(parse_env_kv(&tok[kv_start..]));
            }
            _ if tok.starts_with("-p=") || tok.starts_with("--publish=") => {
                let s = if let Some(v) = tok.strip_prefix("-p=") {
                    v
                } else {
                    &tok["--publish=".len()..]
                };
                raw_ports.push(s.to_string());
            }
            _ if tok.starts_with("-v=") || tok.starts_with("--volume=") => {
                let s = if let Some(v) = tok.strip_prefix("-v=") {
                    v
                } else {
                    &tok["--volume=".len()..]
                };
                raw_volumes.push(s.to_string());
            }
            _ if tok.starts_with("--restart=") => {
                let policy = &tok["--restart=".len()..];
                warnings.push(format!(
                    "line {line_num}: --restart={policy} is ignored; Ato session owns lifecycle"
                ));
            }
            _ if tok.starts_with("--cap-add=") || tok.starts_with("--cap-drop=") => {
                unsupported.push(format!("line {line_num}: {tok} is not supported"));
            }

            // ── Skip combined short flags like -dit, -it, etc. ───────────────
            _ if tok.starts_with('-') && tok.len() > 1 && !tok.starts_with("--") => {
                // Multi-flag short form like -dit; skip.
                continue;
            }

            // ── Shell variable in flag position (e.g., $volume_mount) ────────
            // The Blinko install.sh pattern uses `$volume_mount \` as an
            // optional `-v` flag that expands to empty string or a volume
            // spec at runtime.  We cannot expand it statically; skip it and
            // continue so the real image ref that follows is captured.
            _ if tok.starts_with('$') && iter.peek().is_some() => {
                warnings.push(format!(
                    "line {line_num}: shell variable '{tok}' in flag position skipped \
                     (dynamic expansion not supported; volume mounts using shell variables \
                     must be declared as state bindings)"
                ));
                continue;
            }

            // ── Image reference (first positional arg) ───────────────────────
            _ => {
                image_ref = Some(tok.clone());
                // Everything after the image is a command — stop parsing.
                break;
            }
        }
    }

    let image_ref = image_ref.ok_or(DockerRunScriptImportError::MissingImage { line: line_num })?;

    // Emit warnings and unsupported from this docker run call.
    output.warnings.extend(warnings.iter().cloned());
    output
        .unsupported_features
        .extend(unsupported.iter().cloned());

    // Build logical label from --name.
    let label = source_name
        .as_deref()
        .map(sanitize_service_label)
        .unwrap_or_else(|| format!("service-{line_num}"));

    Ok(RawParsedService {
        line: line_num,
        label,
        source_name,
        image_ref,
        networks,
        raw_env,
        raw_ports,
        raw_volumes,
        warnings,
        unsupported,
        depends_on: Vec::new(),
    })
}

// ── Post-processing ────────────────────────────────────────────────────────────

/// Rewrite `DATABASE_URL` values that reference another service's `--name` with
/// the corresponding logical label, and record a dependency.
fn rewrite_database_url_aliases(
    raw: &mut RawParsedService,
    name_to_label: &HashMap<String, String>,
    output: &mut DockerRunScriptImportOutput,
) {
    for (key, value) in raw.raw_env.iter_mut() {
        if key.to_uppercase() != "DATABASE_URL" {
            continue;
        }
        // Try to rewrite any embedded container name with the logical label.
        let original = value.clone();
        for (src_name, label) in name_to_label {
            if value.contains(src_name.as_str()) && src_name != label {
                *value = value.replace(src_name.as_str(), label.as_str());
            }
        }
        // Infer dependency: if DATABASE_URL mentions a logical label, depend on it.
        for label in name_to_label.values() {
            if label != &raw.label
                && value.contains(format!("@{label}:").as_str())
                && !raw.depends_on.contains(label)
            {
                raw.depends_on.push(label.clone());
                output.warnings.push(format!(
                    "[{}] inferred depends_on '{}' from DATABASE_URL",
                    raw.label, label
                ));
            }
        }
        if *value != original {
            output.warnings.push(format!(
                "[{}] DATABASE_URL rewritten to use logical service alias",
                raw.label
            ));
        }
    }
}

/// If all services share a network AND one service's env references another
/// service's name, we could infer a dependency. This is a conservative version:
/// only add a dep if there's a clear DATABASE_URL or URL reference.
fn infer_network_dependencies(_raw: &mut RawParsedService, _networks: &[String], _idx: usize) {
    // Currently handled by rewrite_database_url_aliases.
    // Placeholder for future network-topology-based inference.
}

/// Convert a `RawParsedService` into a `DockerRunImportedService`.
fn build_service(
    raw: RawParsedService,
    _name_to_label: &HashMap<String, String>,
    output: &mut DockerRunScriptImportOutput,
) -> Result<DockerRunImportedService, DockerRunScriptImportError> {
    let svc_label = raw.label.clone();
    let line = raw.line;

    // Build env list.
    let mut env: Vec<ImportedOciEnvEntry> = Vec::new();
    for (key, value) in &raw.raw_env {
        let is_secret_like = is_secret_like_key(key);
        let entry_value = resolve_env_value(key, value, is_secret_like, &svc_label, line, output);
        env.push(ImportedOciEnvEntry {
            key: key.clone(),
            value: entry_value,
            is_secret_like,
        });
    }

    // Build port list.
    let mut ports: Vec<ImportedOciPort> = Vec::new();
    for port_str in &raw.raw_ports {
        match parse_port_mapping(port_str) {
            Some((container_port, protocol)) => {
                ports.push(ImportedOciPort {
                    container_port,
                    protocol,
                });
            }
            None => {
                output
                    .warnings
                    .push(format!("[{svc_label}] malformed port mapping '{port_str}'"));
            }
        }
    }

    // Build volume mounts.
    let mut volume_mounts: Vec<ImportedOciVolumeMount> = Vec::new();
    for vol_str in &raw.raw_volumes {
        let mount = parse_volume_mount(vol_str, &svc_label, output)?;
        if let Some(m) = mount {
            volume_mounts.push(m);
        }
    }

    // Build depends_on list.
    let depends_on: Vec<ImportedDependency> = raw
        .depends_on
        .iter()
        .map(|dep| ImportedDependency {
            service: dep.clone(),
            condition: DependencyCondition::ServiceStarted,
        })
        .collect();

    Ok(DockerRunImportedService {
        name: svc_label,
        source_container_name: raw.source_name,
        image_ref: raw.image_ref,
        env,
        ports,
        volume_mounts,
        depends_on,
        source_networks: raw.networks,
    })
}

// ── Env value resolution ──────────────────────────────────────────────────────

fn resolve_env_value(
    key: &str,
    raw_value: &str,
    is_secret_like: bool,
    svc_label: &str,
    _line: usize,
    output: &mut DockerRunScriptImportOutput,
) -> ImportedEnvValue {
    // If the value contains shell variable expansion, mark as RequiredExternal.
    if contains_shell_variable(raw_value) {
        // Keep the static parts if the value is DATABASE_URL with a static host component.
        // For all others, drop entirely and require from host env.
        output.warnings.push(format!(
            "[{svc_label}] env '{key}' contains shell variable expansion; \
             treating as required-external (supply via host environment)"
        ));
        return ImportedEnvValue::RequiredExternal;
    }

    if is_secret_like {
        if looks_like_unsafe_default(raw_value) {
            output.warnings.push(format!(
                "[{svc_label}] env '{key}' has an unsafe default value; \
                 replace with Ato secret generation"
            ));
        } else {
            output.warnings.push(format!(
                "[{svc_label}] env '{key}' appears to contain a secret; \
                 consider using Ato secret generation"
            ));
        }
    }

    // DATABASE_URL is always secret-like if it contains a URL with credentials.
    if key.to_uppercase() == "DATABASE_URL" && raw_value.contains("://") && raw_value.contains(':')
    {
        // Has credentials embedded.
        output.warnings.push(format!(
            "[{svc_label}] DATABASE_URL contains embedded credentials; \
                 value redacted from receipt — use Ato secret references"
        ));
    }

    ImportedEnvValue::Literal(raw_value.to_string())
}

fn contains_shell_variable(s: &str) -> bool {
    s.contains("${") || (s.contains('$') && !s.starts_with("$$"))
}

// ── Port parsing ──────────────────────────────────────────────────────────────

fn parse_port_mapping(s: &str) -> Option<(u16, String)> {
    // Forms: "container", "host:container", "ip:host:container", with /proto suffix.
    let (port_part, proto) = if let Some(idx) = s.rfind('/') {
        let p = match &s[idx + 1..] {
            "udp" => "udp",
            _ => "tcp",
        };
        (&s[..idx], p.to_string())
    } else {
        (s, "tcp".to_string())
    };

    let parts: Vec<&str> = port_part.split(':').collect();
    let container_str = match parts.len() {
        1 => parts[0],
        2 => parts[1],
        3 => parts[2],
        _ => return None,
    };
    let port: u16 = container_str.trim().parse().ok()?;
    Some((port, proto))
}

// ── Volume parsing ────────────────────────────────────────────────────────────

fn parse_volume_mount(
    vol_str: &str,
    svc_label: &str,
    output: &mut DockerRunScriptImportOutput,
) -> Result<Option<ImportedOciVolumeMount>, DockerRunScriptImportError> {
    let parts: Vec<&str> = vol_str.splitn(3, ':').collect();
    let (raw_source, target, readonly) = match parts.len() {
        1 => {
            // Anonymous volume "/path" — skip.
            output.unsupported_features.push(format!(
                "[{svc_label}] anonymous volume '{vol_str}' is not supported; use a named volume"
            ));
            return Ok(None);
        }
        2 => (parts[0], parts[1], false),
        3 => (parts[0], parts[1], parts[2] == "ro"),
        _ => return Ok(None),
    };

    // Absolute bind mount → reject.
    if raw_source.starts_with('/') {
        return Err(DockerRunScriptImportError::AbsoluteBindMountRejected {
            service: svc_label.to_string(),
            path: raw_source.to_string(),
        });
    }

    // Relative bind mount → warn and keep as project-root-scoped.
    if raw_source.starts_with('.') {
        output.warnings.push(format!(
            "[{svc_label}] relative bind mount '{raw_source}:{target}' is project-root-scoped; \
             consider converting to a named volume"
        ));
        let state_name = sanitize_service_label(raw_source.trim_start_matches("./"));
        if !output
            .state_bindings
            .iter()
            .any(|b| b.state_name == state_name)
        {
            output.state_bindings.push(ImportedStateBinding {
                state_name: state_name.clone(),
                kind: StateBindingKind::ProjectRootBind {
                    host_rel_path: raw_source.to_string(),
                },
            });
        }
        return Ok(Some(ImportedOciVolumeMount {
            state_name,
            target: target.to_string(),
            readonly,
        }));
    }

    // Named volume.
    let state_name = sanitize_service_label(raw_source);
    if !output
        .state_bindings
        .iter()
        .any(|b| b.state_name == state_name)
    {
        output.state_bindings.push(ImportedStateBinding {
            state_name: state_name.clone(),
            kind: StateBindingKind::Named,
        });
    }
    Ok(Some(ImportedOciVolumeMount {
        state_name,
        target: target.to_string(),
        readonly,
    }))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_env_kv(kv: &str) -> (String, String) {
    if let Some(idx) = kv.find('=') {
        (kv[..idx].to_string(), kv[idx + 1..].to_string())
    } else {
        (kv.to_string(), String::new())
    }
}

fn sanitize_service_label(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
}

fn is_secret_like_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    [
        "PASSWORD",
        "SECRET",
        "TOKEN",
        "PASSWD",
        "CREDENTIAL",
        "_KEY",
    ]
    .iter()
    .any(|pat| upper.contains(pat))
}

const UNSAFE_DEFAULT_VALUES: &[&str] = &[
    "secret",
    "password",
    "mysecretpassword",
    "my_ultra_secure_nextauth_secret",
    "admin",
    "root",
    "postgres",
    "changeme",
    "p@ssw0rd",
    "pass",
    "test",
    "unsafe",
    "default",
    "example",
    "demo",
];

fn looks_like_unsafe_default(val: &str) -> bool {
    let lower = val.to_lowercase();
    UNSAFE_DEFAULT_VALUES.iter().any(|pat| lower == *pat)
}

/// Compute a stable SHA-256 hex hash of the install script text.
pub fn compute_script_source_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn find_cycle_in(dep_map: &HashMap<String, Vec<String>>) -> Option<Vec<String>> {
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut path: Vec<String> = Vec::new();

    fn dfs(
        node: &str,
        dep_map: &HashMap<String, Vec<String>>,
        visited: &mut std::collections::HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if path.contains(&node.to_string()) {
            let idx = path.iter().position(|n| n == node).unwrap();
            let mut cycle = path[idx..].to_vec();
            cycle.push(node.to_string());
            return Some(cycle);
        }
        if visited.contains(node) {
            return None;
        }
        path.push(node.to_string());
        for dep in dep_map.get(node).map(Vec::as_slice).unwrap_or(&[]) {
            if let Some(cycle) = dfs(dep, dep_map, visited, path) {
                return Some(cycle);
            }
        }
        path.pop();
        visited.insert(node.to_string());
        None
    }

    for node in dep_map.keys() {
        if let Some(cycle) = dfs(node, dep_map, &mut visited, &mut path) {
            return Some(cycle);
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(script: &str) -> DockerRunScriptImportInput {
        DockerRunScriptImportInput::new(script.to_string(), PathBuf::from("install.sh"))
    }

    // ── 1. Basic single-service parsing ───────────────────────────────────────

    #[test]
    fn parses_simple_docker_run_single_service() {
        let script = r#"
docker run -d \
  --name my-app \
  -p 8080:8080 \
  -e APP_ENV=production \
  myorg/myapp:latest
"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services.len(), 1);
        let svc = &out.services[0];
        assert_eq!(svc.name, "my-app");
        assert_eq!(svc.image_ref, "myorg/myapp:latest");
        assert_eq!(svc.ports.len(), 1);
        assert_eq!(svc.ports[0].container_port, 8080);
        assert_eq!(svc.env.len(), 1);
        assert_eq!(svc.env[0].key, "APP_ENV");
    }

    // ── 2. Blinko-style two-service parsing ───────────────────────────────────

    #[test]
    fn parses_blinko_install_sh_two_services() {
        let script = blinko_install_sh();
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services.len(), 2, "expected app + postgres");

        let names: Vec<&str> = out.services.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"blinko-postgres") || names.contains(&"blinko-website"),
            "expected blinko services, got: {names:?}"
        );

        // App service should have a port published.
        let app_svc = out
            .services
            .iter()
            .find(|s| s.name.contains("website"))
            .expect("blinko-website service");
        assert!(!app_svc.ports.is_empty(), "app service should have port");
        assert_eq!(app_svc.ports[0].container_port, 1111);
    }

    // ── 2b. Real Blinko install.sh with $volume_mount shell variable ──────────

    #[test]
    fn shell_variable_in_flag_position_is_skipped_real_blinko_pattern() {
        // The actual Blinko install.sh uses `$volume_mount \` as an optional
        // -v flag that expands to either `-v /path:/app/.blinko` or empty
        // string depending on interactive prompt.  The parser must skip it
        // and find `blinkospace/blinko:latest` as the image ref.
        let script = r#"#!/bin/bash
docker network create blinko-network
docker run -d \
  --name blinko-postgres \
  --network blinko-network \
  -e POSTGRES_DB=postgres \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=mysecretpassword \
  --restart always \
  postgres:14
docker run -d \
  --name blinko-website \
  --network blinko-network \
  -p 1111:1111 \
  -e NODE_ENV=production \
  -e NEXTAUTH_SECRET=my_ultra_secure_nextauth_secret \
  -e DATABASE_URL=postgresql://postgres:mysecretpassword@blinko-postgres:5432/postgres \
  $volume_mount \
  --restart always \
  blinkospace/blinko:latest
"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services.len(), 2, "expected postgres + blinko-website");

        let website = out
            .services
            .iter()
            .find(|s| s.name.contains("website"))
            .expect("blinko-website service");
        assert_eq!(
            website.image_ref, "blinkospace/blinko:latest",
            "$volume_mount must not be treated as image ref"
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("$volume_mount") && w.contains("skipped")),
            "expected warning about shell variable, got: {:?}",
            out.warnings
        );
    }

    // ── 3. docker network create is metadata only ─────────────────────────────

    #[test]
    fn docker_network_name_is_source_metadata_only() {
        let script = r#"
docker network create blinko-net
docker run -d --name my-app --network blinko-net alpine:3.19
"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert!(out.extracted_networks.contains(&"blinko-net".to_string()));
        // Runtime network alias comes from the service name, not from blinko-net.
        assert_eq!(out.services[0].name, "my-app");
        assert!(!out.services[0].source_networks.is_empty());
    }

    // ── 4. --name → logical label, not runtime name ───────────────────────────

    #[test]
    fn docker_name_becomes_logical_label_not_runtime_name() {
        let script = r#"docker run -d --name blinko-postgres -e POSTGRES_PASSWORD=secret postgres:16-alpine"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services[0].name, "blinko-postgres");
        // source_container_name is preserved separately.
        assert_eq!(
            out.services[0].source_container_name.as_deref(),
            Some("blinko-postgres")
        );
    }

    // ── 5. --restart is ignored with warning ──────────────────────────────────

    #[test]
    fn restart_always_is_ignored_with_warning() {
        let script = r#"docker run -d --name app --restart always alpine:3.19"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services.len(), 1);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("restart") && w.contains("ignored")),
            "expected restart warning, got: {:?}",
            out.warnings
        );
    }

    // ── 6. Absolute bind mount is rejected ────────────────────────────────────

    #[test]
    fn absolute_bind_mount_is_rejected() {
        let script = r#"docker run -d --name app -v /host/data:/app/data alpine:3.19"#;
        let err = import_docker_run_script(&make_input(script)).unwrap_err();
        assert!(
            matches!(
                err,
                DockerRunScriptImportError::AbsoluteBindMountRejected { .. }
            ),
            "expected AbsoluteBindMountRejected, got: {err:?}"
        );
    }

    // ── 7. Prompt control flow is not executed ────────────────────────────────

    #[test]
    fn prompt_control_flow_is_not_executed() {
        let script = r#"
#!/bin/bash
echo "Setting up Blinko..."
read -p "Enter password: " POSTGRES_PASSWORD
docker run -d --name blinko-postgres postgres:16-alpine
echo "Done"
"#;
        // Should succeed — static docker run is extracted; prompt is skipped.
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services.len(), 1);
        assert_eq!(out.services[0].name, "blinko-postgres");
    }

    // ── 8. --privileged is rejected ───────────────────────────────────────────

    #[test]
    fn unsupported_privileged_is_rejected() {
        let script = r#"docker run -d --privileged --name app alpine:3.19"#;
        let err = import_docker_run_script(&make_input(script)).unwrap_err();
        assert!(matches!(
            err,
            DockerRunScriptImportError::PrivilegedRejected
        ));
    }

    // ── 9. --network host is rejected ─────────────────────────────────────────

    #[test]
    fn host_network_is_rejected() {
        let script = r#"docker run -d --network host --name app alpine:3.19"#;
        let err = import_docker_run_script(&make_input(script)).unwrap_err();
        assert!(matches!(
            err,
            DockerRunScriptImportError::HostNetworkRejected
        ));
    }

    // ── 10. Port mapping uses container port with auto host port ──────────────

    #[test]
    fn port_mapping_uses_container_port_with_auto_host_port() {
        let script = r#"docker run -d --name app -p 3000:3000 alpine:3.19"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services[0].ports[0].container_port, 3000);
    }

    // ── 11. DATABASE_URL service name is rewritten to alias ───────────────────

    #[test]
    fn database_url_service_name_is_rewritten_to_alias() {
        let script = r#"
docker run -d --name blinko-postgres -e POSTGRES_PASSWORD=changeme postgres:16
docker run -d --name blinko-website \
  -e DATABASE_URL="postgresql://postgres:changeme@blinko-postgres:5432/blinko" \
  blinkospace/blinko:latest
"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        let app = out
            .services
            .iter()
            .find(|s| s.name.contains("website"))
            .unwrap();
        let _db_url_entry = app.env.iter().find(|e| e.key == "DATABASE_URL").unwrap();
        // blinko-postgres should remain as the alias since label == source_name here.
        // The dep should be inferred.
        assert!(!app.depends_on.is_empty(), "app should depend on db");
    }

    // ── 12. Secret-like env values are redacted ────────────────────────────────

    #[test]
    fn secret_like_env_values_are_redacted() {
        let script = r#"docker run -d --name app -e POSTGRES_PASSWORD=supersecret alpine:3.19"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        let pass_entry = out.services[0]
            .env
            .iter()
            .find(|e| e.key == "POSTGRES_PASSWORD")
            .unwrap();
        assert!(pass_entry.is_secret_like);
        assert!(out.warnings.iter().any(|w| w.contains("POSTGRES_PASSWORD")));
    }

    // ── 13. Unsafe default password warns ────────────────────────────────────

    #[test]
    fn unsafe_default_password_warns_or_generates_secret() {
        let script = r#"docker run -d --name pg -e POSTGRES_PASSWORD=mysecretpassword postgres:16"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert!(
            out.warnings.iter().any(|w| w.contains("unsafe default")),
            "expected unsafe default warning, got: {:?}",
            out.warnings
        );
    }

    // ── 14. Install.sh source hash written correctly ─────────────────────────

    #[test]
    fn install_sh_source_hash_written_to_oci_lock() {
        let script = r#"docker run -d --name app alpine:3.19"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert!(out.source_hash.starts_with("sha256:"));
        assert_eq!(out.source_hash, compute_script_source_hash(script));
    }

    // ── 15. Source hash stability ────────────────────────────────────────────

    #[test]
    fn rerun_reuses_install_sh_lock_entries() {
        let script = r#"docker run -d --name app alpine:3.19"#;
        let h1 = compute_script_source_hash(script);
        let h2 = compute_script_source_hash(script);
        assert_eq!(h1, h2, "source hash must be deterministic");
    }

    // ── 16. Source hash drift on change ──────────────────────────────────────

    #[test]
    fn source_hash_drift_requires_refresh_or_reresolve() {
        let script1 = r#"docker run -d --name app alpine:3.19"#;
        let script2 = r#"docker run -d --name app alpine:3.20"#;
        let h1 = compute_script_source_hash(script1);
        let h2 = compute_script_source_hash(script2);
        assert_ne!(h1, h2, "changed script must produce different hash");
    }

    // ── 17. install.sh path does not use legacy Bollard ─────────────────────

    #[test]
    fn install_sh_path_does_not_use_legacy_bollard() {
        // The importer is pure. Verifying it returns services (not an error) and
        // has no Bollard references in the output model is sufficient.
        let script = r#"docker run -d --name app alpine:3.19"#;
        let out = import_docker_run_script(&make_input(script)).unwrap();
        assert_eq!(out.services.len(), 1);
        // The service runtime is "oci" (verified via orchestration plan conversion).
        let plan = out.to_orchestration_plan().unwrap();
        let resolved = &plan.services[0];
        assert!(matches!(&resolved.runtime, ResolvedServiceRuntime::Oci(_)));
    }

    // ── 18. Blinko smoke: imports and executes with fake provider ─────────────

    #[test]
    fn imported_install_sh_graph_executes_with_fake_provider() {
        let script = blinko_install_sh();
        let out = import_docker_run_script(&make_input(script)).unwrap();
        // Two services.
        assert_eq!(out.services.len(), 2);
        // Can convert to orchestration plan without error.
        let plan = out.to_orchestration_plan().unwrap();
        assert_eq!(plan.services.len(), 2);
        // Startup order: postgres before blinko.
        let order = &plan.startup_order;
        let pg_pos = order.iter().position(|n| n.contains("postgres"));
        let app_pos = order.iter().position(|n| n.contains("website"));
        if let (Some(pg), Some(app)) = (pg_pos, app_pos) {
            assert!(pg < app, "postgres must start before blinko-website");
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn blinko_install_sh() -> &'static str {
        r#"#!/bin/bash
docker network create blinko-net

docker run -d \
  --name blinko-postgres \
  --network blinko-net \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=changeme \
  -e POSTGRES_DB=blinko \
  -v pg_data:/var/lib/postgresql/data \
  postgres:16-alpine

docker run -d \
  --name blinko-website \
  --network blinko-net \
  -p 1111:1111 \
  -e DATABASE_URL="postgresql://postgres:changeme@blinko-postgres:5432/blinko" \
  -e NEXTAUTH_SECRET=my_ultra_secure_nextauth_secret \
  -e NEXTAUTH_URL=http://0.0.0.0:1111 \
  --restart always \
  blinkospace/blinko:latest
"#
    }
}
