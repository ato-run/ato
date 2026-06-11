#![allow(clippy::result_large_err)]

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::execution_plan::error::AtoExecutionError;
use crate::execution_plan::model::{
    Consent, NonInteractiveBehavior, Provisioning, Runtime, RuntimePolicy,
};
use crate::security::path::validate_path;

/// Schema tag baked into the `consent_ref` hash input so a future change to the
/// consent key structure cannot collide with or be misread as an older ref.
pub const CONSENT_REF_SCHEMA: &str = "execution_plan_consent_v1";

#[derive(Serialize)]
struct ConsentRefInput<'a> {
    schema: &'a str,
    scoped_id: &'a str,
    version: &'a str,
    target_label: &'a str,
    policy_segment_hash: &'a str,
    provisioning_policy_hash: &'a str,
}

/// Stable, display-friendly reference for an ExecutionPlan consent decision:
/// `blake3` over the JCS of the schema + the identity 5-tuple (scoped_id,
/// version, target_label, policy_segment_hash, provisioning_policy_hash). The
/// wire payload and host-local ledger keep the FULL 5-tuple — this ref is only
/// for display and for binding an owner's approval to the exact policy.
pub fn consent_ref(consent: &Consent) -> Result<String, AtoExecutionError> {
    consent_ref_from_parts(
        &consent.key.scoped_id,
        &consent.key.version,
        &consent.key.target_label,
        &consent.policy_segment_hash,
        &consent.provisioning_policy_hash,
    )
}

/// Recompute the consent_ref from the identity 5-tuple alone, without a full
/// `Consent`. The connected runner uses this to verify an owner's approved
/// consent_ref against the exact policy the child gated on, BEFORE it writes its
/// host-local ledger: an approval bound to any other 5-tuple hashes to a
/// different ref and is refused. Kept byte-identical to [`consent_ref`] by
/// sharing `ConsentRefInput` + `CONSENT_REF_SCHEMA`.
pub fn consent_ref_from_parts(
    scoped_id: &str,
    version: &str,
    target_label: &str,
    policy_segment_hash: &str,
    provisioning_policy_hash: &str,
) -> Result<String, AtoExecutionError> {
    canonical_hash(&ConsentRefInput {
        schema: CONSENT_REF_SCHEMA,
        scoped_id,
        version,
        target_label,
        policy_segment_hash,
        provisioning_policy_hash,
    })
}

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
        Consent, ConsentKey, NonInteractiveBehavior, Provisioning, ProvisioningNetwork, Runtime,
        RuntimeFilesystemPolicy, RuntimeNetworkPolicy, RuntimePolicy, RuntimeSecretsPolicy,
        SecretDelivery,
    };
    use std::sync::{Mutex, OnceLock};

    fn sample_consent() -> Consent {
        Consent {
            key: ConsentKey {
                scoped_id: "community/hello-capsule".into(),
                version: "0.3.0".into(),
                target_label: "main".into(),
            },
            policy_segment_hash: "blake3:seg".into(),
            provisioning_policy_hash: "blake3:prov".into(),
            mount_set_algo_id: "algo".into(),
            mount_set_algo_version: 1,
        }
    }

    #[test]
    fn consent_ref_is_stable_schema_bound_and_field_sensitive() {
        let base = sample_consent();
        let r = consent_ref(&base).expect("ref");
        assert!(r.starts_with("blake3:"), "ref is a blake3 digest: {r}");
        assert_eq!(r, consent_ref(&base).expect("ref"), "stable for same input");

        // Changing ANY of the identity 5-tuple fields MUST change the ref
        // (exact binding is what the downstream owner-approval relies on).
        let mutate: [(&str, fn(&mut Consent)); 5] = [
            ("scoped_id", |c| c.key.scoped_id = "community/other".into()),
            ("version", |c| c.key.version = "9.9.9".into()),
            ("target_label", |c| c.key.target_label = "web".into()),
            ("policy_segment_hash", |c| {
                c.policy_segment_hash = "blake3:SEG2".into()
            }),
            ("provisioning_policy_hash", |c| {
                c.provisioning_policy_hash = "blake3:PROV2".into()
            }),
        ];
        for (field, apply) in mutate {
            let mut c = base.clone();
            apply(&mut c);
            assert_ne!(
                consent_ref(&c).expect("ref"),
                r,
                "consent_ref must change when {field} changes"
            );
        }

        // mount_set_algo_* is NOT part of the consent identity → ref unchanged.
        let mut algo = base.clone();
        algo.mount_set_algo_version = 99;
        algo.mount_set_algo_id = "other".into();
        assert_eq!(consent_ref(&algo).expect("ref"), r);
    }

    #[test]
    fn consent_ref_from_parts_matches_consent_ref() {
        // The connected runner recomputes the ref from the 5-tuple it received
        // (no full Consent) to verify an owner's approval before writing its
        // host-local ledger. That recompute MUST be byte-identical to the ref
        // produced from the Consent, or every approval would falsely mismatch.
        let base = sample_consent();
        let from_consent = consent_ref(&base).expect("ref");
        let from_parts = consent_ref_from_parts(
            &base.key.scoped_id,
            &base.key.version,
            &base.key.target_label,
            &base.policy_segment_hash,
            &base.provisioning_policy_hash,
        )
        .expect("ref from parts");
        assert_eq!(from_parts, from_consent);

        // A different 5-tuple yields a different ref — the verification gate.
        let other = consent_ref_from_parts(
            &base.key.scoped_id,
            "9.9.9",
            &base.key.target_label,
            &base.policy_segment_hash,
            &base.provisioning_policy_hash,
        )
        .expect("ref from parts");
        assert_ne!(other, from_consent);
    }

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
    #[serial_test::serial]
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
        assert!(
            err.message
                .contains("home-directory aliases are not allowed")
        );
    }
}
