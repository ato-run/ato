use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::execution_plan::error::AtoExecutionError;
use crate::execution_plan::model::{NonInteractiveBehavior, Provisioning, Runtime, RuntimePolicy};
use crate::security::path::validate_path;

#[derive(Serialize)]
struct PolicyHashInput<'a> {
    runtime_policy: &'a RuntimePolicy,
    runtime_fail_closed: bool,
    non_interactive_behavior: &'a NonInteractiveBehavior,
    mount_set_algo_id: &'a str,
    mount_set_algo_version: u32,
}

#[derive(Serialize)]
struct ProvisioningHashInput<'a> {
    provisioning: &'a Provisioning,
}

pub fn canonical_hash<T: Serialize>(value: &T) -> Result<String, AtoExecutionError> {
    let canonical = serde_jcs::to_vec(value).map_err(|err| {
        AtoExecutionError::internal(format!("failed to canonicalize execution plan JSON: {err}"))
    })?;
    let digest = blake3::hash(&canonical);
    Ok(format!("blake3:{}", digest.to_hex()))
}

pub fn compute_policy_segment_hash(
    runtime: &Runtime,
    mount_set_algo_id: &str,
    mount_set_algo_version: u32,
) -> Result<String, AtoExecutionError> {
    canonical_hash(&PolicyHashInput {
        runtime_policy: &runtime.policy,
        runtime_fail_closed: runtime.fail_closed,
        non_interactive_behavior: &runtime.non_interactive_behavior,
        mount_set_algo_id,
        mount_set_algo_version,
    })
}

pub fn compute_provisioning_policy_hash(
    provisioning: &Provisioning,
) -> Result<String, AtoExecutionError> {
    canonical_hash(&ProvisioningHashInput { provisioning })
}

pub fn normalize_unordered_set(values: &[String]) -> Vec<String> {
    let mut out = values
        .iter()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

pub fn canonicalize_policy_paths(
    project_root: &Path,
    paths: &[String],
) -> Result<Vec<String>, AtoExecutionError> {
    let mut out = Vec::with_capacity(paths.len());
    for raw in paths {
        out.push(canonicalize_path(project_root, raw)?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

pub fn canonicalize_path(project_root: &Path, raw_path: &str) -> Result<String, AtoExecutionError> {
    let root_input = if project_root.is_absolute() {
        project_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| {
                AtoExecutionError::policy_violation(format!(
                    "failed to resolve current directory for '{}': {}",
                    project_root.display(),
                    err
                ))
            })?
            .join(project_root)
    };
    let root = canonicalize_existing_or_ancestor(&root_input).map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "failed to canonicalize project root '{}': {}",
            project_root.display(),
            err
        ))
    })?;

    if raw_path == "~" || raw_path.starts_with("~/") || raw_path.starts_with("~\\") {
        return Err(AtoExecutionError::policy_violation(format!(
            "path canonicalization denied for '{}': home-directory aliases are not allowed",
            raw_path
        )));
    }

    let path = PathBuf::from(raw_path);
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AtoExecutionError::policy_violation(format!(
            "path canonicalization denied for '{}': parent traversal (..) is not allowed",
            raw_path
        )));
    }

    let absolute = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };

    validate_path(
        &absolute.to_string_lossy(),
        &[root.to_string_lossy().to_string()],
    )
    .map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "path canonicalization denied for '{}': {}",
            raw_path, err
        ))
    })?;

    let canonical = canonicalize_existing_or_ancestor(&absolute).map_err(|err| {
        AtoExecutionError::policy_violation(format!(
            "failed to canonicalize '{}': {}",
            raw_path, err
        ))
    })?;

    Ok(canonical.to_string_lossy().to_string())
}

fn canonicalize_existing_or_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return path.canonicalize();
    }

    let mut current = path;
    while !current.exists() {
        current = current
            .parent()
            .ok_or_else(|| std::io::Error::other("missing existing ancestor"))?;
    }

    let canonical_prefix = current.canonicalize()?;
    let remainder = path
        .strip_prefix(current)
        .map_err(|_| std::io::Error::other("failed to strip canonical prefix"))?;

    Ok(canonical_prefix.join(remainder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_plan::model::{
        NonInteractiveBehavior, Provisioning, ProvisioningNetwork, Runtime,
        RuntimeFilesystemPolicy, RuntimeNetworkPolicy, RuntimePolicy, RuntimeSecretsPolicy,
        SecretDelivery,
    };
    use std::sync::{Mutex, OnceLock};

    fn cwd_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn sample_runtime(args: Vec<&str>, allow_hosts: Vec<&str>, read_only: Vec<&str>) -> Runtime {
        Runtime {
            policy: RuntimePolicy {
                network: RuntimeNetworkPolicy {
                    allow_hosts: allow_hosts.into_iter().map(|v| v.to_string()).collect(),
                },
                filesystem: RuntimeFilesystemPolicy {
                    read_only: read_only.into_iter().map(|v| v.to_string()).collect(),
                    read_write: vec![],
                },
                secrets: RuntimeSecretsPolicy {
                    allow_secret_ids: vec![],
                    delivery: SecretDelivery::Fd,
                },
                args: args.into_iter().map(|v| v.to_string()).collect(),
            },
            fail_closed: true,
            non_interactive_behavior: NonInteractiveBehavior::DenyIfUnconsented,
        }
    }

    #[test]
    fn hash_is_stable_for_order_independent_sets() {
        let mut runtime_a = sample_runtime(
            vec!["main.ts"],
            vec!["registry.npmjs.org", "deno.land"],
            vec!["/tmp/a", "/tmp/b"],
        );
        let mut runtime_b = sample_runtime(
            vec!["main.ts"],
            vec!["deno.land", "registry.npmjs.org"],
            vec!["/tmp/b", "/tmp/a"],
        );

        runtime_a.policy.network.allow_hosts =
            normalize_unordered_set(&runtime_a.policy.network.allow_hosts);
        runtime_b.policy.network.allow_hosts =
            normalize_unordered_set(&runtime_b.policy.network.allow_hosts);
        runtime_a.policy.filesystem.read_only =
            normalize_unordered_set(&runtime_a.policy.filesystem.read_only);
        runtime_b.policy.filesystem.read_only =
            normalize_unordered_set(&runtime_b.policy.filesystem.read_only);

        let left = compute_policy_segment_hash(&runtime_a, "lockfile_mountset_v1", 1).unwrap();
        let right = compute_policy_segment_hash(&runtime_b, "lockfile_mountset_v1", 1).unwrap();

        assert_eq!(left, right);
    }

    #[test]
    fn hash_keeps_order_for_runtime_args() {
        let runtime_a = sample_runtime(vec!["a", "b"], vec![], vec![]);
        let runtime_b = sample_runtime(vec!["b", "a"], vec![], vec![]);

        let left = compute_policy_segment_hash(&runtime_a, "lockfile_mountset_v1", 1).unwrap();
        let right = compute_policy_segment_hash(&runtime_b, "lockfile_mountset_v1", 1).unwrap();

        assert_ne!(left, right);
    }

    #[test]
    fn canonicalization_changes_hash_when_effective_path_differs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("public")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();

        let left = canonicalize_policy_paths(root, &["./public".to_string()]).unwrap();
        let right = canonicalize_policy_paths(root, &["./assets".to_string()]).unwrap();
        assert_ne!(left, right);

        let provisioning = Provisioning {
            network: ProvisioningNetwork {
                allow_registry_hosts: vec!["deno.land".to_string()],
            },
            lock_required: true,
            integrity_required: true,
            allowed_registries: vec!["deno.land".to_string()],
        };

        let hash_a = compute_provisioning_policy_hash(&provisioning).unwrap();
        let hash_b = compute_provisioning_policy_hash(&provisioning).unwrap();
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn canonicalize_path_allows_relative_input_with_relative_project_root() {
        let _guard = cwd_lock().lock().unwrap();
        let previous_cwd = std::env::current_dir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::env::set_current_dir(temp.path()).unwrap();

        let resolved = canonicalize_path(Path::new("workspace"), "dist").unwrap();
        let expected = root.canonicalize().unwrap().join("dist");
        assert_eq!(resolved, expected.to_string_lossy());

        std::env::set_current_dir(previous_cwd).unwrap();
    }

    #[test]
    fn canonicalize_path_rejects_parent_traversal_alias() {
        let temp = tempfile::tempdir().unwrap();
        let err = canonicalize_path(temp.path(), "../dist").unwrap_err();
        assert!(err.message.contains("parent traversal (..) is not allowed"));
    }

    #[test]
    fn canonicalize_path_rejects_home_alias() {
        let temp = tempfile::tempdir().unwrap();
        let err = canonicalize_path(temp.path(), "~/dist").unwrap_err();
        assert!(err
            .message
            .contains("home-directory aliases are not allowed"));
    }
}
