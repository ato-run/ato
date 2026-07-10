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

/// Build-tool probe + Dockerfile build execution (the importer's build stage).
pub mod build;
/// Imported-image → ServiceSpec mapping + rootfs export/injection/pack.
pub mod rootfs;
/// Phase 1.5: recipe-owned static seed files for ephemeral tmpfs mounts.
pub mod seed_files;
pub use rootfs::{
    EphemeralMountSeed, EphemeralMountSource, EphemeralMountSpec, VolumePolicy,
    validate_ephemeral_mount_path, validate_ephemeral_mounts,
};

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
    pub fn new(
        dockerfile_path: &str,
        build_args: BTreeMap<String, String>,
    ) -> Result<Self, String> {
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
    /// Phase 1 (ato#1024 generalized): the normalized, sorted ephemeral tmpfs
    /// mounts baked into the guest init (legacy `volumes=tmpfs` image VOLUMEs,
    /// structured `volumes`, and explicit `ephemeral_mounts` all normalize
    /// here — `source` records which). Skipped when EMPTY so every pre-existing
    /// (no-mount) import keeps a byte-identical descriptor envelope — and
    /// therefore the same `import_identity_digest` / `import_descriptor_blake3`;
    /// any mount (or a change to a mount's path/seed/size) intentionally gets a
    /// NEW identity (its init and runtime semantics differ).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ephemeral_mounts: Vec<rootfs::EphemeralMountSpec>,
    /// ato#1026: `true` when the init starts a localhost→guest-IP relay for a
    /// loopback-binding app. Skipped when false so every pre-existing import
    /// keeps a byte-identical descriptor envelope (same identity digest); an
    /// opted-in build intentionally gets a NEW identity (its init differs).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub host_bind_relay: bool,
    /// Phase 1.5: recipe-owned static seed files staged onto ephemeral tmpfs
    /// mounts (per file: destination + content blake3 digest; NEVER content).
    /// Skipped when empty so every pre-existing import keeps a byte-identical
    /// descriptor envelope + identity digest; a seeded build intentionally gets
    /// a NEW identity (its init and on-disk files differ, and a content change
    /// flips the digest). Folded into the import identity via the whole
    /// `import_options` value in `import_descriptor_canonical_json`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ephemeral_seed_mounts: Vec<seed_files::StagedSeedMount>,
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
    /// sha256 of the packed ext4 rootfs artifact the injection step produced
    /// (`sha256:<hex>`; the artifact the Ready-State build boots).
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
    Ok(EnvPartition {
        base_env,
        bindings_env,
    })
}

/// The REBUILD-INPUTS identity digest: `sha256:<hex>` over the JCS
/// canonicalization of the import's INPUTS only — importer version, platform,
/// Dockerfile path + sha256, build-context digest, digest-pinned base images
/// (sorted by original ref), build args, AND the [`DockerImportOptions`]
/// (secret policy, port override, readiness path, ext4 size — review finding
/// on ato#994: each of these changes the plan or the packed artifact, so an
/// identity that ignores them collides distinct artifacts in any cache/gate
/// keyed on it).
///
/// Outputs (final image digest, rootfs digest, warnings) are deliberately
/// excluded: this identity answers "would rebuilding produce the same
/// artifact?", so it must be computable from declared inputs alone (never a
/// job id, timestamp, or builder-host state). `effective_dockerfile_sha256`
/// is also excluded: it is DERIVED from two included inputs (dockerfile
/// sha256 + base digests), and renderer changes are covered by
/// `importer_version`.
///
/// This is NOT the Ato execution identity (ato#1002 review): it lives only in
/// the [`DockerImportReceipt`] / import descriptor lane
/// ([`import_descriptor_blake3`] shares its envelope). The execution identity
/// an import build stamps into the sealed manifest is [`import_execution_id`].
pub fn import_identity_digest(receipt: &DockerImportReceipt) -> String {
    format!(
        "sha256:{}",
        build::sha256_hex(import_descriptor_canonical_json(receipt).as_bytes())
    )
}

/// The import EXECUTION identity: `sha256:<hex>` over the JCS canonicalization
/// of the execution ENVELOPE
///
/// ```text
/// { v: "ato-import-exec/1",
///   service: { name, cmd, cwd, base_env, bindings_env, port, readiness_http_path },
///   platform, final_image_digest, secret_env_policy }
/// ```
///
/// — WHAT EXECUTES (ato#1002 review): the derived service (argv, cwd, env
/// split, public port, readiness) pinned to the exact image that runs it
/// (`final_image_digest`), all host-independent — aligned in MEANING with the
/// recipe path's `declared_execution_id` (never a job id, timestamp, or
/// builder-host state) and emitted in the same `sha256:<hex>` format
/// convention. Contrast with [`import_identity_digest`], the REBUILD-INPUTS
/// identity: rebuilding the same declared inputs keeps the rebuild identity by
/// construction, while the execution identity commits to what the sealed
/// artifact actually executes.
pub fn import_execution_id(plan: &rootfs::ImportedServicePlan, receipt: &DockerImportReceipt) -> String {
    let envelope = import_execution_envelope(
        plan,
        &receipt.platform,
        &receipt.final_image_digest,
        receipt.import_options.secret_env_policy,
    );
    let canonical = serde_jcs::to_string(&envelope).unwrap_or_else(|_| envelope.to_string());
    format!("sha256:{}", build::sha256_hex(canonical.as_bytes()))
}

/// The shared `ato-import-exec/1` execution envelope — WHAT EXECUTES: the derived
/// service pinned to the image that runs it + platform + secret policy. Both
/// import lanes (Dockerfile [`import_execution_id`] and registry-image
/// [`oci_import_execution_id`]) fold their plan through THIS one envelope, so an
/// identical derived service on an identical image digest is the same execution
/// regardless of how the image was acquired (built vs pulled).
///
/// v0 emits exactly ONE service ([`rootfs::IMPORTED_SERVICE_NAME`]); the envelope
/// is folded over the plan's first service so a future multi-service import must
/// extend it DELIBERATELY (new envelope version), never silently. The fallbacks
/// mirror the derive defaults and are unreachable for plans produced by
/// `derive_imported_service_plan`.
fn import_execution_envelope(
    plan: &rootfs::ImportedServicePlan,
    platform: &str,
    final_image_digest: &str,
    secret_env_policy: SecretEnvPolicy,
) -> serde_json::Value {
    let svc = plan.supervisor.services.as_deref().and_then(|s| s.first());
    serde_json::json!({
        "v": "ato-import-exec/1",
        "service": {
            "name": svc.map(|s| s.name.clone()).unwrap_or_else(|| rootfs::IMPORTED_SERVICE_NAME.to_string()),
            "cmd": svc.map(|s| s.cmd.clone()).unwrap_or_default(),
            "cwd": svc.map(|s| s.cwd.clone()).unwrap_or_else(|| "/".to_string()),
            "base_env": svc.map(|s| s.base_env.clone()).unwrap_or_default(),
            "bindings_env": svc.map(|s| s.env_map.clone()).unwrap_or_default(),
            "port": plan.port,
            "readiness_http_path": plan.readiness_http_path,
        },
        "platform": platform,
        "final_image_digest": final_image_digest,
        // Serde form ("reject" / "convert_placeholders") — the same stable
        // literal the receipt records.
        "secret_env_policy": secret_env_policy,
    })
}

/// The import DESCRIPTOR hash: `blake3:<hex>` over the **same** JCS input envelope
/// as [`import_identity_digest`] (ato#1002). An import job has no `capsule.toml`,
/// so the registry's `capsule_manifest_hash` column carries this instead — it is a
/// hash of the input-only import DESCRIPTOR ("what was asked to be built"), NOT of
/// a capsule manifest; there is no manifest to hash. Same input-only discipline:
/// outputs and derived fields never shift it. The envelope construction is shared
/// ([`import_descriptor_canonical_json`]) so the two digests hash identical bytes
/// by construction and cannot drift.
pub fn import_descriptor_blake3(receipt: &DockerImportReceipt) -> String {
    format!(
        "blake3:{}",
        blake3::hash(import_descriptor_canonical_json(receipt).as_bytes()).to_hex()
    )
}

/// The JCS-canonicalized input-only import descriptor both digests hash — factored
/// so [`import_identity_digest`] (sha256, the rebuild-inputs identity) and
/// [`import_descriptor_blake3`] (blake3, the registry descriptor hash) cannot drift.
fn import_descriptor_canonical_json(receipt: &DockerImportReceipt) -> String {
    let mut bases: Vec<&ResolvedBaseImage> = receipt.resolved_base_images.iter().collect();
    bases.sort_by(|a, b| a.original_ref.cmp(&b.original_ref));
    let inputs = serde_json::json!({
        "importer_version": receipt.importer_version,
        "platform": receipt.platform,
        "dockerfile_path": receipt.dockerfile_path,
        "dockerfile_sha256": receipt.dockerfile_sha256,
        "build_context_digest": receipt.build_context_digest,
        "resolved_base_images": bases.iter().map(|b| serde_json::json!({
            "original_ref": b.original_ref,
            "resolved_digest": b.resolved_digest,
        })).collect::<Vec<_>>(),
        "build_args": receipt.build_args,
        "import_options": serde_json::to_value(&receipt.import_options)
            .unwrap_or(serde_json::Value::Null),
    });
    serde_jcs::to_string(&inputs).unwrap_or_else(|_| inputs.to_string())
}

/// One end-to-end Dockerfile import request (builder-host side).
#[derive(Debug)]
pub struct DockerfileImportRequest<'a> {
    /// The build context (a materialized checkout).
    pub context_dir: &'a std::path::Path,
    pub spec: DockerImportSpec,
    pub policy: SecretEnvPolicy,
    /// Explicit public port (wins over EXPOSE).
    pub port_override: Option<u16>,
    /// Explicit readiness path (`None` = synthesized `GET /`).
    pub readiness_http_path: Option<String>,
    /// ato#1024: how to treat image-declared VOLUMEs (default fail-closed).
    pub volume_policy: rootfs::VolumePolicy,
    /// Phase 1: explicit, image-independent ephemeral tmpfs mounts (with
    /// optional copy-up seeding + per-mount size cap). Normalized + merged with
    /// the image-VOLUME expansion into the plan's single sorted mount list.
    pub ephemeral_mounts: Vec<rootfs::EphemeralMountSpec>,
    /// ato#1026: start the localhost→guest-IP relay (default off).
    pub host_bind_relay: bool,
    /// Phase 1.5: recipe-owned ephemeral seed mounts (tmpfs mount + static files
    /// read from the build context / recipe root at build time). Default empty.
    pub ephemeral_seed_mounts: Vec<seed_files::EphemeralMountSpec>,
    /// Tag for the ephemeral built image (removed after export).
    pub image_tag: String,
    pub out_ext4: &'a std::path::Path,
    pub size_mib: u64,
}

/// Everything a caller (dev CLI now; builder daemon later) needs from a
/// completed import: the provenance receipt, the launch plan, and the packed
/// rootfs artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerfileImportOutcome {
    pub receipt: DockerImportReceipt,
    pub plan: rootfs::ImportedServicePlan,
    pub rootfs_path: String,
    pub rootfs_bytes: u64,
}

/// Pure receipt assembly from the build + plan outputs (unit-tested; the
/// executor below stays thin).
fn assemble_receipt(
    probe: &build::BuildToolProbe,
    spec: &DockerImportSpec,
    built: &build::DockerfileBuildOutput,
    plan: &rootfs::ImportedServicePlan,
    import_options: DockerImportOptions,
    exported_rootfs_digest: String,
) -> DockerImportReceipt {
    DockerImportReceipt {
        importer_version: DOCKER_IMPORTER_VERSION.to_string(),
        build_tool: probe.tool,
        build_tool_version: probe.version.clone(),
        platform: spec.platform.clone(),
        dockerfile_path: spec.dockerfile_path.clone(),
        dockerfile_sha256: built.dockerfile_sha256.clone(),
        effective_dockerfile_sha256: built.effective_dockerfile_sha256.clone(),
        build_context_digest: built.build_context_digest.clone(),
        resolved_base_images: built.resolved_base_images.clone(),
        final_image_digest: built.final_image_digest.clone(),
        exported_rootfs_digest,
        build_args: spec.build_args.clone(),
        import_options,
        warnings: plan.warnings.clone(),
    }
}

/// Drive one Dockerfile import end to end on the builder host:
/// probe → build (+ digest-pin bases) → derive the service plan → pack the
/// imported image into a bootable ext4 → assemble the provenance receipt.
/// The output ext4 is a normal supervisor rootfs — it feeds the existing
/// Ready-State build (boot → verify → snapshot → seal) unchanged.
pub fn run_dockerfile_import(
    runner: &dyn build::ImportCommandRunner,
    req: &DockerfileImportRequest<'_>,
) -> Result<DockerfileImportOutcome, String> {
    let probe = build::probe_build_tool(runner)?;
    let built = build::run_dockerfile_build(runner, &probe, req.context_dir, &req.spec, &req.image_tag)?;
    let plan = rootfs::derive_imported_service_plan_with_mounts(
        &built.image_config,
        req.policy,
        req.port_override,
        req.readiness_http_path.clone(),
        req.volume_policy,
        req.ephemeral_mounts.clone(),
        req.host_bind_relay,
    )?;
    // Phase 1.5: stage recipe-owned seed files from the build context (the recipe
    // root) — validate paths + secret-scan content + digest each, fail-closed.
    // The staged records (path+digest) go into the receipt/identity; the render
    // records carry the content for the guest init.
    let mut staged_seed_mounts = Vec::with_capacity(req.ephemeral_seed_mounts.len());
    let mut rendered_seed_mounts = Vec::with_capacity(req.ephemeral_seed_mounts.len());
    for m in &req.ephemeral_seed_mounts {
        let (staged, rendered) = seed_files::stage_seed_mount(req.context_dir, m)?;
        staged_seed_mounts.push(staged);
        rendered_seed_mounts.push(rendered);
    }
    let rootfs_bytes = rootfs::pack_imported_rootfs(
        probe.tool,
        &req.image_tag,
        &plan,
        &rendered_seed_mounts,
        req.out_ext4,
        req.size_mib,
    )?;
    let exported_rootfs_digest = format!("sha256:{}", build::sha256_file_hex(req.out_ext4)?);
    let import_options = DockerImportOptions {
        secret_env_policy: req.policy,
        port_override: req.port_override,
        readiness_http_path: req.readiness_http_path.clone(),
        size_mib: req.size_mib,
        // The normalized+sorted mount list the plan derived (image-VOLUME
        // expansion + explicit mounts) — the identity/receipt record.
        ephemeral_mounts: plan.ephemeral_mounts.clone(),
        host_bind_relay: req.host_bind_relay,
        ephemeral_seed_mounts: staged_seed_mounts,
    };
    let receipt = assemble_receipt(
        &probe,
        &req.spec,
        &built,
        &plan,
        import_options,
        exported_rootfs_digest,
    );
    Ok(DockerfileImportOutcome {
        receipt,
        plan,
        rootfs_path: req.out_ext4.display().to_string(),
        rootfs_bytes,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// ato#1028 Registry Image Import v1.8 (`oci_image_import`)
//
// Pull a PUBLIC registry image and pack it into a Ready-State rootfs, REUSING
// the entire Dockerfile-import backend after the image is materialized. Only the
// ACQUIRE stage differs (pull+inspect vs build — [`build::pull_and_inspect_image`]);
// plan derivation, rootfs pack, seal → restore-verify → scan → ack are the SAME
// code path. v1 scope: public registry, single image, linux/amd64, no auth, one
// public web service, existing secret/env gate.
// ═══════════════════════════════════════════════════════════════════════════

/// Provenance of the resolved registry image. `original_ref` is provenance-ONLY:
/// the artifact identity keys on `resolved_digest`, so the SAME image pulled via
/// two DIFFERENT tags produces the SAME artifact identity (a tag is not part of
/// the identity — only the pinned digest is). Non-secret; safe in a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedRegistryImage {
    /// What the job asked for (`ghcr.io/x/y:latest` or an already-pinned
    /// `…@sha256:…`). Provenance only — never an identity input.
    pub original_ref: String,
    /// The registry manifest digest the pull resolved to for [`platform`]
    /// (`registry/repo@sha256:…`). THE identity input for the image.
    pub resolved_digest: String,
    /// Always [`DOCKER_IMPORT_PLATFORM`] in v1.
    pub platform: String,
}

/// Non-secret provenance of a completed registry-image import — the OCI-lane
/// analogue of [`DockerImportReceipt`]. There is no Dockerfile / build context /
/// base-image list (the image was pulled, not built); the resolved digest + the
/// inspected image config stand in as the reproducible inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OciImageImportReceipt {
    /// [`DOCKER_IMPORTER_VERSION`] at import time.
    pub importer_version: String,
    /// The container CLI that pulled + inspected the image (podman/docker).
    pub pull_tool: BuildTool,
    pub pull_tool_version: String,
    /// Resolved image identity + provenance.
    pub image: ResolvedRegistryImage,
    /// The inspected runtime config the plan derived from. An identity input
    /// (recorded for audit since an import has no Dockerfile) — but pinned by
    /// `image.resolved_digest`, so it does NOT let two tags of the same image
    /// diverge in identity.
    pub image_config: build::DockerImageConfig,
    /// The local image content id (`.Id`, `sha256:…`) — the exact image that runs
    /// (an OUTPUT: an execution fact, never a rebuild-inputs identity fact).
    pub final_image_digest: String,
    /// sha256 of the packed ext4 rootfs artifact (`sha256:<hex>`).
    pub exported_rootfs_digest: String,
    /// The request options that shaped the plan/rootfs — identity inputs.
    pub import_options: DockerImportOptions,
    pub warnings: Vec<DockerImportWarning>,
}

/// One end-to-end registry-image import request (builder-host side).
#[derive(Debug)]
pub struct OciImageImportRequest<'a> {
    /// The registry image reference: a tag OR a digest. Validated fail-closed
    /// ([`validate_image_ref`]); pinned to a digest before the pack regardless.
    pub image_ref: String,
    /// Secret ENV policy for the inspected image config (Store jobs fix `Reject`).
    pub policy: SecretEnvPolicy,
    pub port_override: Option<u16>,
    pub readiness_http_path: Option<String>,
    /// How to treat image-declared VOLUMEs (default fail-closed, ato#1024).
    pub volume_policy: rootfs::VolumePolicy,
    /// Start the localhost→guest-IP relay (default off, ato#1026).
    pub host_bind_relay: bool,
    pub out_ext4: &'a std::path::Path,
    pub size_mib: u64,
}

/// Everything a caller needs from a completed registry-image import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciImageImportOutcome {
    pub receipt: OciImageImportReceipt,
    pub plan: rootfs::ImportedServicePlan,
    pub rootfs_path: String,
    pub rootfs_bytes: u64,
}

/// Validate a registry image reference (tag OR digest) fail-closed. The ref goes
/// to podman/docker as a NON-shell positional arg (no injection risk there) and,
/// once resolved, into the QUOTED pack-script `TAG=`; this gate rejects shapes
/// that are ambiguous or hostile regardless: empty/whitespace-padded, over-long,
/// a leading `-` (would be read as a CLI flag), or any char outside the OCI
/// reference charset `[A-Za-z0-9._/:@-]`.
pub fn validate_image_ref(image: &str) -> Result<(), String> {
    if image.is_empty() {
        return Err("image reference is empty".into());
    }
    if image.trim() != image {
        return Err("image reference has leading/trailing whitespace".into());
    }
    if image.chars().count() > 512 {
        return Err("image reference exceeds 512 characters".into());
    }
    if image.starts_with('-') {
        return Err(format!("image reference {image:?} must not start with '-' (fail-closed)"));
    }
    if !image
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':' | '@'))
    {
        return Err(format!(
            "image reference {image:?} contains characters outside [A-Za-z0-9._/:@-] — \
             refusing to pass it to the registry client (fail-closed)"
        ));
    }
    Ok(())
}

/// True when the reference is already digest-pinned (`…@sha256:…`). Both a tag
/// and a digest ref are accepted inputs — either way the import resolves the
/// registry manifest digest before the pack, so the artifact is always pinned.
pub fn is_pinned_digest_ref(image: &str) -> bool {
    image.contains("@sha256:")
}

/// The REBUILD-INPUTS identity digest for a registry import: `sha256:<hex>` over
/// the JCS canonicalization of the INPUTS only — importer version, platform, the
/// resolved image digest, the normalized image config, and the
/// [`DockerImportOptions`]. It deliberately does NOT hash `original_ref` (so two
/// tags of the same image share an identity) nor the OUTPUTS (`final_image_digest`,
/// `exported_rootfs_digest`, warnings). The OCI-lane analogue of
/// [`import_identity_digest`].
pub fn oci_import_identity_digest(receipt: &OciImageImportReceipt) -> String {
    format!("sha256:{}", build::sha256_hex(oci_import_descriptor_canonical_json(receipt).as_bytes()))
}

/// The registry-import DESCRIPTOR hash: `blake3:<hex>` over the SAME input-only
/// JCS envelope as [`oci_import_identity_digest`] — an import has no capsule.toml,
/// so the registry's `capsule_manifest_hash` column carries this descriptor hash.
/// Shared envelope construction so the two digests cannot drift.
pub fn oci_import_descriptor_blake3(receipt: &OciImageImportReceipt) -> String {
    format!("blake3:{}", blake3::hash(oci_import_descriptor_canonical_json(receipt).as_bytes()).to_hex())
}

/// The JCS-canonicalized input-only registry-import descriptor both digests hash.
/// Keys on `resolved_digest`, NOT `original_ref` — the identity is the pinned
/// image, so the same digest reached via different tags is one artifact.
fn oci_import_descriptor_canonical_json(receipt: &OciImageImportReceipt) -> String {
    let inputs = serde_json::json!({
        "importer_version": receipt.importer_version,
        "platform": receipt.image.platform,
        "resolved_digest": receipt.image.resolved_digest,
        "image_config": serde_json::to_value(&receipt.image_config).unwrap_or(serde_json::Value::Null),
        "import_options": serde_json::to_value(&receipt.import_options).unwrap_or(serde_json::Value::Null),
    });
    serde_jcs::to_string(&inputs).unwrap_or_else(|_| inputs.to_string())
}

/// The import EXECUTION identity for a registry import — WHAT EXECUTES (derived
/// service + platform + the exact image digest that runs it), via the SAME
/// `ato-import-exec/1` envelope as the Dockerfile lane ([`import_execution_id`]).
pub fn oci_import_execution_id(
    plan: &rootfs::ImportedServicePlan,
    receipt: &OciImageImportReceipt,
) -> String {
    let envelope = import_execution_envelope(
        plan,
        &receipt.image.platform,
        &receipt.final_image_digest,
        receipt.import_options.secret_env_policy,
    );
    let canonical = serde_jcs::to_string(&envelope).unwrap_or_else(|_| envelope.to_string());
    format!("sha256:{}", build::sha256_hex(canonical.as_bytes()))
}

/// Drive one registry-image import end to end on the builder host: probe the
/// container tool → pull + digest-pin + inspect the image ([`build::pull_and_inspect_image`])
/// → derive the SAME service plan the Dockerfile lane derives → pack the pinned
/// image into a bootable supervisor ext4 → assemble the provenance receipt. The
/// output ext4 feeds the existing Ready-State build (boot → verify → snapshot →
/// seal) unchanged.
pub fn run_oci_image_import(
    runner: &dyn build::ImportCommandRunner,
    req: &OciImageImportRequest<'_>,
) -> Result<OciImageImportOutcome, String> {
    validate_image_ref(&req.image_ref)?;
    let probe = build::probe_build_tool(runner)?;
    let pulled = build::pull_and_inspect_image(runner, probe.tool, &req.image_ref)?;
    let plan = rootfs::derive_imported_service_plan_with_volumes(
        &pulled.image_config,
        req.policy,
        req.port_override,
        req.readiness_http_path.clone(),
        req.volume_policy,
        req.host_bind_relay,
    )?;
    // Pack from the PINNED digest ref (build from the digest, not the tag — the
    // pack script single-quotes it into TAG= and drives create/export/rmi).
    let rootfs_bytes =
        rootfs::pack_imported_rootfs(probe.tool, &pulled.resolved_digest, &plan, &[], req.out_ext4, req.size_mib)?;
    let exported_rootfs_digest = format!("sha256:{}", build::sha256_file_hex(req.out_ext4)?);
    let import_options = DockerImportOptions {
        secret_env_policy: req.policy,
        port_override: req.port_override,
        readiness_http_path: req.readiness_http_path.clone(),
        size_mib: req.size_mib,
        // The normalized ephemeral mount list the plan derived from
        // volume_policy (OCI v1 has no recipe-owned seed files yet).
        ephemeral_mounts: plan.ephemeral_mounts.clone(),
        host_bind_relay: req.host_bind_relay,
        ephemeral_seed_mounts: vec![],
    };
    let receipt = OciImageImportReceipt {
        importer_version: DOCKER_IMPORTER_VERSION.to_string(),
        pull_tool: probe.tool,
        pull_tool_version: probe.version.clone(),
        image: ResolvedRegistryImage {
            original_ref: req.image_ref.clone(),
            resolved_digest: pulled.resolved_digest.clone(),
            platform: DOCKER_IMPORT_PLATFORM.to_string(),
        },
        image_config: pulled.image_config.clone(),
        final_image_digest: pulled.final_image_digest.clone(),
        exported_rootfs_digest,
        import_options,
        warnings: plan.warnings.clone(),
    };
    Ok(OciImageImportOutcome {
        receipt,
        plan,
        rootfs_path: req.out_ext4.display().to_string(),
        rootfs_bytes,
    })
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
        assert_eq!(
            normalize_binding_name("OPENAI_API_KEY").unwrap(),
            "openai_api_key"
        );
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
        assert!(
            normalize_binding_name(".")
                .unwrap_err()
                .contains("invalid binding name")
        );
        assert!(
            normalize_binding_name("..")
                .unwrap_err()
                .contains("invalid binding name")
        );
        // Over-length is BindingName's own bound (128), not re-implemented here.
        let long = "a".repeat(200);
        assert!(
            normalize_binding_name(&long)
                .unwrap_err()
                .contains("invalid binding name")
        );
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
            assert_eq!(
                classify_dockerfile_env(k, v),
                EnvSecretClass::SecretLiteral,
                "{k}"
            );
        }
    }

    #[test]
    fn sensitive_key_with_placeholder_is_placeholder() {
        for v in [
            "",
            "  ",
            "${OPENAI_API_KEY}",
            "$OPENAI_API_KEY",
            "<your-key-here>",
        ] {
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
        assert_eq!(
            classify_dockerfile_env("MODE", "sk-latest"),
            EnvSecretClass::Plain
        );
    }

    #[test]
    fn plain_env_is_plain() {
        for (k, v) in [
            ("PORT", "8080"),
            ("NODE_ENV", "production"),
            ("WORKERS", "4"),
        ] {
            assert_eq!(classify_dockerfile_env(k, v), EnvSecretClass::Plain, "{k}");
        }
    }

    // --- partition_dockerfile_env ---------------------------------------------------

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn partition_plain_env_passes_through() {
        let p = partition_dockerfile_env(
            &env(&[("PORT", "8080"), ("NODE_ENV", "production")]),
            SecretEnvPolicy::Reject,
        )
        .unwrap();
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
        assert!(
            !err.contains("sk-abcdefghijklmnopqrstuvwx"),
            "value must never be echoed: {err}"
        );
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
        let err =
            partition_dockerfile_env(&env(&[("1BAD", "x")]), SecretEnvPolicy::Reject).unwrap_err();
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
        for bad in [
            "",
            "  ",
            "/abs/Dockerfile",
            "../Dockerfile",
            "a/../../Dockerfile",
        ] {
            assert!(
                DockerImportSpec::new(bad, BTreeMap::new()).is_err(),
                "{bad:?}"
            );
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

    fn sample_receipt() -> DockerImportReceipt {
        DockerImportReceipt {
            importer_version: DOCKER_IMPORTER_VERSION.into(),
            build_tool: BuildTool::Podman,
            build_tool_version: "podman 4.9".into(),
            platform: DOCKER_IMPORT_PLATFORM.into(),
            dockerfile_path: "Dockerfile".into(),
            dockerfile_sha256: "ab".repeat(32),
            build_context_digest: "cd".repeat(32),
            resolved_base_images: vec![
                ResolvedBaseImage {
                    original_ref: "node:20".into(),
                    resolved_digest: format!("docker.io/library/node@sha256:{}", "ef".repeat(32)),
                },
                ResolvedBaseImage {
                    original_ref: "alpine:3.19".into(),
                    resolved_digest: format!("docker.io/library/alpine@sha256:{}", "12".repeat(32)),
                },
            ],
            final_image_digest: format!("sha256:{}", "01".repeat(32)),
            exported_rootfs_digest: format!("sha256:{}", "23".repeat(32)),
            build_args: BTreeMap::new(),
            effective_dockerfile_sha256: "fe".repeat(32),
            import_options: DockerImportOptions {
                secret_env_policy: SecretEnvPolicy::Reject,
                port_override: None,
                readiness_http_path: None,
                size_mib: 2048,
                ephemeral_mounts: vec![],
                host_bind_relay: false,
                ephemeral_seed_mounts: vec![],
            },
            warnings: vec![],
        }
    }

    // --- import_identity_digest ---------------------------------------------------

    #[test]
    fn identity_digest_is_deterministic_and_input_only() {
        let r = sample_receipt();
        let a = import_identity_digest(&r);
        assert!(a.starts_with("sha256:") && a.len() == 7 + 64, "{a}");
        // Base image ORDER must not matter (canonicalized by original_ref).
        let mut swapped = r.clone();
        swapped.resolved_base_images.reverse();
        assert_eq!(import_identity_digest(&swapped), a);
        // OUTPUTS must not matter (identity = inputs only).
        let mut outputs_differ = r.clone();
        outputs_differ.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        outputs_differ.exported_rootfs_digest = format!("sha256:{}", "ee".repeat(32));
        outputs_differ.warnings = vec![DockerImportWarning::DockerUserIgnored];
        assert_eq!(import_identity_digest(&outputs_differ), a);
        // INPUTS must matter.
        let mut input_differs = r.clone();
        input_differs.dockerfile_sha256 = "00".repeat(32);
        assert_ne!(import_identity_digest(&input_differs), a);
        let mut base_differs = r.clone();
        base_differs.resolved_base_images[0].resolved_digest =
            format!("docker.io/library/node@sha256:{}", "aa".repeat(32));
        assert_ne!(import_identity_digest(&base_differs), a);
        // effective_dockerfile_sha256 is DERIVED (dockerfile sha + base digests)
        // — by itself it must not shift the identity.
        let mut derived_differs = r;
        derived_differs.effective_dockerfile_sha256 = "dd".repeat(32);
        assert_eq!(import_identity_digest(&derived_differs), a);
    }

    #[test]
    fn identity_digest_covers_every_import_option() {
        // Review blocker (ato#994 PR 5): same Dockerfile + different import
        // options = different artifact, so each option must shift the identity.
        let base = sample_receipt();
        let a = import_identity_digest(&base);

        let mut port = base.clone();
        port.import_options.port_override = Some(8080);
        assert_ne!(
            import_identity_digest(&port),
            a,
            "port_override must be identity input"
        );

        let mut readiness = base.clone();
        readiness.import_options.readiness_http_path = Some("/healthz".into());
        assert_ne!(
            import_identity_digest(&readiness),
            a,
            "readiness_http_path must be identity input"
        );

        let mut policy = base.clone();
        policy.import_options.secret_env_policy = SecretEnvPolicy::ConvertPlaceholders;
        assert_ne!(
            import_identity_digest(&policy),
            a,
            "secret_env_policy must be identity input"
        );

        let mut size = base.clone();
        size.import_options.size_mib = 4096;
        assert_ne!(
            import_identity_digest(&size),
            a,
            "size_mib must be identity input"
        );

        // And distinct option sets are pairwise distinct from one another.
        let ids = [
            import_identity_digest(&port),
            import_identity_digest(&readiness),
            import_identity_digest(&policy),
            import_identity_digest(&size),
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j]);
            }
        }
    }

    // --- import_execution_id --------------------------------------------------

    fn sample_image_config() -> build::DockerImageConfig {
        build::DockerImageConfig {
            entrypoint: vec!["docker-entrypoint.sh".into()],
            cmd: vec!["node".into(), "server.js".into()],
            working_dir: Some("/srv".into()),
            env: env(&[("NODE_ENV", "production")]),
            exposed_tcp_ports: vec![3000],
            user: None,
            has_healthcheck: false,
            volumes: vec![],
        }
    }

    fn sample_plan() -> rootfs::ImportedServicePlan {
        rootfs::derive_imported_service_plan(
            &sample_image_config(),
            SecretEnvPolicy::Reject,
            None,
            None,
        )
        .expect("sample plan derives")
    }

    #[test]
    fn execution_id_is_deterministic_and_execution_scoped() {
        // ato#1002 review (D3): the execution id hashes WHAT EXECUTES — the derived
        // service + platform + final image digest — in the recipe path's
        // `sha256:<hex>` format convention, stable across recomputations.
        let receipt = sample_receipt();
        let a = import_execution_id(&sample_plan(), &receipt);
        assert!(a.starts_with("sha256:") && a.len() == 7 + 64, "{a}");
        assert_eq!(
            import_execution_id(&sample_plan(), &receipt),
            a,
            "stable across runs"
        );

        // cmd changes the id (a different argv is a different execution).
        let mut cfg = sample_image_config();
        cfg.cmd = vec!["node".into(), "other.js".into()];
        let cmd_differs =
            rootfs::derive_imported_service_plan(&cfg, SecretEnvPolicy::Reject, None, None)
                .unwrap();
        assert_ne!(
            import_execution_id(&cmd_differs, &receipt),
            a,
            "cmd must shift the execution id"
        );

        // env changes the id.
        let mut cfg = sample_image_config();
        cfg.env.insert("WORKERS".into(), "4".into());
        let env_differs =
            rootfs::derive_imported_service_plan(&cfg, SecretEnvPolicy::Reject, None, None)
                .unwrap();
        assert_ne!(
            import_execution_id(&env_differs, &receipt),
            a,
            "env must shift the execution id"
        );

        // port changes the id.
        let port_differs = rootfs::derive_imported_service_plan(
            &sample_image_config(),
            SecretEnvPolicy::Reject,
            Some(8080),
            None,
        )
        .unwrap();
        assert_ne!(
            import_execution_id(&port_differs, &receipt),
            a,
            "port must shift the execution id"
        );

        // readiness path changes the id.
        let readiness_differs = rootfs::derive_imported_service_plan(
            &sample_image_config(),
            SecretEnvPolicy::Reject,
            None,
            Some("/healthz".into()),
        )
        .unwrap();
        assert_ne!(
            import_execution_id(&readiness_differs, &receipt),
            a,
            "readiness must shift the execution id"
        );

        // final_image_digest changes the id (the exact image that executes).
        let mut image_differs = receipt.clone();
        image_differs.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        assert_ne!(
            import_execution_id(&sample_plan(), &image_differs),
            a,
            "image digest must shift the execution id"
        );
    }

    #[test]
    fn execution_id_and_rebuild_identity_are_distinct_identities() {
        // ato#1002 review (D3): for the SAME outcome the two ids differ (different
        // envelopes, both sha256 — never one relabeled as the other), and they
        // respond to DIFFERENT facts: the final image digest is an execution fact
        // (not a rebuild input), the dockerfile sha256 a rebuild input (not an
        // execution fact).
        let receipt = sample_receipt();
        let plan = sample_plan();
        assert_ne!(
            import_execution_id(&plan, &receipt),
            import_identity_digest(&receipt)
        );

        let mut image_differs = receipt.clone();
        image_differs.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        assert_ne!(
            import_execution_id(&plan, &image_differs),
            import_execution_id(&plan, &receipt)
        );
        assert_eq!(
            import_identity_digest(&image_differs),
            import_identity_digest(&receipt)
        );

        let mut dockerfile_differs = receipt.clone();
        dockerfile_differs.dockerfile_sha256 = "00".repeat(32);
        assert_eq!(
            import_execution_id(&plan, &dockerfile_differs),
            import_execution_id(&plan, &receipt)
        );
        assert_ne!(
            import_identity_digest(&dockerfile_differs),
            import_identity_digest(&receipt)
        );
    }

    #[test]
    fn descriptor_blake3_shares_the_identity_envelope() {
        // ato#1002: import_descriptor_blake3 hashes the SAME input-only JCS envelope
        // as import_identity_digest — same determinism, same input/output split.
        let r = sample_receipt();
        let d = import_descriptor_blake3(&r);
        assert!(d.starts_with("blake3:") && d.len() == 7 + 64, "{d}");
        assert_eq!(
            import_descriptor_blake3(&r.clone()),
            d,
            "must be deterministic"
        );
        // Base image ORDER must not matter (canonicalized by original_ref).
        let mut swapped = r.clone();
        swapped.resolved_base_images.reverse();
        assert_eq!(import_descriptor_blake3(&swapped), d);
        // OUTPUTS + derived fields must not matter (descriptor = inputs only).
        let mut outputs_differ = r.clone();
        outputs_differ.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        outputs_differ.exported_rootfs_digest = format!("sha256:{}", "ee".repeat(32));
        outputs_differ.effective_dockerfile_sha256 = "dd".repeat(32);
        outputs_differ.warnings = vec![DockerImportWarning::DockerUserIgnored];
        assert_eq!(import_descriptor_blake3(&outputs_differ), d);
        // INPUTS must matter — including every import option (identity discipline).
        let mut input_differs = r.clone();
        input_differs.dockerfile_sha256 = "00".repeat(32);
        assert_ne!(import_descriptor_blake3(&input_differs), d);
        let mut option_differs = r.clone();
        option_differs.import_options.port_override = Some(8080);
        assert_ne!(import_descriptor_blake3(&option_differs), d);
        // A different hash family over the same bytes — never the sha256 relabeled.
        assert_ne!(
            d.trim_start_matches("blake3:"),
            import_identity_digest(&r).trim_start_matches("sha256:")
        );
    }

    #[test]
    fn host_bind_relay_is_skipped_when_false_and_shifts_identity_when_true() {
        // ato#1026: host_bind_relay=false is dropped from the serialized envelope
        // (skip_serializing_if), so a pre-existing import keeps a byte-identical
        // descriptor + identity digest; =true is a new input => new identity.
        let base = sample_receipt(); // import_options.host_bind_relay defaults false
        assert!(!base.import_options.host_bind_relay);
        let json = serde_json::to_string(&base.import_options).unwrap();
        assert!(!json.contains("host_bind_relay"), "false must be omitted: {json}");

        let mut relayed = base.clone();
        relayed.import_options.host_bind_relay = true;
        let relayed_json = serde_json::to_string(&relayed.import_options).unwrap();
        assert!(relayed_json.contains("\"host_bind_relay\":true"), "true must serialize: {relayed_json}");

        // Identity moves ONLY when the flag flips on -- never on the default.
        assert_ne!(import_descriptor_blake3(&relayed), import_descriptor_blake3(&base));
        assert_ne!(import_identity_digest(&relayed), import_identity_digest(&base));
    }

    #[test]
    fn ephemeral_mounts_skipped_when_empty_and_shift_identity_when_present() {
        // Phase 1: an empty mount list is dropped from the serialized envelope
        // (skip_serializing_if Vec::is_empty), so a pre-existing (no-mount)
        // import keeps a byte-identical descriptor + identity digest; any mount
        // is a new input => new identity.
        let base = sample_receipt(); // import_options.ephemeral_mounts defaults empty
        assert!(base.import_options.ephemeral_mounts.is_empty());
        let json = serde_json::to_string(&base.import_options).unwrap();
        assert!(!json.contains("ephemeral_mounts"), "empty must be omitted: {json}");
        let base_id = import_identity_digest(&base);

        let with_mount = |seed, size, source, path: &str| {
            let mut r = base.clone();
            r.import_options.ephemeral_mounts = vec![rootfs::EphemeralMountSpec {
                path: path.into(),
                seed,
                size_mib: size,
                source,
            }];
            r
        };
        let m = with_mount(rootfs::EphemeralMountSeed::CopyUp, Some(16), rootfs::EphemeralMountSource::Explicit, "/config");
        let m_json = serde_json::to_string(&m.import_options).unwrap();
        assert!(m_json.contains("\"ephemeral_mounts\""), "present when non-empty: {m_json}");
        assert!(m_json.contains("\"seed\":\"copy-up\""), "seed serializes kebab-case: {m_json}");
        assert!(m_json.contains("\"source\":\"explicit\""), "{m_json}");
        assert_ne!(import_identity_digest(&m), base_id, "a mount must shift identity");
        assert_ne!(import_descriptor_blake3(&m), import_descriptor_blake3(&base));

        // path / seed / size each independently shift the identity.
        let a = import_identity_digest(&m);
        let path_diff = with_mount(rootfs::EphemeralMountSeed::CopyUp, Some(16), rootfs::EphemeralMountSource::Explicit, "/other");
        assert_ne!(import_identity_digest(&path_diff), a, "path shifts identity");
        let seed_diff = with_mount(rootfs::EphemeralMountSeed::Empty, Some(16), rootfs::EphemeralMountSource::Explicit, "/config");
        assert_ne!(import_identity_digest(&seed_diff), a, "seed shifts identity");
        let size_diff = with_mount(rootfs::EphemeralMountSeed::CopyUp, Some(512), rootfs::EphemeralMountSource::Explicit, "/config");
        assert_ne!(import_identity_digest(&size_diff), a, "size shifts identity");
    }

    #[test]
    fn ephemeral_seed_mounts_shift_identity_and_are_skipped_when_empty() {
        // Phase 1.5: an empty seed set is dropped from the serialized envelope
        // (skip_serializing_if), so a pre-existing import keeps a byte-identical
        // descriptor + identity digest; a staged seed file (path+content digest)
        // is a NEW identity input, and its CONTENT digest is what moves identity.
        use super::seed_files::{SeedMode, StagedSeedFile, StagedSeedMount};
        let base = sample_receipt();
        assert!(base.import_options.ephemeral_seed_mounts.is_empty());
        let json = serde_json::to_string(&base.import_options).unwrap();
        assert!(!json.contains("ephemeral_seed_mounts"), "empty must be omitted: {json}");

        let mut seeded = base.clone();
        seeded.import_options.ephemeral_seed_mounts = vec![StagedSeedMount {
            path: "/config".into(),
            seed: SeedMode::CopyUp,
            size_mib: Some(16),
            files: vec![StagedSeedFile {
                dest: "config.yml".into(),
                digest: format!("blake3:{}", "ab".repeat(32)),
                if_missing: true,
            }],
        }];
        assert_ne!(import_descriptor_blake3(&seeded), import_descriptor_blake3(&base));
        assert_ne!(import_identity_digest(&seeded), import_identity_digest(&base));

        // The CONTENT digest is an identity input: a different file digest for the
        // same dest ⇒ a different artifact identity.
        let mut other = seeded.clone();
        other.import_options.ephemeral_seed_mounts[0].files[0].digest = format!("blake3:{}", "cd".repeat(32));
        assert_ne!(import_descriptor_blake3(&other), import_descriptor_blake3(&seeded));
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
                ephemeral_mounts: vec![],
                host_bind_relay: false,
                ephemeral_seed_mounts: vec![],
            },
            warnings: vec![DockerImportWarning::DockerUserIgnored],
        };
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
        assert_eq!(v["importer_version"], DOCKER_IMPORTER_VERSION);
        assert_eq!(v["build_tool"], "buildah");
        assert_eq!(v["resolved_base_images"][0]["original_ref"], "node:20");
        assert_eq!(v["warnings"][0], "docker_user_ignored");
        // Import options are identity inputs and must serialize stably.
        assert_eq!(
            v["import_options"]["secret_env_policy"],
            "convert_placeholders"
        );
        assert_eq!(v["import_options"]["port_override"], 8080);
        assert_eq!(v["import_options"]["readiness_http_path"], "/health");
        assert_eq!(v["import_options"]["size_mib"], 2048);
        assert_eq!(v["effective_dockerfile_sha256"], "ab".repeat(32));
    }

    // --- ato#1028 registry image import ------------------------------------------

    #[test]
    fn image_ref_validation_accepts_tags_and_digests_and_rejects_hostile_shapes() {
        for ok in [
            "metube",
            "ghcr.io/alexta69/metube:latest",
            "docker.io/library/redis:7.2-alpine",
            "ghcr.io/x/y@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "localhost:5000/team/app:v1.2.3",
        ] {
            assert!(validate_image_ref(ok).is_ok(), "{ok:?} should be valid");
        }
        for (bad, why) in [
            ("", "empty"),
            (" metube ", "whitespace"),
            ("-rm -rf", "leading dash"),
            ("metube;rm -rf", "shell metacharacter"),
            ("metube latest", "space"),
            ("app\nid", "newline"),
        ] {
            assert!(validate_image_ref(bad).is_err(), "{bad:?} should fail ({why})");
        }
        let long = format!("registry.example/{}:latest", "a".repeat(520));
        assert!(validate_image_ref(&long).unwrap_err().contains("512"));
    }

    #[test]
    fn digest_ref_detection() {
        assert!(is_pinned_digest_ref("ghcr.io/x/y@sha256:abcd"));
        assert!(!is_pinned_digest_ref("ghcr.io/x/y:latest"));
        assert!(!is_pinned_digest_ref("metube"));
    }

    fn sample_oci_receipt() -> OciImageImportReceipt {
        OciImageImportReceipt {
            importer_version: DOCKER_IMPORTER_VERSION.into(),
            pull_tool: BuildTool::Podman,
            pull_tool_version: "podman 4.9".into(),
            image: ResolvedRegistryImage {
                original_ref: "ghcr.io/alexta69/metube:latest".into(),
                resolved_digest: format!("ghcr.io/alexta69/metube@sha256:{}", "ab".repeat(32)),
                platform: DOCKER_IMPORT_PLATFORM.into(),
            },
            image_config: sample_image_config(),
            final_image_digest: format!("sha256:{}", "01".repeat(32)),
            exported_rootfs_digest: format!("sha256:{}", "23".repeat(32)),
            import_options: DockerImportOptions {
                secret_env_policy: SecretEnvPolicy::Reject,
                port_override: None,
                readiness_http_path: None,
                size_mib: 2048,
                ephemeral_mounts: vec![],
                host_bind_relay: false,
                ephemeral_seed_mounts: vec![],
            },
            warnings: vec![],
        }
    }

    #[test]
    fn oci_identity_is_deterministic_input_only_and_keys_on_digest_not_tag() {
        let r = sample_oci_receipt();
        let a = oci_import_identity_digest(&r);
        assert!(a.starts_with("sha256:") && a.len() == 7 + 64, "{a}");
        assert_eq!(oci_import_identity_digest(&r.clone()), a, "deterministic");

        // TWO TAGS, SAME DIGEST ⇒ SAME identity (original_ref is provenance-only).
        let mut other_tag = r.clone();
        other_tag.image.original_ref = "docker.io/somemirror/metube:v2025.07".into();
        assert_eq!(oci_import_identity_digest(&other_tag), a, "tag must not be an identity input");

        // OUTPUTS must not matter.
        let mut outputs_differ = r.clone();
        outputs_differ.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        outputs_differ.exported_rootfs_digest = format!("sha256:{}", "ee".repeat(32));
        outputs_differ.warnings = vec![DockerImportWarning::DockerUserIgnored];
        assert_eq!(oci_import_identity_digest(&outputs_differ), a, "outputs are not identity inputs");

        // INPUTS must matter: resolved_digest, platform, image_config, import options.
        let mut dig = r.clone();
        dig.image.resolved_digest = format!("ghcr.io/alexta69/metube@sha256:{}", "cd".repeat(32));
        assert_ne!(oci_import_identity_digest(&dig), a, "resolved_digest must be an identity input");

        let mut cfg = r.clone();
        cfg.image_config.cmd = vec!["node".into(), "other.js".into()];
        assert_ne!(oci_import_identity_digest(&cfg), a, "image config must be an identity input");

        let mut port = r.clone();
        port.import_options.port_override = Some(8080);
        assert_ne!(oci_import_identity_digest(&port), a, "port_override must be an identity input");

        let mut relay = r.clone();
        relay.import_options.host_bind_relay = true;
        assert_ne!(oci_import_identity_digest(&relay), a, "host_bind_relay must be an identity input");
    }

    #[test]
    fn oci_descriptor_blake3_shares_the_identity_envelope() {
        let r = sample_oci_receipt();
        let d = oci_import_descriptor_blake3(&r);
        assert!(d.starts_with("blake3:") && d.len() == 7 + 64, "{d}");
        assert_eq!(oci_import_descriptor_blake3(&r.clone()), d, "deterministic");
        // Same input/output split as the sha256 identity, different hash family.
        let mut outputs_differ = r.clone();
        outputs_differ.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        assert_eq!(oci_import_descriptor_blake3(&outputs_differ), d);
        let mut input_differs = r.clone();
        input_differs.image.resolved_digest = format!("ghcr.io/x/y@sha256:{}", "cd".repeat(32));
        assert_ne!(oci_import_descriptor_blake3(&input_differs), d);
        assert_ne!(
            d.trim_start_matches("blake3:"),
            oci_import_identity_digest(&r).trim_start_matches("sha256:")
        );
    }

    #[test]
    fn oci_execution_id_matches_the_shared_import_exec_envelope() {
        // The OCI lane folds its plan through the SAME ato-import-exec/1 envelope
        // as the Dockerfile lane, so identical service + platform + image digest ⇒
        // identical execution id across lanes (execution identity is lane-agnostic).
        let oci = sample_oci_receipt();
        let plan = sample_plan();
        let a = oci_import_execution_id(&plan, &oci);
        assert!(a.starts_with("sha256:") && a.len() == 7 + 64, "{a}");
        assert_eq!(oci_import_execution_id(&plan, &oci), a, "deterministic");

        // A Dockerfile receipt with the SAME platform + final_image_digest + policy
        // yields the SAME execution id for the same plan (shared envelope).
        let mut df = sample_receipt();
        df.platform = oci.image.platform.clone();
        df.final_image_digest = oci.final_image_digest.clone();
        df.import_options.secret_env_policy = oci.import_options.secret_env_policy;
        assert_eq!(import_execution_id(&plan, &df), a, "execution identity is lane-agnostic");

        // final_image_digest is an execution fact (shifts the exec id) but NOT a
        // rebuild-inputs identity fact (leaves oci_import_identity_digest alone).
        let mut image_differs = oci.clone();
        image_differs.final_image_digest = format!("sha256:{}", "ff".repeat(32));
        assert_ne!(oci_import_execution_id(&plan, &image_differs), a);
        assert_eq!(oci_import_identity_digest(&image_differs), oci_import_identity_digest(&oci));
    }

    #[test]
    fn oci_receipt_serializes_with_provenance_and_records_original_ref() {
        let r = sample_oci_receipt();
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["importer_version"], DOCKER_IMPORTER_VERSION);
        assert_eq!(v["pull_tool"], "podman");
        assert_eq!(v["image"]["original_ref"], "ghcr.io/alexta69/metube:latest");
        assert!(v["image"]["resolved_digest"].as_str().unwrap().contains("@sha256:"));
        assert_eq!(v["image"]["platform"], DOCKER_IMPORT_PLATFORM);
        // The inspected config is recorded (an import has no Dockerfile to record).
        assert!(v["image_config"]["cmd"].is_array());
    }
}
