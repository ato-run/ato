use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::{CapsuleError, Result};
use crate::types::CapsuleManifest;

/// Controls which files are allowed into a capsule archive.
///
/// - `Source` (default for `ato publish`): source files and manifest metadata only.
///   Dependency outputs and build artifacts are excluded by invariant, not denylist.
///   Next.js `.next/standalone/node_modules` is excluded under this profile.
/// - `Artifact`: allows build/runtime-complete artifacts through.
///   The legacy pack behavior — use for `ato publish --profile artifact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublishProfile {
    #[default]
    Source,
    Artifact,
}

const SMART_DEFAULT_EXCLUDES: &[&str] = &[
    ".git/**",
    ".svn/**",
    ".hg/**",
    ".capsule/**",
    "target/**",
    "**/node_modules/**",
    "**/bower_components/**",
    "**/.venv/**",
    "**/venv/**",
    "**/env/**",
    "**/__pycache__/**",
    "**/.pytest_cache/**",
    ".next/cache/**",
    "**/.next/cache/**",
    ".turbo/**",
    "**/.turbo/**",
    ".wrangler/**",
    "**/.wrangler/**",
    ".DS_Store",
    "**/.DS_Store",
    "Thumbs.db",
    "**/Thumbs.db",
    "**/*.capsule",
    "**/*.sig",
    ".ato/**",
    "**/.ato/**",
    ".tmp/**",
    "**/.tmp/**",
    ".ato.run.generated.capsule.toml",
    "ato.lock.json",
    ".capsule.lock.inputs.json",
    "**/.capsule.lock.inputs.json",
    "capsule.toml",
    "config.json",
    "capsule.lock.json",
    "capsule.lock",
    "signature.json",
    "payload.v3.manifest.json",
    "payload.tar",
    "payload.tar.zst",
    // Secret / config files — never include in capsule archives
    ".env",
    "**/.env",
    ".env.*",
    "**/.env.*",
    ".envrc",
    "**/.envrc",
    // Private keys and credentials
    "**/*.pem",
    "**/*.key",
    "**/*.p12",
    "**/*.pfx",
    "**/credentials.json",
    "**/service-account*.json",
    "**/.netrc",
    "**/.npmrc",
    "**/.pypirc",
];

#[derive(Debug, Clone)]
pub struct PackFilter {
    include: Option<GlobSet>,
    exclude: GlobSet,
    profile: PublishProfile,
}

impl PackFilter {
    pub fn from_manifest_path(manifest_path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(manifest_path).map_err(CapsuleError::Io)?;
        let manifest = CapsuleManifest::from_toml(&raw).map_err(|e| {
            CapsuleError::Pack(format!(
                "Failed to parse {}: {}",
                manifest_path.display(),
                e
            ))
        })?;
        Self::from_manifest(&manifest)
    }

    /// Build a `PackFilter` from a manifest using the [`PublishProfile::Artifact`] profile.
    ///
    /// This preserves legacy behavior (Next.js standalone `node_modules` included).
    /// For new source-native publish flows, use [`Self::from_manifest_with_profile`] with
    /// [`PublishProfile::Source`] instead.
    pub fn from_manifest(manifest: &CapsuleManifest) -> Result<Self> {
        Self::from_manifest_with_profile(manifest, PublishProfile::Artifact)
    }

    /// Build a `PackFilter` from a manifest with an explicit publish profile.
    ///
    /// - [`PublishProfile::Source`]: excludes Next.js standalone `node_modules` and all
    ///   other dependency/build outputs by invariant.  Default for `ato publish`.
    /// - [`PublishProfile::Artifact`]: allows runtime-complete artifacts through
    ///   (legacy behavior; use for explicit artifact publish).
    pub fn from_manifest_with_profile(
        manifest: &CapsuleManifest,
        profile: PublishProfile,
    ) -> Result<Self> {
        let pack = manifest.pack.clone().unwrap_or_default();
        let include_patterns = normalized_patterns(&pack.include);
        let exclude_patterns = normalized_patterns(&pack.exclude);

        let include = if include_patterns.is_empty() {
            None
        } else {
            Some(build_glob_set(&include_patterns)?)
        };

        let mut excludes = Vec::<String>::new();
        excludes.extend(SMART_DEFAULT_EXCLUDES.iter().map(|v| v.to_string()));
        excludes.extend(exclude_patterns);

        let exclude = build_glob_set(&excludes)?;
        Ok(Self {
            include,
            exclude,
            profile,
        })
    }

    pub fn should_include_file(&self, relative_path: &Path) -> bool {
        let rel = normalize_rel_path(relative_path);
        if rel.is_empty() {
            return false;
        }

        if let Some(include) = &self.include
            && !include.is_match(&rel)
        {
            return false;
        }

        // Next.js standalone runtime bundles node_modules under `.next/standalone`.
        // Only allow this subtree for the Artifact profile — the Source profile treats
        // all dependency outputs as excluded by invariant, not denylist.
        if self.profile == PublishProfile::Artifact && is_next_standalone_node_modules(&rel) {
            return true;
        }

        // .env.example / .env.template / .env.sample are safe template files and must
        // always be included so recipients know which variables to configure.
        if is_env_template_file(&rel) {
            return true;
        }

        !self.exclude.is_match(&rel)
    }
}

pub fn load_pack_filter_from_path(manifest_path: &Path) -> Result<PackFilter> {
    PackFilter::from_manifest_path(manifest_path)
}

fn normalized_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(normalize_pattern)
        .collect()
}

fn build_glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| {
            CapsuleError::Pack(format!("Invalid pack pattern '{}': {}", pattern, e))
        })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| CapsuleError::Pack(format!("Failed to build pack pattern matcher: {}", e)))
}

fn normalize_pattern(pattern: &str) -> String {
    pattern.replace('\\', "/")
}

fn normalize_rel_path(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_next_standalone_node_modules(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    let marker = ".next/standalone/";
    if let Some(idx) = lower.find(marker) {
        let tail = &lower[idx + marker.len()..];
        tail == "node_modules"
            || tail.starts_with("node_modules/")
            || tail.contains("/node_modules/")
            || tail.ends_with("/node_modules")
    } else {
        false
    }
}

/// Returns `true` for `.env.example`, `.env.template`, `.env.sample`, `.env.dist`,
/// and similar template files that document required env keys but contain no secrets.
/// These must always be included in capsule archives even though `.env.*` is in the
/// default exclude list.
fn is_env_template_file(rel: &str) -> bool {
    let basename = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    matches!(
        basename.as_str(),
        ".env.example"
            | ".env.template"
            | ".env.sample"
            | ".env.dist"
            | ".env.default"
            | ".env.schema"
    )
}

#[cfg(test)]
mod tests {
    // std
    use std::default::Default;
    use std::path::Path;

    // external crates

    // internal crates
    use super::{PackFilter, PublishProfile};
    use crate::types::{CapsuleManifest, PackConfig};

    #[test]
    fn defaults_exclude_workspace_local_and_ephemeral_outputs() {
        let filter = PackFilter::from_manifest(&CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        })
        .expect("filter");
        assert!(!filter.should_include_file(Path::new(".ato/source-inference/provenance.json")));
        assert!(
            !filter.should_include_file(Path::new(".tmp/source-inference/attempt/ato.lock.json"))
        );
        assert!(!filter.should_include_file(Path::new(".ato.run.generated.capsule.toml")));
        assert!(!filter.should_include_file(Path::new("ato.lock.json")));
    }

    #[test]
    fn defaults_exclude_node_modules_when_include_absent() {
        let filter = PackFilter::from_manifest(&CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        })
        .expect("filter");
        assert!(!filter.should_include_file(Path::new("node_modules/a.js")));
        assert!(filter.should_include_file(Path::new("apps/a.ts")));
    }

    #[test]
    fn include_mode_allows_explicitly_selected_paths() {
        let mut manifest = CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        };
        manifest.pack = Some(PackConfig {
            include: vec!["apps/**".to_string()],
            exclude: vec!["**/*.test.ts".to_string()],
        });

        let filter = PackFilter::from_manifest(&manifest).expect("filter");
        assert!(filter.should_include_file(Path::new("apps/a.ts")));
        assert!(!filter.should_include_file(Path::new("apps/a.test.ts")));
        assert!(!filter.should_include_file(Path::new("node_modules/x.js")));
    }

    #[test]
    fn include_mode_cannot_force_include_hard_default_excludes() {
        let mut manifest = CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        };
        manifest.pack = Some(PackConfig {
            include: vec!["**/node_modules/**".to_string()],
            exclude: vec![],
        });

        let filter = PackFilter::from_manifest(&manifest).expect("filter");
        assert!(!filter.should_include_file(Path::new("apps/web/node_modules/react/index.js")));
    }

    #[test]
    fn next_standalone_node_modules_are_kept_even_with_broad_node_modules_exclude() {
        let mut manifest = CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        };
        manifest.pack = Some(PackConfig {
            include: vec!["apps/dashboard/.next/standalone/**".to_string()],
            exclude: vec!["**/node_modules/**".to_string()],
        });

        let filter = PackFilter::from_manifest(&manifest).expect("filter");
        assert!(filter.should_include_file(Path::new(
            "apps/dashboard/.next/standalone/node_modules/next/dist/bin/next"
        )));
        assert!(filter.should_include_file(Path::new(
            "apps/dashboard/.next/standalone/apps/dashboard/server.js"
        )));
        assert!(
            !filter
                .should_include_file(Path::new("apps/dashboard/node_modules/next/dist/bin/next"))
        );
    }

    #[test]
    fn nested_next_standalone_node_modules_are_kept() {
        let mut manifest = CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        };
        manifest.pack = Some(PackConfig {
            include: vec!["apps/dashboard/.next/standalone/**".to_string()],
            exclude: vec!["**/node_modules/**".to_string()],
        });

        let filter = PackFilter::from_manifest(&manifest).expect("filter");
        assert!(filter.should_include_file(Path::new(
            "apps/dashboard/.next/standalone/apps/dashboard/node_modules/next/package.json"
        )));
    }

    fn empty_filter() -> PackFilter {
        PackFilter::from_manifest(&CapsuleManifest {
            schema_version: "0.2".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        })
        .expect("filter")
    }

    #[test]
    fn defaults_exclude_dotenv_files() {
        let filter = empty_filter();
        // Root-level .env variants
        assert!(!filter.should_include_file(Path::new(".env")));
        assert!(!filter.should_include_file(Path::new(".env.local")));
        assert!(!filter.should_include_file(Path::new(".env.production")));
        assert!(!filter.should_include_file(Path::new(".env.staging")));
        assert!(!filter.should_include_file(Path::new(".env.development")));
        assert!(!filter.should_include_file(Path::new(".envrc")));
        // Nested .env variants
        assert!(!filter.should_include_file(Path::new("apps/api/.env")));
        assert!(!filter.should_include_file(Path::new("apps/web/.env.local")));
        assert!(!filter.should_include_file(Path::new("services/backend/.envrc")));
        // Regular source files are still included
        assert!(filter.should_include_file(Path::new("src/config.ts")));
        // Template / documentation files are always included
        assert!(filter.should_include_file(Path::new(".env.example")));
        assert!(filter.should_include_file(Path::new(".env.template")));
        assert!(filter.should_include_file(Path::new(".env.sample")));
        assert!(filter.should_include_file(Path::new("apps/web/.env.example")));
    }

    #[test]
    fn defaults_exclude_secret_key_files() {
        let filter = empty_filter();
        assert!(!filter.should_include_file(Path::new("server.pem")));
        assert!(!filter.should_include_file(Path::new("private.key")));
        assert!(!filter.should_include_file(Path::new("certs/ca.pem")));
        assert!(!filter.should_include_file(Path::new("keys/signing.key")));
        assert!(!filter.should_include_file(Path::new("credentials.json")));
        assert!(!filter.should_include_file(Path::new("service-account.json")));
        assert!(!filter.should_include_file(Path::new("service-account-prod.json")));
        assert!(!filter.should_include_file(Path::new("config/.npmrc")));
        assert!(!filter.should_include_file(Path::new(".netrc")));
        // Regular JSON files are still included
        assert!(filter.should_include_file(Path::new("src/data.json")));
        assert!(filter.should_include_file(Path::new("package.json")));
    }

    fn make_manifest_with_pack(include: Vec<String>, exclude: Vec<String>) -> CapsuleManifest {
        let mut m = CapsuleManifest {
            schema_version: "0.3".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: crate::types::CapsuleType::App,
            default_target: "cli".to_string(),
            metadata: Default::default(),
            capabilities: None,
            requirements: Default::default(),
            execution: Default::default(),
            storage: Default::default(),
            state: Default::default(),
            state_owner_scope: None,
            service_binding_scope: None,
            routing: Default::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            exports: None,
            services: None,
            dependencies: Default::default(),
            required_env: Vec::new(),
            contracts: Default::default(),
            workspace: None,
            distribution: None,
            platforms: Default::default(),
            tool_dependencies: Default::default(),
            foundation_requirements: None,
            host_capabilities: vec![],
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: Default::default(),
            bindings: Default::default(),
            external: Default::default(),
            context: None,
            generated_bindings: Default::default(),
        };
        m.pack = Some(PackConfig { include, exclude });
        m
    }

    /// Source profile must exclude Next.js standalone node_modules — the invariant is:
    /// source-native publish archives NEVER contain materialized dependency outputs.
    #[test]
    fn source_profile_excludes_next_standalone_node_modules() {
        let manifest = make_manifest_with_pack(
            vec!["**".to_string()],
            vec!["**/node_modules/**".to_string()],
        );
        let filter = PackFilter::from_manifest_with_profile(&manifest, PublishProfile::Source)
            .expect("filter");
        // The Next.js standalone subtree must NOT be allowed through Source profile.
        assert!(
            !filter
                .should_include_file(Path::new(".next/standalone/node_modules/next/package.json"))
        );
        assert!(!filter.should_include_file(Path::new(
            "apps/web/.next/standalone/apps/web/node_modules/react/index.js"
        )));
        // Regular source files must still be included.
        assert!(filter.should_include_file(Path::new("src/index.ts")));
        assert!(filter.should_include_file(Path::new(".next/standalone/server.js")));
    }

    /// Artifact profile must still include Next.js standalone node_modules — pre-built
    /// Next.js standalone apps bundle their own node_modules under `.next/standalone`.
    #[test]
    fn artifact_profile_includes_next_standalone_node_modules() {
        let manifest = make_manifest_with_pack(
            vec!["**".to_string()],
            vec!["**/node_modules/**".to_string()],
        );
        let filter = PackFilter::from_manifest_with_profile(&manifest, PublishProfile::Artifact)
            .expect("filter");
        assert!(
            filter
                .should_include_file(Path::new(".next/standalone/node_modules/next/package.json"))
        );
        assert!(filter.should_include_file(Path::new(
            "apps/web/.next/standalone/apps/web/node_modules/react/index.js"
        )));
    }

    /// Common dependency/build output dirs are excluded by both profiles via SMART_DEFAULT_EXCLUDES.
    #[test]
    fn both_profiles_exclude_common_dependency_dirs() {
        let base_manifest = make_manifest_with_pack(vec![], vec![]);
        for profile in [PublishProfile::Source, PublishProfile::Artifact] {
            let filter =
                PackFilter::from_manifest_with_profile(&base_manifest, profile).expect("filter");
            // Python virtual envs
            assert!(
                !filter
                    .should_include_file(Path::new(".venv/lib/site-packages/requests/__init__.py")),
                "{profile:?}: .venv should be excluded"
            );
            assert!(
                !filter.should_include_file(Path::new("venv/bin/python")),
                "{profile:?}: venv should be excluded"
            );
            // Rust build output
            assert!(
                !filter.should_include_file(Path::new("target/release/my-bin")),
                "{profile:?}: target/ should be excluded"
            );
            // Node modules at project root
            assert!(
                !filter.should_include_file(Path::new("node_modules/.bin/eslint")),
                "{profile:?}: node_modules at root should be excluded"
            );
        }
    }
}
