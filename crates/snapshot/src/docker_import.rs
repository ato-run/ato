//! v1.7 Dockerfile-to-Snapshot Import (ato#994) — schema, provenance types,
//! binding-name normalization, and the Dockerfile ENV secret-safety classifier.
//!
//! **Docker assets in. Docker runtime out.** Docker/Podman/Buildah are build/import
//! tools only (exactly like the generated-Dockerfile path in [`crate::rootfs_builder`]);
//! the restored VM runs the Ato guest-agent + supervisor directly — no Docker daemon,
//! containerd, Podman machine, docker.sock, or DinD inside a restored snapshot.
//!
//! This module is PURE (no build execution, no shelling out): the build-tool probe and
//! `docker build` driver land in the importer PR, the rootfs export + injection in the
//! next. Everything here is unit-testable and fail-closed:
//!
//! * [`normalize_binding_name`] — Docker env keys are conventionally UPPERCASE while a
//!   [`BindingName`] is lowercase `[a-z0-9_.-]` (the exact mismatch behind the guest
//!   `exit(2)` in ato#961). The env var name stays VERBATIM as the process env key;
//!   only the derived binding name is normalized, and a post-normalization collision
//!   is a hard error (`binding_name_collision`), never a silent merge.
//! * [`classify_dockerfile_env`] — `ENV OPENAI_API_KEY=sk-…` baked into a Dockerfile
//!   would end up sealed into the rootfs, violating the no-secret invariant the seal
//!   scanner gates on. Secret-looking keys with a non-empty literal value reject the
//!   import; placeholder/empty ones may convert to required Ato bindings only under an
//!   explicit [`SecretEnvPolicy`]. Shares the scanner's marker tables so the two
//!   policies cannot drift.
//! * [`DockerImportReceipt`] — non-secret provenance for the sealed artifact: which
//!   Dockerfile (path + sha256), which build context, which base images at which
//!   DIGESTS (tags must be resolved before an artifact can be Snapshot Ready — a tag
//!   reference is not a reproducible identity), which tool, and which import warnings
//!   (`docker_user_ignored`, `docker_healthcheck_ignored`, …) were emitted.

use std::collections::BTreeMap;

use protocol::binding_lease::BindingName;
use serde::Serialize;

use crate::rootfs_builder::{valid_env_var_name, validate_subdir};
use crate::scanner::{PROVIDER_KEY_PREFIXES, SENSITIVE_ENV_MARKERS};

/// Importer identity recorded in every [`DockerImportReceipt`]. Bump on any change to
/// the normalization/classification/provenance semantics in this module.
pub const DOCKER_IMPORTER_VERSION: &str = "ato-docker-import/0.1.0";

/// The single platform v0 imports (matches the KVM builder lane; arm64 desktop import
/// is explicitly out of scope until a later slice).
pub const DOCKER_IMPORT_PLATFORM: &str = "linux/amd64";

/// Which container build tool executed the import build. Import/build-time only —
/// never present at restore/runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildTool {
    Buildah,
    Podman,
    Docker,
}

impl BuildTool {
    pub fn as_str(&self) -> &'static str {
        match self {
            BuildTool::Buildah => "buildah",
            BuildTool::Podman => "podman",
            BuildTool::Docker => "docker",
        }
    }
}

/// Structured, machine-readable import warnings. Serialized snake_case so receipts
/// carry the stable literals `docker_user_ignored` / `docker_healthcheck_ignored` /
/// `exposed_port_inferred`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DockerImportWarning {
    /// v0 does not honor `USER` as a runtime uid — the imported snapshot runs under
    /// the current guest supervisor model. Follow-up: ServiceSpec uid/gid mapping.
    DockerUserIgnored,
    /// v0 does not map `HEALTHCHECK` (ReadinessSpec is port + http_path only, so a
    /// `HEALTHCHECK CMD …` cannot be honored honestly). Readiness derives from
    /// EXPOSE + Ato/default readiness config.
    DockerHealthcheckIgnored,
    /// No usable `EXPOSE` — the public port was inferred rather than declared.
    ExposedPortInferred,
}

/// One base image reference resolved at build time. `original_ref` is what the
/// Dockerfile said (`node:20`, `ghcr.io/x/y:v1`, or already a digest ref);
/// `resolved_digest` is the pinned identity the build actually used. A stage
/// reference in a multi-stage build (`FROM builder AS run`) is NOT a base image and
/// never appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedBaseImage {
    pub original_ref: String,
    pub resolved_digest: String,
}

/// The resolved, pre-build import request. Non-secret; safe in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DockerImportSpec {
    /// Repo-relative Dockerfile path (validated: relative, no `..`, no prefix).
    pub dockerfile_path: String,
    /// Always [`DOCKER_IMPORT_PLATFORM`] in v0.
    pub platform: String,
    /// Build args passed through to the build. Values are recorded in the receipt, so
    /// secret-looking args are REJECTED at spec construction (never "supported but
    /// redacted" — v0 simply has no secret build args).
    pub build_args: BTreeMap<String, String>,
}

impl DockerImportSpec {
    /// Validate + construct. Fail-closed on a path that could escape the checkout and
    /// on secret-looking build args (the receipt records args verbatim; a secret arg
    /// must not exist rather than be redacted).
    pub fn new(dockerfile_path: &str, build_args: BTreeMap<String, String>) -> Result<Self, String> {
        validate_dockerfile_path(dockerfile_path)?;
        for (k, v) in &build_args {
            if !valid_env_var_name(k) {
                return Err(format!("build arg {k:?} is not a POSIX identifier"));
            }
            if classify_dockerfile_env(k, v) != EnvSecretClass::Plain {
                return Err(format!(
                    "build arg {k:?} looks secret — secret build args are out of scope for \
                     import v0 (declare an Ato [secrets.*] binding instead)"
                ));
            }
        }
        Ok(DockerImportSpec {
            dockerfile_path: dockerfile_path.to_string(),
            platform: DOCKER_IMPORT_PLATFORM.to_string(),
            build_args,
        })
    }
}

/// The import-request options that shape the produced artifact BEYOND the
/// Dockerfile/context/base inputs. Every field here changes the plan or the
/// packed rootfs, so all of them are IDENTITY inputs (review finding on
/// ato#994 PR 5: the same Dockerfile with `--port 8080` vs `--port 3000`, a
/// different readiness path, a different secret policy, or a different ext4
/// size is a DIFFERENT artifact — an identity that ignores them poisons any
/// artifact cache / receipt gate keyed on it). Non-secret; safe in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DockerImportOptions {
    pub secret_env_policy: SecretEnvPolicy,
    /// Explicit public port (wins over EXPOSE). `None` = derived from EXPOSE.
    pub port_override: Option<u16>,
    /// Explicit readiness path. `None` = synthesized `GET /`.
    pub readiness_http_path: Option<String>,
    /// Packed ext4 size — changes the artifact bytes, therefore identity.
    pub size_mib: u64,
}

/// Non-secret provenance of a completed Dockerfile import build. Recorded alongside
/// (not replacing) the existing [`crate::rootfs_builder::RootfsReceipt`]; the artifact
/// identity for a Snapshot Ready candidate must cover every field that changes what
/// was built: dockerfile sha256, build context digest, resolved base digests, build
/// args, import options, importer version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DockerImportReceipt {
    /// [`DOCKER_IMPORTER_VERSION`] at build time.
    pub importer_version: String,
    pub build_tool: BuildTool,
    pub build_tool_version: String,
    /// [`DOCKER_IMPORT_PLATFORM`] in v0.
    pub platform: String,
    pub dockerfile_path: String,
    /// sha256 (lowercase hex) of the ORIGINAL Dockerfile bytes (as authored).
    pub dockerfile_sha256: String,
    /// sha256 (lowercase hex) of the EFFECTIVE Dockerfile the build actually
    /// consumed: the original with every registry `FROM` rewritten to its
    /// resolved digest (stage aliases preserved, prior-stage `FROM`s
    /// untouched). Populated by the build slice — this is what makes the
    /// digest pin an enforced BUILD INPUT, not an advisory record.
    pub effective_dockerfile_sha256: String,
    /// Digest over the build context actually sent to the build (deterministic walk;
    /// computed by the importer PR).
    pub build_context_digest: String,
    /// Every registry base image the build used, digest-pinned. MUST be non-empty and
    /// fully resolved before the artifact may be classified Snapshot Ready.
    pub resolved_base_images: Vec<ResolvedBaseImage>,
    /// Digest of the final built image (the multi-stage FINAL stage).
    pub final_image_digest: String,
    /// Digest of the exported rootfs tree the injection step consumed.
    pub exported_rootfs_digest: String,
    /// Build args used (already secret-screened at [`DockerImportSpec::new`]).
    pub build_args: BTreeMap<String, String>,
    /// The request options that shaped the plan/rootfs — identity inputs.
    pub import_options: DockerImportOptions,
    pub warnings: Vec<DockerImportWarning>,
}

/// Validate a repo-relative Dockerfile path: non-empty, relative, no `..`/prefix
/// components (same containment discipline as the source `subdir` gate).
pub fn validate_dockerfile_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("dockerfile path is empty".into());
    }
    validate_subdir(path)
}

/// Normalize an env var name into an Ato binding name.
///
/// The env key itself is NEVER changed — `OPENAI_API_KEY` stays `OPENAI_API_KEY` in
/// `ServiceSpec.bindings_env`; this derives only the map's VALUE (the binding name):
///
/// 1. ASCII-lowercase;
/// 2. replace every char outside `[a-z0-9_.-]` with `_`;
/// 3. collapse runs of `_`;
/// 4. trim leading/trailing `_`;
/// 5. reject empty, then revalidate with [`BindingName::parse`] (defense in depth —
///    catches `.`/`..` and over-length names with the protocol's own rules).
pub fn normalize_binding_name(env_key: &str) -> Result<String, String> {
    let mut out = String::with_capacity(env_key.len());
    let mut prev_underscore = false;
    for c in env_key.chars() {
        let c = c.to_ascii_lowercase();
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-') {
            c
        } else {
            '_'
        };
        if mapped == '_' {
            if prev_underscore {
                continue; // collapse runs of '_'
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        return Err(format!(
            "env var {env_key:?} normalizes to an empty binding name (no usable characters)"
        ));
    }
    BindingName::parse(trimmed)
        .map(|b| b.as_str().to_string())
        .map_err(|e| format!("env var {env_key:?} normalizes to an invalid binding name: {e}"))
}

/// Normalize a set of env var names into an `ENV_VAR → binding name` map,
/// **fail-closed on collision**: two distinct env keys normalizing to the same
/// binding name is a `binding_name_collision` error (a silent merge would deliver one
/// secret to two different env vars, or drop one binding entirely).
pub fn normalize_env_binding_names<'a>(
    env_keys: impl IntoIterator<Item = &'a str>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut owner: BTreeMap<String, String> = BTreeMap::new(); // binding name → env key
    // BTreeMap input order (sorted keys) keeps the FIRST-owner attribution in the
    // collision error deterministic regardless of caller iteration order.
    let mut keys: Vec<&str> = env_keys.into_iter().collect();
    keys.sort_unstable();
    keys.dedup();
    for key in keys {
        let binding = normalize_binding_name(key)?;
        if let Some(prev) = owner.insert(binding.clone(), key.to_string()) {
            return Err(format!(
                "binding_name_collision: env vars {prev:?} and {key:?} both normalize to \
                 binding name {binding:?} — rename one (fail-closed)"
            ));
        }
        out.insert(key.to_string(), binding);
    }
    Ok(out)
}

/// How a Dockerfile `ENV` entry is classified for import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvSecretClass {
    /// Not secret-looking — becomes ordinary `ServiceSpec.base_env`.
    Plain,
    /// Secret-looking key with a non-empty literal value — the import REJECTS this
    /// (baking it would seal a secret into the rootfs).
    SecretLiteral,
    /// Secret-looking key with an empty/placeholder value — convertible to a required
    /// Ato binding under [`SecretEnvPolicy::ConvertPlaceholders`].
    SecretPlaceholder,
}

/// Import policy for secret-looking `ENV` keys with placeholder/empty values.
/// Serialized snake_case: it is part of [`DockerImportOptions`] — an IDENTITY
/// input (the policy changes which env vars become bindings, i.e. the artifact).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretEnvPolicy {
    /// Default: any secret-looking ENV (literal OR placeholder) rejects the import.
    Reject,
    /// Explicitly enabled: placeholder-valued secret-looking keys become required Ato
    /// bindings (`ENV_VAR → normalized binding name`); literals still reject.
    ConvertPlaceholders,
}

/// Classify one Dockerfile `ENV` (or image-config env) entry.
///
/// Secret-looking means: the KEY contains a sensitive marker (`KEY`, `SECRET`,
/// `TOKEN`, …, matched uppercased — the scanner's own table), or the VALUE is shaped
/// like a provider credential (known prefix starting a token of plausible key
/// length). Values that are empty or a clearly-unresolved placeholder (`${VAR}`,
/// `$VAR`, `<value>`) downgrade a sensitive key to [`EnvSecretClass::SecretPlaceholder`].
pub fn classify_dockerfile_env(key: &str, value: &str) -> EnvSecretClass {
    let key_upper = key.to_ascii_uppercase();
    let sensitive_key = SENSITIVE_ENV_MARKERS.iter().any(|m| key_upper.contains(m));
    if sensitive_key {
        return if is_placeholder_value(value) {
            EnvSecretClass::SecretPlaceholder
        } else {
            EnvSecretClass::SecretLiteral
        };
    }
    if looks_like_provider_key(value) {
        // Non-sensitive NAME but a credential-shaped VALUE (e.g. `CFG=sk-…`): treat as
        // a literal secret — there is no placeholder downgrade without a sensitive key.
        return EnvSecretClass::SecretLiteral;
    }
    EnvSecretClass::Plain
}

/// Empty, or a value that is transparently NOT a credential: an unresolved
/// substitution (`$VAR` / `${VAR}`) or a docs-style `<placeholder>`. Anything else —
/// including `changeme`-style sentinels — is treated as a literal, fail-closed; the
/// author removes the ENV or declares an Ato `[secrets.*]` instead.
fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return true;
    }
    if let Some(rest) = v.strip_prefix("${") {
        return rest.ends_with('}');
    }
    if let Some(rest) = v.strip_prefix('$') {
        return !rest.is_empty() && valid_env_var_name(rest);
    }
    v.starts_with('<') && v.ends_with('>')
}

/// A value shaped like a real provider credential: a known prefix starting the token
/// and a suffix long enough to be key material (mirrors the seal scanner's
/// provider-prefix gate, minus the entropy checks — an import-time ENV is
/// human-authored text, not binary noise, so length alone is the right bar here).
fn looks_like_provider_key(value: &str) -> bool {
    let v = value.trim();
    PROVIDER_KEY_PREFIXES
        .iter()
        .any(|p| v.len() >= p.len() + 16 && v.starts_with(p))
}

/// The import-time split of a Dockerfile/image env map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvPartition {
    /// Plain entries → `ServiceSpec.base_env` (key and value verbatim).
    pub base_env: BTreeMap<String, String>,
    /// Converted placeholders → `ServiceSpec.bindings_env` (`ENV_VAR → binding name`).
    /// Always empty under [`SecretEnvPolicy::Reject`].
    pub bindings_env: BTreeMap<String, String>,
}

/// Partition a Dockerfile/image env map into base env + required bindings under the
/// given policy. Fail-closed: any [`EnvSecretClass::SecretLiteral`] is an error; under
/// [`SecretEnvPolicy::Reject`] a placeholder secret is an error too (with a hint to
/// enable conversion); env keys must be POSIX identifiers (they are emitted into
/// `supervisor.json` and the guest spawn env, same rule as the existing builder).
pub fn partition_dockerfile_env(
    env: &BTreeMap<String, String>,
    policy: SecretEnvPolicy,
) -> Result<EnvPartition, String> {
    let mut base_env = BTreeMap::new();
    let mut binding_keys: Vec<&str> = Vec::new();
    for (key, value) in env {
        if !valid_env_var_name(key) {
            return Err(format!("env var {key:?} is not a POSIX identifier"));
        }
        match classify_dockerfile_env(key, value) {
            EnvSecretClass::Plain => {
                base_env.insert(key.clone(), value.clone());
            }
            EnvSecretClass::SecretLiteral => {
                // Never echo the value (it may BE the secret).
                return Err(format!(
                    "env var {key:?} carries a secret-looking literal value — refusing to bake \
                     it into a snapshot. Remove it from the Dockerfile or declare an Ato \
                     [secrets.*] binding for it"
                ));
            }
            EnvSecretClass::SecretPlaceholder => match policy {
                SecretEnvPolicy::Reject => {
                    return Err(format!(
                        "env var {key:?} is secret-looking (placeholder value) — import policy \
                         rejects secret env conversion; enable placeholder conversion to turn \
                         it into a required Ato binding"
                    ));
                }
                SecretEnvPolicy::ConvertPlaceholders => binding_keys.push(key),
            },
        }
    }
    let bindings_env = normalize_env_binding_names(binding_keys)?;
    Ok(EnvPartition { base_env, bindings_env })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_binding_name -------------------------------------------------

    #[test]
    fn normalization_matches_the_spec_examples() {
        assert_eq!(normalize_binding_name("API_KEY").unwrap(), "api_key");
        assert_eq!(normalize_binding_name("api-key").unwrap(), "api-key");
        assert_eq!(normalize_binding_name("API KEY").unwrap(), "api_key");
        assert_eq!(normalize_binding_name("OPENAI_API_KEY").unwrap(), "openai_api_key");
        assert_eq!(normalize_binding_name("db.url").unwrap(), "db.url");
    }

    #[test]
    fn normalization_collapses_and_trims_underscores() {
        assert_eq!(normalize_binding_name("__A___B__").unwrap(), "a_b");
        assert_eq!(normalize_binding_name("A!!B").unwrap(), "a_b"); // run of replacements collapses
        assert_eq!(normalize_binding_name("_X_").unwrap(), "x");
    }

    #[test]
    fn normalization_rejects_empty_results() {
        for k in ["", "___", "!!!", "_"] {
            let err = normalize_binding_name(k).unwrap_err();
            assert!(err.contains("empty binding name"), "{k:?}: {err}");
        }
    }

    #[test]
    fn normalization_defers_reserved_and_overlong_to_binding_name_parse() {
        // "." / ".." survive the char map but are not usable path components.
        assert!(normalize_binding_name(".").unwrap_err().contains("invalid binding name"));
        assert!(normalize_binding_name("..").unwrap_err().contains("invalid binding name"));
        // Over-length is BindingName's own bound (128), not re-implemented here.
        let long = "a".repeat(200);
        assert!(normalize_binding_name(&long).unwrap_err().contains("invalid binding name"));
    }

    #[test]
    fn non_ascii_maps_to_underscore_deterministically() {
        assert_eq!(normalize_binding_name("ÅPI_KEY").unwrap(), "pi_key");
        assert_eq!(normalize_binding_name("日本語KEY").unwrap(), "key");
    }

    // --- normalize_env_binding_names ---------------------------------------------

    #[test]
    fn distinct_keys_map_and_collisions_fail_closed() {
        let ok = normalize_env_binding_names(["API_KEY", "api-key"]).unwrap();
        assert_eq!(ok["API_KEY"], "api_key");
        assert_eq!(ok["api-key"], "api-key");

        let err = normalize_env_binding_names(["API_KEY", "API!KEY"]).unwrap_err();
        assert!(err.contains("binding_name_collision"), "{err}");
        assert!(err.contains("API_KEY") && err.contains("API!KEY"), "{err}");
    }

    #[test]
    fn collision_error_is_deterministic_regardless_of_input_order() {
        let a = normalize_env_binding_names(["B!X", "B_X"]).unwrap_err();
        let b = normalize_env_binding_names(["B_X", "B!X"]).unwrap_err();
        assert_eq!(a, b);
    }

    // --- classify_dockerfile_env ---------------------------------------------------

    #[test]
    fn sensitive_key_with_literal_value_is_secret_literal() {
        for (k, v) in [
            ("OPENAI_API_KEY", "sk-abcdefghijklmnopqrstuvwx"),
            ("DATABASE_PASSWORD", "hunter2hunter2"),
            ("GITHUB_TOKEN", "ghp_0123456789abcdefghij"),
            ("MY_SECRET", "x"), // short but literal — still fail-closed
        ] {
            assert_eq!(classify_dockerfile_env(k, v), EnvSecretClass::SecretLiteral, "{k}");
        }
    }

    #[test]
    fn sensitive_key_with_placeholder_is_placeholder() {
        for v in ["", "  ", "${OPENAI_API_KEY}", "$OPENAI_API_KEY", "<your-key-here>"] {
            assert_eq!(
                classify_dockerfile_env("OPENAI_API_KEY", v),
                EnvSecretClass::SecretPlaceholder,
                "{v:?}"
            );
        }
    }

    #[test]
    fn sentinel_words_stay_fail_closed_literals() {
        // "changeme"-style sentinels are NOT recognized placeholders — deliberate.
        assert_eq!(
            classify_dockerfile_env("ADMIN_PASSWORD", "changeme"),
            EnvSecretClass::SecretLiteral
        );
    }

    #[test]
    fn provider_shaped_value_under_innocent_key_is_secret_literal() {
        assert_eq!(
            classify_dockerfile_env("CFG", "sk-abcdefghijklmnopqrstuvwxyz012345"),
            EnvSecretClass::SecretLiteral
        );
        // Short suffix after a known prefix is NOT credential-shaped (e.g. sk-latest).
        assert_eq!(classify_dockerfile_env("MODE", "sk-latest"), EnvSecretClass::Plain);
    }

    #[test]
    fn plain_env_is_plain() {
        for (k, v) in [("PORT", "8080"), ("NODE_ENV", "production"), ("WORKERS", "4")] {
            assert_eq!(classify_dockerfile_env(k, v), EnvSecretClass::Plain, "{k}");
        }
    }

    // --- partition_dockerfile_env ---------------------------------------------------

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn partition_plain_env_passes_through() {
        let p = partition_dockerfile_env(&env(&[("PORT", "8080"), ("NODE_ENV", "production")]), SecretEnvPolicy::Reject).unwrap();
        assert_eq!(p.base_env.len(), 2);
        assert!(p.bindings_env.is_empty());
    }

    #[test]
    fn partition_rejects_secret_literal_without_echoing_the_value() {
        let err = partition_dockerfile_env(
            &env(&[("OPENAI_API_KEY", "sk-abcdefghijklmnopqrstuvwx")]),
            SecretEnvPolicy::ConvertPlaceholders,
        )
        .unwrap_err();
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
        assert!(!err.contains("sk-abcdefghijklmnopqrstuvwx"), "value must never be echoed: {err}");
    }

    #[test]
    fn partition_placeholder_rejected_by_default_converted_under_policy() {
        let e = env(&[("OPENAI_API_KEY", ""), ("PORT", "8080")]);
        let err = partition_dockerfile_env(&e, SecretEnvPolicy::Reject).unwrap_err();
        assert!(err.contains("OPENAI_API_KEY"), "{err}");

        let p = partition_dockerfile_env(&e, SecretEnvPolicy::ConvertPlaceholders).unwrap();
        assert_eq!(p.base_env["PORT"], "8080");
        assert_eq!(p.bindings_env["OPENAI_API_KEY"], "openai_api_key");
    }

    #[test]
    fn partition_rejects_non_posix_env_keys() {
        let err = partition_dockerfile_env(&env(&[("1BAD", "x")]), SecretEnvPolicy::Reject).unwrap_err();
        assert!(err.contains("POSIX identifier"), "{err}");
    }

    #[test]
    fn partition_surfaces_binding_collisions() {
        // Both convertible placeholders, colliding post-normalization.
        let e = env(&[("DB_TOKEN", ""), ("DB!TOKEN", "")]);
        // "DB!TOKEN" is not a POSIX identifier, so the identifier gate fires first —
        // use two valid keys that collide instead.
        let _ = e;
        let e = env(&[("Db_Token", ""), ("DB_TOKEN", "")]);
        let err = partition_dockerfile_env(&e, SecretEnvPolicy::ConvertPlaceholders).unwrap_err();
        assert!(err.contains("binding_name_collision"), "{err}");
    }

    // --- DockerImportSpec -----------------------------------------------------------

    #[test]
    fn spec_validates_dockerfile_path_containment() {
        assert!(DockerImportSpec::new("Dockerfile", BTreeMap::new()).is_ok());
        assert!(DockerImportSpec::new("docker/prod.Dockerfile", BTreeMap::new()).is_ok());
        for bad in ["", "  ", "/abs/Dockerfile", "../Dockerfile", "a/../../Dockerfile"] {
            assert!(DockerImportSpec::new(bad, BTreeMap::new()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_rejects_secret_build_args() {
        let args = env(&[("NPM_TOKEN", "abc123abc123")]);
        let err = DockerImportSpec::new("Dockerfile", args).unwrap_err();
        assert!(err.contains("NPM_TOKEN") && err.contains("secret"), "{err}");
        let ok = DockerImportSpec::new("Dockerfile", env(&[("NODE_VERSION", "20")])).unwrap();
        assert_eq!(ok.platform, DOCKER_IMPORT_PLATFORM);
    }

    // --- receipt serialization shape --------------------------------------------------

    #[test]
    fn warnings_serialize_as_stable_snake_case_literals() {
        let json = serde_json::to_string(&vec![
            DockerImportWarning::DockerUserIgnored,
            DockerImportWarning::DockerHealthcheckIgnored,
            DockerImportWarning::ExposedPortInferred,
        ])
        .unwrap();
        assert_eq!(
            json,
            r#"["docker_user_ignored","docker_healthcheck_ignored","exposed_port_inferred"]"#
        );
    }

    #[test]
    fn receipt_serializes_with_full_provenance() {
        let receipt = DockerImportReceipt {
            importer_version: DOCKER_IMPORTER_VERSION.into(),
            build_tool: BuildTool::Buildah,
            build_tool_version: "1.35.0".into(),
            platform: DOCKER_IMPORT_PLATFORM.into(),
            dockerfile_path: "Dockerfile".into(),
            dockerfile_sha256: "ab".repeat(32),
            effective_dockerfile_sha256: "ab".repeat(32),
            build_context_digest: "cd".repeat(32),
            resolved_base_images: vec![ResolvedBaseImage {
                original_ref: "node:20".into(),
                resolved_digest: "sha256:".to_string() + &"ef".repeat(32),
            }],
            final_image_digest: "sha256:".to_string() + &"01".repeat(32),
            exported_rootfs_digest: "sha256:".to_string() + &"23".repeat(32),
            build_args: BTreeMap::new(),
            import_options: DockerImportOptions {
                secret_env_policy: SecretEnvPolicy::ConvertPlaceholders,
                port_override: Some(8080),
                readiness_http_path: Some("/health".into()),
                size_mib: 2048,
            },
            warnings: vec![DockerImportWarning::DockerUserIgnored],
        };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
        assert_eq!(v["importer_version"], DOCKER_IMPORTER_VERSION);
        assert_eq!(v["build_tool"], "buildah");
        assert_eq!(v["resolved_base_images"][0]["original_ref"], "node:20");
        assert_eq!(v["warnings"][0], "docker_user_ignored");
        // Import options are identity inputs and must serialize stably.
        assert_eq!(v["import_options"]["secret_env_policy"], "convert_placeholders");
        assert_eq!(v["import_options"]["port_override"], 8080);
        assert_eq!(v["import_options"]["readiness_http_path"], "/health");
        assert_eq!(v["import_options"]["size_mib"], 2048);
        assert_eq!(v["effective_dockerfile_sha256"], "ab".repeat(32));
    }
}
