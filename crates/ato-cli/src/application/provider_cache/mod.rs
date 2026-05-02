//! Cache integration helpers that the synthetic provider workspaces wrap
//! their install commands with.
//!
//! Each provider materialization (`materialize_pypi_workspace`,
//! `materialize_npm_workspace`, …) produces a dependency tree at a known
//! location (`site-packages/`, `node_modules/`, …) by running an external
//! tool (`uv pip sync`, `npm install`, …). To plug into the A1 derivation
//! cache without rewiring the entire pipeline, we expose two narrow hooks:
//!
//! - [`check_and_project`] — compute `derivation_hash`, look up the cache,
//!   and (on hit) project the cached payload into the deps directory.
//!   The caller skips its install command on hit.
//! - [`freeze_after_install`] — after a successful install, freeze the
//!   resulting tree into the immutable store.
//!
//! ## Cache strategy resolution
//!
//! The provider materialization runs much earlier than the dependency
//! materializer in the run pipeline, so it does not yet have access to the
//! resolved [`CacheStrategy`]. To avoid deep plumbing for the A1 default
//! flip, we read `ATO_CACHE_STRATEGY` directly here using the same
//! [`CacheStrategyArg::Auto`] resolution rules. CLI plumbing can replace
//! this in a follow-up.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use capsule_core::common::store::BlobAddress;

use crate::application::dependency_materializer::freeze::{freeze_dep_tree, FreezeOutcome};
use crate::application::dependency_materializer::{
    CacheStrategy, DepDerivationKeyV1, DependencyMaterializationRequest, DependencyMaterializer,
    InstallPolicies, ManifestInputs, PlatformTriple, RuntimeSelection,
    SessionDependencyMaterializer,
};
use crate::application::projection::{project_payload, ProjectionOutcome};
use crate::cli::shared::CacheStrategyArg;

/// Inputs needed to compute a derivation hash for a provider workspace.
#[derive(Debug, Clone)]
pub struct ProviderCacheInputs<'a> {
    /// `"pypi"` or `"npm"` (kept aligned with the materializer's ecosystem
    /// vocabulary so the cache index stays consistent).
    pub ecosystem: &'a str,
    /// `"uv"`, `"npm"`, `"pnpm"`, `"bun"`, ...
    pub package_manager: Option<&'a str>,
    pub package_manager_version: Option<&'a str>,
    pub runtime: RuntimeSelection,
    /// Path to the lockfile that pins this install. Must exist before
    /// `check_and_project` is called.
    pub lockfile_path: Option<&'a Path>,
    /// Path to the manifest (package.json, requirements.txt, pyproject.toml, …).
    pub manifest_path: Option<&'a Path>,
    /// Network enforcement string; flows through the install policy digest.
    pub network_policy: &'a str,
}

/// Outcome of [`check_and_project`].
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProviderCachePlan {
    pub derivation_hash: Option<String>,
    pub action: ProviderCacheAction,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ProviderCacheAction {
    /// The cache had a matching blob and it has been projected into the
    /// deps directory. The caller MUST NOT run the install.
    ///
    /// Fields are exposed for tracing / debugging by current call sites; not
    /// every caller reads them.
    Hit {
        blob_hash: String,
        projection: ProjectionOutcome,
    },
    /// The cache did not have a matching entry. The caller MUST run the
    /// install and then call [`freeze_after_install`] with the returned
    /// `derivation_hash`.
    Miss { derivation_hash: String },
    /// Caching is disabled for this run. The caller MUST run the install
    /// and SHOULD NOT call [`freeze_after_install`] (it would be a no-op).
    Disabled,
}

/// Reads `ATO_CACHE_STRATEGY` and returns the resolved strategy.
///
/// Honors the same rules as `CacheStrategyArg::Auto`, so the global default
/// flip in `cli::shared` automatically reaches the provider materialization
/// path without further wiring.
fn current_cache_strategy() -> CacheStrategy {
    CacheStrategyArg::Auto.resolve()
}

/// Reads a file and returns its `sha256:<hex>` digest, or `None` if the file
/// is absent.
fn digest_optional_file(path: Option<&Path>) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(&bytes))
    )))
}

/// Builds a minimal materialization request whose only purpose is to feed
/// the derivation key calculator.
fn build_materialization_request(
    inputs: &ProviderCacheInputs<'_>,
    workspace_root: &Path,
) -> Result<DependencyMaterializationRequest> {
    let lockfile_digest = digest_optional_file(inputs.lockfile_path)?;
    let manifest_digest = digest_optional_file(inputs.manifest_path)?;
    Ok(DependencyMaterializationRequest {
        // The session/capsule ids are not part of the derivation hash; pass
        // sentinel values that make traces self-explanatory.
        session_id: "provider-cache".to_string(),
        capsule_id: format!(
            "provider-{}-{}",
            inputs.ecosystem,
            workspace_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
        ),
        source_root: workspace_root.to_path_buf(),
        workspace_root: workspace_root.to_path_buf(),
        ecosystem: inputs.ecosystem.to_string(),
        package_manager: inputs.package_manager.map(str::to_string),
        package_manager_version: inputs.package_manager_version.map(str::to_string),
        runtime: inputs.runtime.clone(),
        manifests: ManifestInputs {
            lockfile_digest,
            package_manifest_digest: manifest_digest,
            workspace_manifest_digest: None,
            path_dependency_digest: None,
        },
        policies: InstallPolicies {
            lifecycle_script_policy: "sandbox".to_string(),
            registry_policy: "default".to_string(),
            network_policy: inputs.network_policy.to_string(),
            env_allowlist_digest: None,
        },
        platform: PlatformTriple::current(),
        cache_strategy: CacheStrategy::DerivationCache,
        attestation_strategy:
            crate::application::dependency_materializer::AttestationStrategy::None,
    })
}

/// Decides whether to project from the cache or fall through to a fresh
/// install.
///
/// `deps_path` is the directory the install would normally write to (for
/// pypi it is `<workspace>/site-packages`, for npm it is
/// `<workspace>/node_modules`). The directory may already exist as empty
/// scaffolding; this helper deletes it before projecting on hit so the
/// projection's "no existing target" rule is satisfied.
pub fn check_and_project(
    workspace_root: &Path,
    deps_path: &Path,
    inputs: &ProviderCacheInputs<'_>,
) -> Result<ProviderCachePlan> {
    let strategy = current_cache_strategy();
    if matches!(strategy, CacheStrategy::None) {
        tracing::debug!(
            ecosystem = inputs.ecosystem,
            "provider_cache: strategy=none, skipping cache lookup"
        );
        return Ok(ProviderCachePlan {
            derivation_hash: None,
            action: ProviderCacheAction::Disabled,
        });
    }

    let request = build_materialization_request(inputs, workspace_root)?;
    let materializer = SessionDependencyMaterializer::new();
    let plan = materializer.plan(&request)?;
    let derivation_hash = plan.derivation_hash.clone();

    use crate::application::dependency_materializer::CacheLookupResult;
    match plan.cache_lookup {
        CacheLookupResult::Hit { blob_hash } => {
            let address = BlobAddress::parse(&blob_hash)
                .with_context(|| format!("blob hash {blob_hash} could not be parsed"))?;
            // Remove the empty scaffolding the provider may have already
            // created so projection's "target must not exist" guard passes.
            if deps_path.exists() {
                let mut entries = fs::read_dir(deps_path)
                    .with_context(|| format!("failed to read {}", deps_path.display()))?;
                if entries.next().is_none() {
                    fs::remove_dir(deps_path).with_context(|| {
                        format!("failed to remove empty scaffolding {}", deps_path.display())
                    })?;
                }
            }
            let projection =
                project_payload(&address.payload_dir(), deps_path).with_context(|| {
                    format!(
                        "failed to project blob {blob_hash} into {}",
                        deps_path.display()
                    )
                })?;
            tracing::info!(
                ecosystem = inputs.ecosystem,
                derivation_hash = %derivation_hash,
                blob_hash = %blob_hash,
                cache_result = "hit",
                "provider_cache: projected cached deps, skipping install"
            );
            Ok(ProviderCachePlan {
                derivation_hash: Some(derivation_hash),
                action: ProviderCacheAction::Hit {
                    blob_hash,
                    projection,
                },
            })
        }
        CacheLookupResult::Miss => {
            tracing::info!(
                ecosystem = inputs.ecosystem,
                derivation_hash = %derivation_hash,
                cache_result = "miss",
                "provider_cache: cache miss, install will run"
            );
            Ok(ProviderCachePlan {
                derivation_hash: Some(derivation_hash.clone()),
                action: ProviderCacheAction::Miss { derivation_hash },
            })
        }
        CacheLookupResult::Disabled => Ok(ProviderCachePlan {
            derivation_hash: Some(derivation_hash),
            action: ProviderCacheAction::Disabled,
        }),
    }
}

/// Freezes the install output into the immutable store.
///
/// Idempotent: a second call with the same `derivation_hash` and an
/// identical tree observes the existing blob without rewriting bytes.
pub fn freeze_after_install(
    deps_path: &Path,
    derivation_hash: &str,
    ecosystem: &str,
) -> Result<FreezeOutcome> {
    freeze_dep_tree(deps_path, derivation_hash, ecosystem)
}

/// Computes the derivation hash for a provider workspace without performing
/// any cache lookup or projection. Used by callers that always run the
/// install (e.g. npm where lockfile generation is bundled with install) but
/// still want to freeze the result.
pub fn compute_derivation_hash(
    workspace_root: &Path,
    inputs: &ProviderCacheInputs<'_>,
) -> Result<String> {
    let request = build_materialization_request(inputs, workspace_root)?;
    DepDerivationKeyV1::from_request(&request).derivation_hash()
}

/// Returns true when caching is currently enabled. Provider materialization
/// can short-circuit freeze when this is `false`.
pub fn cache_enabled() -> bool {
    !matches!(current_cache_strategy(), CacheStrategy::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set<V: AsRef<OsStr>>(key: &'static str, value: V) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn write_file(root: &Path, rel: &str, contents: &[u8]) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn sample_inputs<'a>(lockfile: &'a Path, manifest: &'a Path) -> ProviderCacheInputs<'a> {
        ProviderCacheInputs {
            ecosystem: "pypi",
            package_manager: Some("uv"),
            package_manager_version: Some("0.4.0"),
            runtime: RuntimeSelection {
                name: "python".to_string(),
                version: Some("3.12.0".to_string()),
            },
            lockfile_path: Some(lockfile),
            manifest_path: Some(manifest),
            network_policy: "default",
        }
    }

    #[test]
    #[serial_test::serial]
    fn check_returns_disabled_when_strategy_is_none() {
        let tmp = TempDir::new().unwrap();
        let _home = EnvGuard::set("ATO_HOME", tmp.path());
        let _strategy = EnvGuard::set("ATO_CACHE_STRATEGY", "none");

        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let lockfile = workspace.join("uv.lock");
        let manifest = workspace.join("requirements.txt");
        fs::write(&lockfile, b"# generated\n").unwrap();
        fs::write(&manifest, b"requests==2.0\n").unwrap();

        let plan = check_and_project(
            &workspace,
            &workspace.join("site-packages"),
            &sample_inputs(&lockfile, &manifest),
        )
        .unwrap();
        assert!(matches!(plan.action, ProviderCacheAction::Disabled));
    }

    #[test]
    #[serial_test::serial]
    fn check_returns_miss_when_no_blob_exists_yet() {
        let tmp = TempDir::new().unwrap();
        let _home = EnvGuard::set("ATO_HOME", tmp.path());
        std::env::remove_var("ATO_CACHE_STRATEGY");

        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let lockfile = workspace.join("uv.lock");
        let manifest = workspace.join("requirements.txt");
        fs::write(&lockfile, b"# generated\n").unwrap();
        fs::write(&manifest, b"requests==2.0\n").unwrap();

        let plan = check_and_project(
            &workspace,
            &workspace.join("site-packages"),
            &sample_inputs(&lockfile, &manifest),
        )
        .unwrap();
        match plan.action {
            ProviderCacheAction::Miss { derivation_hash } => {
                assert!(derivation_hash.starts_with("sha256:"));
            }
            other => panic!("expected miss, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn freeze_then_check_yields_hit_and_projects() {
        let tmp = TempDir::new().unwrap();
        let _home = EnvGuard::set("ATO_HOME", tmp.path());
        std::env::remove_var("ATO_CACHE_STRATEGY");

        let workspace = tmp.path().join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let lockfile = workspace.join("uv.lock");
        let manifest = workspace.join("requirements.txt");
        fs::write(&lockfile, b"# pinned\n").unwrap();
        fs::write(&manifest, b"foo==1.0\n").unwrap();

        // Cold: simulate install output and freeze it.
        let deps = workspace.join("site-packages");
        write_file(&deps, "foo/__init__.py", b"# foo\n");
        write_file(&deps, "foo-1.0.dist-info/METADATA", b"Name: foo\n");

        let inputs = sample_inputs(&lockfile, &manifest);
        let cold_plan = check_and_project(&workspace, &deps, &inputs).unwrap();
        let derivation_hash = match cold_plan.action {
            ProviderCacheAction::Miss { derivation_hash } => derivation_hash,
            other => panic!("expected miss on cold run, got {other:?}"),
        };
        let outcome = freeze_after_install(&deps, &derivation_hash, "pypi").unwrap();
        assert!(outcome.did_freeze);

        // Warm: a fresh workspace with an EMPTY site-packages should hit
        // and have its scaffolding replaced by the projection.
        let warm_workspace = tmp.path().join("warm");
        fs::create_dir_all(&warm_workspace).unwrap();
        let warm_lockfile = warm_workspace.join("uv.lock");
        let warm_manifest = warm_workspace.join("requirements.txt");
        fs::copy(&lockfile, &warm_lockfile).unwrap();
        fs::copy(&manifest, &warm_manifest).unwrap();
        let warm_deps = warm_workspace.join("site-packages");
        fs::create_dir_all(&warm_deps).unwrap();

        let warm_inputs = sample_inputs(&warm_lockfile, &warm_manifest);
        let warm_plan = check_and_project(&warm_workspace, &warm_deps, &warm_inputs).unwrap();
        match warm_plan.action {
            ProviderCacheAction::Hit { blob_hash, .. } => {
                assert_eq!(blob_hash, outcome.blob_hash);
            }
            other => panic!("expected hit on warm run, got {other:?}"),
        }
        assert!(warm_deps.join("foo/__init__.py").is_file());
        assert!(warm_deps.join("foo-1.0.dist-info/METADATA").is_file());
    }
}
