use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::common::paths::{ato_runs_dir, ato_store_dir, nacelle_home_dir_or_workspace_tmp};
use capsule_core::execution_identity::{
    CanonicalPath, DependencyIdentityV2, EnvironmentEntry, EnvironmentIdentityV2, EnvironmentMode,
    FilesystemIdentityV2, FilesystemSemantics, LaunchArg, LaunchEntryPoint, LaunchIdentityV2,
    LocalExecutionLocator, PathRoleNormalizer, PolicyIdentityV2, ReadonlyLayerIdentity,
    RuntimeCompleteness, RuntimeIdentityV2, SourceIdentityV2, SourceProvenance,
    SourceProvenanceKind, StateBindingIdentity, StateBindingKind, Tracked,
    ValueNormalizationStatus, WorkspacePathCanonicalizer, WritableDirIdentity,
    WritableDirLifecycle,
};
use capsule_core::execution_plan::model::ExecutionPlan;
use capsule_core::launch_spec::LaunchSpec;
use capsule_core::router::ManifestData;

use crate::application::build_materialization::BuildObservation;
use crate::application::execution_observers::{hash_source_tree, hash_tree, is_sensitive_env_key};
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::runtime::overrides as runtime_overrides;

/// Identity-relevant environment keys per plan §"Environment identity v2".
///
/// Manifest-declared and Ato-injected keys are added on top of this fixed set
/// at observation time.
const INTRINSIC_IDENTITY_ENV_KEYS: &[&str] = &["PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ"];

/// Bundle of canonicalization helpers passed to every v2 observer so that path
/// canonicalization rules stay consistent across source/launch/filesystem/env
/// observation.
#[allow(dead_code)] // workspace_root/ato_home/ato_store/ato_runs retained for future observer wiring (Phase 8).
pub(crate) struct ObserverContextV2 {
    pub(crate) canonicalizer: WorkspacePathCanonicalizer,
    pub(crate) normalizer: PathRoleNormalizer,
    pub(crate) workspace_root: PathBuf,
    pub(crate) ato_home: PathBuf,
    pub(crate) ato_store: PathBuf,
    pub(crate) ato_runtimes: PathBuf,
    pub(crate) ato_runs: PathBuf,
}

impl ObserverContextV2 {
    pub(crate) fn for_plan(plan: &ManifestData) -> Self {
        let workspace_root = plan.workspace_root.clone();
        let ato_home = nacelle_home_dir_or_workspace_tmp();
        let ato_store = ato_store_dir();
        let ato_runtimes = ato_home.join("runtimes");
        let ato_runs = ato_runs_dir();

        let canonicalizer = WorkspacePathCanonicalizer::new(workspace_root.display().to_string());
        let normalizer = PathRoleNormalizer::new(vec![
            // Most specific first; PathRoleNormalizer also sorts longest-first.
            ("${ATO_STORE}".to_string(), ato_store.display().to_string()),
            (
                "${ATO_RUNTIME}".to_string(),
                ato_runtimes.display().to_string(),
            ),
            ("${ATO_RUNS}".to_string(), ato_runs.display().to_string()),
            ("${ATO_HOME}".to_string(), ato_home.display().to_string()),
            (
                "${WORKSPACE}".to_string(),
                workspace_root.display().to_string(),
            ),
        ]);

        Self {
            canonicalizer,
            normalizer,
            workspace_root,
            ato_home,
            ato_store,
            ato_runtimes,
            ato_runs,
        }
    }

    fn role_for_path(&self, path: &Path) -> Tracked<String> {
        self.canonicalizer.role_string(path.display().to_string())
    }
}

pub(crate) fn observe_source_v2(
    plan: &ManifestData,
    ctx: &ObserverContextV2,
) -> Result<SourceIdentityV2> {
    // Use hash_source_tree (NOT hash_tree) so the v2 observer respects the
    // same DEFAULT_IGNORED_DIRS list as the v1 observer (skips
    // `.git`, `.venv`, `node_modules`, `target`, `__pycache__`, `.ato`,
    // `.tmp`). hash_tree has no ignore list and would otherwise pull
    // build-tool byproducts (uv-created `.venv`, npm-created
    // `node_modules`, Python `__pycache__`) into the launch envelope
    // identity, causing source_tree_hash to drift across runs even when
    // the user-authored source bytes are identical.
    let source_tree_hash = if plan.workspace_root.is_dir() {
        let hash = hash_source_tree(&plan.workspace_root).with_context(|| {
            format!(
                "failed to hash workspace source tree at {}",
                plan.workspace_root.display()
            )
        })?;
        Tracked::known(hash)
    } else {
        Tracked::unknown(format!(
            "workspace root is not available for source observation: {}",
            plan.workspace_root.display()
        ))
    };

    let manifest_path_role = match ctx
        .canonicalizer
        .canonicalize(plan.manifest_path.display().to_string())
    {
        CanonicalPath::WorkspaceRoot => Tracked::known("workspace:.".to_string()),
        CanonicalPath::WorkspaceRelative(relative) => {
            Tracked::known(format!("workspace:{relative}"))
        }
        CanonicalPath::OutsideWorkspace(_) => {
            Tracked::untracked("manifest path is outside workspace")
        }
    };

    Ok(SourceIdentityV2 {
        source_tree_hash,
        manifest_path_role,
    })
}

pub(crate) fn observe_source_provenance(plan: &ManifestData) -> SourceProvenance {
    SourceProvenance {
        kind: SourceProvenanceKind::Local,
        git_remote: None,
        git_commit: None,
        registry_ref: detect_registry_ref(plan),
    }
}

fn detect_registry_ref(plan: &ManifestData) -> Option<String> {
    let path = plan.workspace_root.display().to_string();
    if let Some(idx) = path.find("/.ato/runtimes/") {
        let suffix = &path[idx + "/.ato/runtimes/".len()..];
        if !suffix.is_empty() {
            return Some(format!("registry:{suffix}"));
        }
    }
    None
}

pub(crate) fn observe_dependencies_v2(
    plan: &ManifestData,
    launch_spec: &LaunchSpec,
    launch_ctx: &RuntimeLaunchContext,
    build_observation: Option<&BuildObservation>,
    runtime: &RuntimeIdentityV2,
) -> Result<DependencyIdentityV2> {
    let dep_v1 = crate::application::execution_observers::observe_dependencies(
        launch_spec,
        launch_ctx,
        build_observation,
    )?;

    let derivation_inputs = build_observation.map(|observation| {
        let install_tokens = shell_words::split(&observation.command)
            .unwrap_or_else(|_| vec![observation.command.clone()]);
        let package_manager = install_tokens
            .first()
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        capsule_core::execution_identity::DependencyDerivationInputsV2 {
            package_manager,
            package_manager_version: Tracked::unknown(
                "package manager version observer not implemented",
            ),
            runtime_resolved_ref: runtime.resolved_ref.clone(),
            platform_abi: runtime.platform.clone(),
            dependency_manifest_digests: BTreeMap::new(),
            lockfile_digests: build_observation_lockfile_digests(plan),
            install_command: install_tokens,
            package_manager_config_hash: Tracked::untracked(
                "package manager config observer not implemented",
            ),
            lifecycle_script_policy_hash: Tracked::untracked(
                "lifecycle script policy observer not implemented",
            ),
            registry_policy_hash: Tracked::untracked("registry policy observer not implemented"),
            network_policy_hash: Tracked::untracked(
                "materialization network policy observer not implemented",
            ),
            environment_allowlist_hash: Tracked::untracked(
                "materialization environment allowlist observer not implemented",
            ),
            declared_system_build_inputs: Vec::new(),
        }
    });

    Ok(DependencyIdentityV2 {
        derivation_hash: dep_v1.derivation_hash,
        output_hash: dep_v1.output_hash,
        derivation_inputs,
    })
}

fn build_observation_lockfile_digests(plan: &ManifestData) -> BTreeMap<String, String> {
    let mut digests = BTreeMap::new();
    let lock_path = plan.lock_path.clone();
    if lock_path.is_file() {
        if let Ok(bytes) = std::fs::read(&lock_path) {
            digests.insert(
                lock_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("ato.lock.json")
                    .to_string(),
                format!("blake3:{}", blake3::hash(&bytes).to_hex()),
            );
        }
    }
    digests
}

pub(crate) fn observe_runtime_v2(
    execution_plan: &ExecutionPlan,
    launch_spec: &LaunchSpec,
    ctx: &ObserverContextV2,
) -> Result<RuntimeIdentityV2> {
    let v1 = crate::application::execution_observers::observe_runtime(execution_plan, launch_spec)?;

    let resolved_ref =
        build_runtime_resolved_ref(launch_spec.runtime.as_deref(), v1.resolved.as_deref());
    let _ = ctx; // canonicalizer not needed: resolved path moves to local locator

    let completeness = match (
        v1.binary_hash.value.as_ref(),
        v1.dynamic_linkage.value.as_ref(),
    ) {
        (Some(_), Some(_)) => RuntimeCompleteness::BinaryWithDynamicClosure,
        (Some(_), None) => RuntimeCompleteness::ResolvedBinary,
        (None, _) if v1.resolved.is_some() => RuntimeCompleteness::DeclaredOnly,
        _ => RuntimeCompleteness::BestEffort,
    };

    Ok(RuntimeIdentityV2 {
        declared: v1.declared,
        resolved_ref,
        binary_hash: v1.binary_hash,
        dynamic_linkage: v1.dynamic_linkage,
        completeness,
        platform: v1.platform,
    })
}

fn build_runtime_resolved_ref(
    declared: Option<&str>,
    resolved_path: Option<&str>,
) -> Tracked<String> {
    if let Some(declared) = declared.filter(|value| !value.is_empty() && !value.contains('/')) {
        return Tracked::known(declared.to_string());
    }
    if let Some(path) = resolved_path {
        if let Some(file_name) = Path::new(path).file_name().and_then(|name| name.to_str()) {
            return Tracked::known(file_name.to_string());
        }
    }
    Tracked::untracked("runtime resolved_ref observer cannot derive a portable identifier")
}

pub(crate) fn observe_environment_v2(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
    ctx: &ObserverContextV2,
) -> Result<EnvironmentIdentityV2> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    env.extend(plan.execution_env());
    env.extend(launch_ctx.merged_env());
    if let Some(port) = runtime_overrides::override_port(plan.execution_port()) {
        env.insert("PORT".to_string(), port.to_string());
    }

    let manifest_keys: Vec<String> = plan.execution_env().keys().cloned().collect();
    let injected_keys: Vec<String> = launch_ctx.injected_env().keys().cloned().collect();

    let identity_relevant: std::collections::HashSet<String> = INTRINSIC_IDENTITY_ENV_KEYS
        .iter()
        .map(|key| key.to_string())
        .chain(manifest_keys)
        .chain(injected_keys)
        .collect();

    let mut entries = Vec::new();
    let mut ambient = Vec::new();

    for (key, value) in env {
        if identity_relevant.contains(&key) {
            let entry = build_env_entry(&key, &value, &ctx.normalizer);
            entries.push(entry);
        } else {
            ambient.push(key);
        }
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    ambient.sort();

    Ok(EnvironmentIdentityV2 {
        entries,
        fd_layout: Tracked::untracked("fd layout observer not implemented"),
        umask: Tracked::untracked("umask observer not implemented"),
        ulimits: Tracked::untracked("ulimits observer not implemented"),
        mode: EnvironmentMode::Partial,
        ambient_untracked_keys: ambient,
    })
}

fn build_env_entry(key: &str, value: &str, normalizer: &PathRoleNormalizer) -> EnvironmentEntry {
    if is_sensitive_env_key(key) {
        return EnvironmentEntry {
            key: key.to_string(),
            value_hash: Tracked::untracked(
                "secret reference identity not implemented; raw value never hashed",
            ),
            normalization: ValueNormalizationStatus::SecretReferenceRequired,
        };
    }
    let (value_hash, normalization) = normalizer.tracked_hash(value);
    EnvironmentEntry {
        key: key.to_string(),
        value_hash,
        normalization,
    }
}

pub(crate) fn observe_filesystem_v2(
    plan: &ManifestData,
    launch_ctx: &RuntimeLaunchContext,
    launch_spec: &LaunchSpec,
    ctx: &ObserverContextV2,
) -> Result<FilesystemIdentityV2> {
    let source_root = ctx.role_for_path(&plan.workspace_root);
    let working_directory = ctx.role_for_path(&launch_spec.working_dir);

    let mut readonly_layers = Vec::new();
    let mut writable_dirs = Vec::new();
    for mount in launch_ctx.injected_mounts() {
        let role = mount.target.clone();
        let identity = canonical_mount_identity(&mount.source);
        if mount.readonly {
            readonly_layers.push(ReadonlyLayerIdentity { role, identity });
        } else {
            let lifecycle = lifecycle_for_writable_target(&mount.target);
            writable_dirs.push(WritableDirIdentity { role, lifecycle });
        }
    }
    readonly_layers.sort_by(|a, b| a.role.cmp(&b.role));
    writable_dirs.sort_by(|a, b| a.role.cmp(&b.role));

    let mut persistent_state: Vec<StateBindingIdentity> = plan
        .state_source_overrides
        .iter()
        .map(|(name, locator)| StateBindingIdentity {
            name: name.clone(),
            kind: classify_state_binding(locator),
            identity: Tracked::known(locator.clone()),
        })
        .collect();
    persistent_state.sort_by(|a, b| a.name.cmp(&b.name));

    let semantics = FilesystemSemantics {
        case_sensitivity: Tracked::untracked("case sensitivity observer not implemented"),
        symlink_policy: Tracked::untracked("symlink policy observer not implemented"),
        tmp_policy: Tracked::untracked("tmp policy observer not implemented"),
    };

    let partial_canonical = serde_jcs::to_vec(&PartialViewHashInput {
        source_root: source_root.value.as_deref().unwrap_or(""),
        working_directory: working_directory.value.as_deref().unwrap_or(""),
        readonly_count: readonly_layers.len(),
        writable_count: writable_dirs.len(),
        persistent_state_count: persistent_state.len(),
    })
    .map_err(|err| anyhow::anyhow!("partial_view_hash canonicalization failed: {err}"))?;
    let partial_view_hash = Some(format!(
        "blake3:{}",
        blake3::hash(&partial_canonical).to_hex()
    ));

    Ok(FilesystemIdentityV2 {
        view_hash: Tracked::untracked(
            "filesystem view hash is partial: mount source identities, case sensitivity, symlink policy, tmp policy, and full state semantics not yet observed",
        ),
        partial_view_hash,
        source_root,
        working_directory,
        readonly_layers,
        writable_dirs,
        persistent_state,
        semantics,
    })
}

fn canonical_mount_identity(source: &Path) -> Tracked<String> {
    if source.is_dir() {
        match hash_tree(source) {
            Ok(hash) => Tracked::known(hash),
            Err(_) => Tracked::unknown("failed to hash mount source tree"),
        }
    } else if source.is_file() {
        Tracked::unknown("file mount identity observer not implemented")
    } else {
        Tracked::unknown("mount source path is not present at observation time")
    }
}

fn lifecycle_for_writable_target(target: &str) -> WritableDirLifecycle {
    if target.contains("/runs/") || target == "/tmp" || target.ends_with("/tmp") {
        WritableDirLifecycle::SessionLocal
    } else if target.contains("/state/") || target.contains("/data") {
        WritableDirLifecycle::PersistentState
    } else {
        WritableDirLifecycle::HostPath
    }
}

fn classify_state_binding(locator: &str) -> StateBindingKind {
    if locator.starts_with("state-") || locator.starts_with("blake3:") {
        StateBindingKind::AtoStateRef
    } else if Path::new(locator).is_absolute() {
        StateBindingKind::HostPath
    } else {
        StateBindingKind::ContentSnapshot
    }
}

#[derive(serde::Serialize)]
struct PartialViewHashInput<'a> {
    source_root: &'a str,
    working_directory: &'a str,
    readonly_count: usize,
    writable_count: usize,
    persistent_state_count: usize,
}

pub(crate) fn observe_launch_v2(
    launch_spec: &LaunchSpec,
    launch_ctx: &RuntimeLaunchContext,
    runtime: &RuntimeIdentityV2,
    ctx: &ObserverContextV2,
) -> Result<LaunchIdentityV2> {
    let entry_point = classify_entry_point(&launch_spec.command, runtime, ctx);

    let mut argv: Vec<String> = launch_spec.args.clone();
    argv.extend(launch_ctx.command_args().iter().cloned());
    let argv = argv
        .iter()
        .map(|value| build_launch_arg(value, &ctx.normalizer))
        .collect();

    let working_directory = ctx.role_for_path(&launch_spec.working_dir);

    Ok(LaunchIdentityV2 {
        entry_point,
        argv,
        working_directory,
    })
}

fn classify_entry_point(
    command: &str,
    runtime: &RuntimeIdentityV2,
    ctx: &ObserverContextV2,
) -> LaunchEntryPoint {
    if command.is_empty() {
        return LaunchEntryPoint::Untracked {
            reason: "entry_point is empty".to_string(),
        };
    }
    if !command.contains('/') && !command.contains(std::path::MAIN_SEPARATOR) {
        if let Some(resolved_ref) = runtime.resolved_ref.value.as_deref() {
            if resolved_ref == command {
                return LaunchEntryPoint::RuntimeManaged {
                    resolved_ref: resolved_ref.to_string(),
                };
            }
        }
        return LaunchEntryPoint::Command {
            name: command.to_string(),
        };
    }
    let path = PathBuf::from(command);
    if path.is_absolute() {
        if path.starts_with(&ctx.ato_runtimes) {
            if let Some(resolved_ref) = runtime.resolved_ref.value.as_deref() {
                return LaunchEntryPoint::RuntimeManaged {
                    resolved_ref: resolved_ref.to_string(),
                };
            }
        }
        if let Some(role) = workspace_role(&path, ctx) {
            return LaunchEntryPoint::WorkspaceRelative { path: role };
        }
        return LaunchEntryPoint::Untracked {
            reason: "entry_point absolute path is outside known roles".to_string(),
        };
    }
    LaunchEntryPoint::WorkspaceRelative {
        path: format!("workspace:{}", normalize_workspace_relative(command)),
    }
}

fn workspace_role(path: &Path, ctx: &ObserverContextV2) -> Option<String> {
    let canonical = ctx.canonicalizer.canonicalize(path.display().to_string());
    match canonical {
        CanonicalPath::WorkspaceRoot => Some("workspace:.".to_string()),
        CanonicalPath::WorkspaceRelative(rel) => Some(format!("workspace:{rel}")),
        CanonicalPath::OutsideWorkspace(_) => None,
    }
}

fn normalize_workspace_relative(value: &str) -> String {
    value.replace(std::path::MAIN_SEPARATOR, "/")
}

fn build_launch_arg(value: &str, normalizer: &PathRoleNormalizer) -> LaunchArg {
    let (value_hash, normalization) = normalizer.tracked_hash(value);
    LaunchArg {
        value_hash,
        normalization,
    }
}

pub(crate) fn build_local_locator(
    plan: &ManifestData,
    launch_spec: &LaunchSpec,
    launch_ctx: &RuntimeLaunchContext,
    runtime: &RuntimeIdentityV2,
) -> Option<LocalExecutionLocator> {
    let manifest_path = Some(plan.manifest_path.display().to_string());
    let workspace_root = Some(plan.workspace_root.display().to_string());
    let working_directory_path = Some(launch_spec.working_dir.display().to_string());
    let runtime_resolved_path = runtime
        .resolved_ref
        .value
        .as_ref()
        .filter(|value| value.contains('/'))
        .cloned();
    let entry_point_raw = (!launch_spec.command.is_empty()).then(|| launch_spec.command.clone());

    let mut argv_raw = launch_spec.args.clone();
    argv_raw.extend(launch_ctx.command_args().iter().cloned());

    Some(LocalExecutionLocator {
        manifest_path,
        workspace_root,
        working_directory_path,
        runtime_resolved_path,
        state_paths: state_paths_for_locator(plan),
        entry_point_raw,
        argv_raw,
    })
}

fn state_paths_for_locator(plan: &ManifestData) -> BTreeMap<String, String> {
    plan.state_source_overrides
        .iter()
        .filter_map(|(name, locator)| {
            if Path::new(locator).is_absolute() {
                Some((name.clone(), locator.clone()))
            } else {
                None
            }
        })
        .collect()
}

pub(crate) fn build_policy_identity_v2(execution_plan: &ExecutionPlan) -> PolicyIdentityV2 {
    PolicyIdentityV2 {
        network_policy_hash: Tracked::known(
            execution_plan.consent.provisioning_policy_hash.clone(),
        ),
        capability_policy_hash: Tracked::known(execution_plan.consent.policy_segment_hash.clone()),
        sandbox_policy_hash: Tracked::known(sandbox_policy_hash_v2(execution_plan)),
    }
}

#[derive(serde::Serialize)]
struct SandboxPolicyHashInputV2<'a> {
    target_runtime: &'a str,
    target_driver: &'a str,
    fail_closed: bool,
    mount_set_algo_id: &'a str,
    mount_set_algo_version: u32,
    network_mode: &'a str,
    allow_hosts_count: usize,
}

fn sandbox_policy_hash_v2(execution_plan: &ExecutionPlan) -> String {
    let input = SandboxPolicyHashInputV2 {
        target_runtime: execution_plan.target.runtime.as_str(),
        target_driver: execution_plan.target.driver.as_str(),
        fail_closed: execution_plan.runtime.fail_closed,
        mount_set_algo_id: execution_plan.consent.mount_set_algo_id.as_str(),
        mount_set_algo_version: execution_plan.consent.mount_set_algo_version,
        network_mode: if execution_plan.runtime.policy.network.allow_hosts.is_empty() {
            "deny"
        } else {
            "allowlist"
        },
        allow_hosts_count: execution_plan.runtime.policy.network.allow_hosts.len(),
    };
    let canonical = serde_jcs::to_vec(&input).unwrap_or_default();
    format!("blake3:{}", blake3::hash(&canonical).to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_runtime_resolved_ref_prefers_declared_short_name() {
        let tracked = build_runtime_resolved_ref(Some("node@20"), Some("/usr/bin/node"));
        assert_eq!(tracked.value.as_deref(), Some("node@20"));
    }

    #[test]
    fn build_runtime_resolved_ref_strips_path_when_only_resolved_path_known() {
        let tracked =
            build_runtime_resolved_ref(None, Some("/Users/alice/.ato/runtimes/node/20/bin/node"));
        assert_eq!(tracked.value.as_deref(), Some("node"));
    }

    #[test]
    fn build_runtime_resolved_ref_untracked_when_only_path_segments_present() {
        let tracked = build_runtime_resolved_ref(Some("/usr/local/node/bin/node"), None);
        assert!(matches!(
            tracked.status,
            capsule_core::execution_identity::TrackingStatus::Untracked
        ));
    }

    #[test]
    fn classify_state_binding_recognizes_known_kinds() {
        assert!(matches!(
            classify_state_binding("state-abc123"),
            StateBindingKind::AtoStateRef
        ));
        assert!(matches!(
            classify_state_binding("blake3:abc"),
            StateBindingKind::AtoStateRef
        ));
        assert!(matches!(
            classify_state_binding("/Users/alice/state"),
            StateBindingKind::HostPath
        ));
        assert!(matches!(
            classify_state_binding("relative/path"),
            StateBindingKind::ContentSnapshot
        ));
    }

    #[test]
    fn detect_registry_ref_extracts_scoped_id_from_runtimes_path() {
        let mut plan = sample_plan(PathBuf::from(
            "/Users/alice/.ato/runtimes/koh0920/tiddlywiki/5.4.1/cap",
        ));
        let provenance = observe_source_provenance(&plan);
        assert_eq!(
            provenance.registry_ref.as_deref(),
            Some("registry:koh0920/tiddlywiki/5.4.1/cap")
        );
        assert!(matches!(provenance.kind, SourceProvenanceKind::Local));

        plan.workspace_root = PathBuf::from("/Users/alice/proj");
        let provenance = observe_source_provenance(&plan);
        assert!(provenance.registry_ref.is_none());
    }

    #[test]
    fn v2_source_identity_is_workspace_portable() {
        use std::fs;
        use tempfile::tempdir;

        // Two distinct workspace roots with the same source contents should
        // produce the same source_tree_hash and the same manifest_path_role
        // ("workspace:capsule.toml"). This is the core v2 portability claim.
        let alice = tempdir().expect("alice tempdir");
        let bob = tempdir().expect("bob tempdir");
        for root in [alice.path(), bob.path()] {
            fs::write(root.join("main.py"), "print('hello')\n").expect("write source");
            fs::write(
                root.join("capsule.toml"),
                "schema_version = \"0.3\"\nname = \"x\"\nversion = \"0.1.0\"\ntype = \"app\"\ndefault_target = \"app\"\n\n[targets.app]\nruntime = \"source\"\ndriver = \"python\"\nruntime_version = \"3.11\"\nrun = \"main.py\"\n",
            )
            .expect("write manifest");
        }
        let alice_plan = sample_plan(alice.path().to_path_buf());
        let bob_plan = sample_plan(bob.path().to_path_buf());

        let alice_ctx = ObserverContextV2::for_plan(&alice_plan);
        let bob_ctx = ObserverContextV2::for_plan(&bob_plan);

        let alice_source = observe_source_v2(&alice_plan, &alice_ctx).expect("alice");
        let bob_source = observe_source_v2(&bob_plan, &bob_ctx).expect("bob");

        assert_eq!(
            alice_source.source_tree_hash.value, bob_source.source_tree_hash.value,
            "same source contents under different absolute roots must hash equally"
        );
        assert_eq!(
            alice_source.manifest_path_role.value.as_deref(),
            Some("workspace:capsule.toml")
        );
        assert_eq!(
            alice_source.manifest_path_role.value,
            bob_source.manifest_path_role.value
        );
    }

    #[test]
    fn v2_local_locator_does_not_leak_into_identity_when_paths_match_role() {
        // Even though manifest_path and workspace_root differ between hosts,
        // the v2 launch working_directory must canonicalize to "workspace:."
        // and not embed the host-local absolute path in the hash.
        use tempfile::tempdir;
        let alice = tempdir().expect("alice");
        let bob = tempdir().expect("bob");
        let alice_plan = sample_plan(alice.path().to_path_buf());
        let bob_plan = sample_plan(bob.path().to_path_buf());

        let alice_ctx = ObserverContextV2::for_plan(&alice_plan);
        let bob_ctx = ObserverContextV2::for_plan(&bob_plan);

        let alice_role = alice_ctx.role_for_path(&alice_plan.workspace_root);
        let bob_role = bob_ctx.role_for_path(&bob_plan.workspace_root);

        assert_eq!(alice_role.value.as_deref(), Some("workspace:."));
        assert_eq!(alice_role.value, bob_role.value);
    }

    fn sample_plan(workspace_root: PathBuf) -> ManifestData {
        // Construct a minimal ExecutionDescriptor by repurposing
        // execution_observers test plumbing would be heavyweight here. The
        // detect_registry_ref function only inspects workspace_root, so build a
        // ManifestData via the public constructor used in execution_observers
        // tests.
        let manifest = format!(
            r#"
schema_version = "0.3"
name = "test"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run = "main.py"
"#,
        );
        let manifest_path = workspace_root.join("capsule.toml");
        let parsed: toml::Value = toml::from_str(&manifest).expect("parse manifest");
        capsule_core::router::execution_descriptor_from_manifest_parts(
            parsed,
            manifest_path,
            workspace_root,
            capsule_core::router::ExecutionProfile::Dev,
            Some("app"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor")
    }
}
