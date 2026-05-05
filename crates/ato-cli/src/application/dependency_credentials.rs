//! Dependency credential resolution and materialization.
//!
//! Implements RFC `CAPSULE_DEPENDENCY_CONTRACTS.md` §7.3.2 Rules M1–M5.
//! v1 invariant: `{{credentials.X}}` is **never** literal-substituted into
//! argv / shell command body / env capture. The resolver here is the single
//! entry point that converts a credential template (preserved verbatim in
//! the lockfile) into a resolved value held in memory only as long as
//! materialization needs it; the value is then handed to the provider
//! process via one of three channels (TempFile / EnvVar / Stdin) and
//! zeroized on drop (best-effort, M4-b).
//!
//! # Boundaries
//! - This module **does not** read the lockfile or the manifest. It accepts
//!   already-parsed `TemplatedString` values plus the manifest top-level
//!   `required_env` scope set (RFC §5.2), and a host-env reader trait so
//!   tests can stub the host environment.
//! - This module **does not** orchestrate provider start. It produces a
//!   `MaterializedRef` that the orchestrator (P5) splices into provider
//!   command lines / env / stdin via the appropriate channel.

use std::collections::BTreeSet;
use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use capsule_core::types::{TemplateExpr, TemplateSegment, TemplatedString};
use thiserror::Error;
use zeroize::Zeroize;

// --------------------------------------------------------------- errors -----

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential template references {{{{env.{key}}}}} but '{key}' is not in manifest top-level required_env")]
    EnvKeyOutOfScope { key: String },

    #[error("credential template references {{{{env.{key}}}}} but the host environment does not set '{key}'")]
    EnvKeyMissing { key: String },

    #[error(
        "credential template includes {{{{{expr}}}}} which is not allowed in a credential value (only literal text and {{{{env.X}}}} are accepted)"
    )]
    UnsupportedTemplateExpr { expr: String },

    #[error("credential materialization failed: {detail}")]
    MaterializationFailure { detail: String },
}

// ---------------------------------------------------------- resolved value --

/// A resolved credential value held only in memory. Implements `Zeroize` so
/// the underlying buffer is wiped on drop (M4-b best-effort guarantee).
pub struct ResolvedSecret {
    inner: zeroize::Zeroizing<String>,
}

impl ResolvedSecret {
    pub fn new(value: String) -> Self {
        Self {
            inner: zeroize::Zeroizing::new(value),
        }
    }

    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

impl std::fmt::Debug for ResolvedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedSecret")
            .field("len", &self.inner.len())
            .field("value", &"***")
            .finish()
    }
}

// -------------------------------------------------------------- host env ---

/// Host environment reader. Tests substitute a `MapHostEnv` so the resolver
/// stays pure with respect to the real process env.
pub trait HostEnv {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessHostEnv;

impl HostEnv for ProcessHostEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
pub struct MapHostEnv {
    map: std::collections::HashMap<String, String>,
}

#[cfg(test)]
impl MapHostEnv {
    pub fn new(entries: &[(&str, &str)]) -> Self {
        Self {
            map: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

#[cfg(test)]
impl HostEnv for MapHostEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
}

// -------------------------------------------------- credential resolver -----

/// Resolve a credential template against the manifest top-level
/// `required_env` scope. The template must consist only of literal segments
/// and `{{env.X}}` expressions — `{{params.X}}`, `{{credentials.X}}`,
/// `{{host}}`, etc. are rejected (the lockfile-time verifier already
/// enforces this for consumer credentials, but we double-check here as
/// defense-in-depth).
pub fn resolve_credential_template(
    template: &TemplatedString,
    env_scope: &BTreeSet<&str>,
    host_env: &dyn HostEnv,
) -> Result<ResolvedSecret, CredentialError> {
    let mut buffer = String::new();
    for segment in &template.segments {
        match segment {
            TemplateSegment::Literal(text) => buffer.push_str(text),
            TemplateSegment::Expr(expr) => match expr {
                TemplateExpr::Env(key) => {
                    if !env_scope.contains(key.as_str()) {
                        return Err(CredentialError::EnvKeyOutOfScope { key: key.clone() });
                    }
                    let Some(value) = host_env.get(key) else {
                        return Err(CredentialError::EnvKeyMissing { key: key.clone() });
                    };
                    buffer.push_str(&value);
                }
                other => {
                    return Err(CredentialError::UnsupportedTemplateExpr {
                        expr: format!("{}", other),
                    });
                }
            },
        }
    }
    Ok(ResolvedSecret::new(buffer))
}

// ------------------------------------------------ materialization channel --

/// Materialization channel choice. v1 default is `TempFile`; `EnvVar` is for
/// providers that explicitly opt-in via env reading; `Stdin` is for
/// providers whose `run` command can read the secret from stdin.
// EnvVar / Stdin variants are part of the v1 API surface but only TempFile
// is wired today (per Rule M1). The dormant variants are kept so providers
// can opt in without churn — clippy's dead-code lint is suppressed for
// that reason.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum MaterializationChannel {
    /// Write to `<state_dir>/.ato-cred-<key>` with mode 0600. The
    /// returned `MaterializedRef::file_path` should be substituted in
    /// place of `{{credentials.<key>}}` in the provider's command line.
    TempFile {
        state_dir: PathBuf,
        cred_key: String,
    },
    /// Inject the value into the provider process env under the given key.
    /// Origin = `EnvOrigin::DepCredential` so v2 receipt env capture
    /// excludes it. The returned `MaterializedRef::env_key` is the key
    /// that the orchestrator should set in the child env.
    EnvVar { provider_env_key: String },
    /// Hand the value to the provider via stdin pipe. The returned
    /// `MaterializedRef::stdin_payload` is the full payload to write.
    Stdin,
}

/// Output of materialization. Each variant exposes only the *reference*
/// (path / env key / stdin payload) needed by the orchestrator. The
/// underlying `ResolvedSecret` lives inside this struct and is zeroized on
/// drop alongside any temp file unlink.
// `secret` is kept by-value so its `Drop` zeroizes the buffer when the
// `MaterializedRef` itself is dropped — even when the orchestrator only
// reads `file_path()`. Same shape for the dormant `EnvVar { key }` arm.
#[allow(dead_code)]
pub struct MaterializedRef {
    secret: ResolvedSecret,
    kind: MaterializedKind,
}

#[allow(dead_code)]
enum MaterializedKind {
    TempFile {
        path: PathBuf,
        // Owns the file handle so we can ensure unlink-on-drop semantics.
        // None after `take_path()` is called by the orchestrator if we ever
        // need the file to outlive this struct (not used in v1).
        _guard: Option<TempFileGuard>,
    },
    EnvVar {
        key: String,
    },
    Stdin,
}

struct TempFileGuard {
    path: PathBuf,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        // Best-effort unlink. If it fails (already gone, perm denied, etc.)
        // we cannot recover here; the orchestrator should not be relying on
        // this to enforce security — the caller's responsibility is to
        // confine access via state_dir permissions.
        let _ = std::fs::remove_file(&self.path);
    }
}

// `env_key` and `reveal` are forward-compat with the EnvVar / Stdin
// channels (see `MaterializationChannel` above); they are not called yet
// while only TempFile is wired.
#[allow(dead_code)]
impl MaterializedRef {
    /// Channel-specific reference for the orchestrator.
    pub fn file_path(&self) -> Option<&Path> {
        match &self.kind {
            MaterializedKind::TempFile { path, .. } => Some(path.as_path()),
            _ => None,
        }
    }

    pub fn env_key(&self) -> Option<&str> {
        match &self.kind {
            MaterializedKind::EnvVar { key } => Some(key.as_str()),
            _ => None,
        }
    }

    /// Returns the secret value for stdin / env-var injection. The caller
    /// must not log or persist this value (the redaction registry hooks
    /// any printed copy).
    pub fn reveal(&self) -> &str {
        self.secret.as_str()
    }
}

/// Materialize a resolved secret into the provider process via the chosen
/// channel.
pub fn materialize_credential(
    secret: ResolvedSecret,
    channel: MaterializationChannel,
) -> Result<MaterializedRef, CredentialError> {
    match channel {
        MaterializationChannel::TempFile {
            state_dir,
            cred_key,
        } => {
            let safe_key = sanitize_key(&cred_key);
            let path = state_dir.join(format!(".ato-cred-{}", safe_key));
            // Create with mode 0600 atomically: open with create_new + then
            // set perms; if it already exists (e.g. orphan from a previous
            // run), unlink first.
            let _ = std::fs::remove_file(&path);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|err| CredentialError::MaterializationFailure {
                    detail: format!("create temp credential file {}: {}", path.display(), err),
                })?;
            std::fs::set_permissions(&path, Permissions::from_mode(0o600)).map_err(|err| {
                CredentialError::MaterializationFailure {
                    detail: format!("chmod 0600 on {}: {}", path.display(), err),
                }
            })?;
            file.write_all(secret.as_str().as_bytes()).map_err(|err| {
                CredentialError::MaterializationFailure {
                    detail: format!("write to {}: {}", path.display(), err),
                }
            })?;
            // Drop file handle deliberately after write so the data is
            // flushed to disk; the value is also held in `secret` until
            // MaterializedRef is dropped.
            drop(file);
            Ok(MaterializedRef {
                secret,
                kind: MaterializedKind::TempFile {
                    path: path.clone(),
                    _guard: Some(TempFileGuard { path }),
                },
            })
        }
        MaterializationChannel::EnvVar { provider_env_key } => Ok(MaterializedRef {
            secret,
            kind: MaterializedKind::EnvVar {
                key: provider_env_key,
            },
        }),
        MaterializationChannel::Stdin => Ok(MaterializedRef {
            secret,
            kind: MaterializedKind::Stdin,
        }),
    }
}

fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// --------------------------------------------------- redaction registry ----

/// Registry of resolved credential strings. Once registered, any call to
/// [`RedactionRegistry::redact`] replaces those substrings with `***` so
/// log writers, receipt builders, and explain-hash output cannot leak
/// credentials even when called from code that doesn't know it's holding a
/// secret. M3 invariant.
pub struct RedactionRegistry {
    entries: RwLock<Vec<String>>,
}

impl RedactionRegistry {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    /// Register a value to be scrubbed in subsequent `redact` calls.
    /// Empty strings and 1-char strings are ignored to avoid pathological
    /// global mangling — production credentials are far longer.
    pub fn register(&self, value: &str) {
        if value.len() < 2 {
            return;
        }
        let mut entries = self.entries.write().expect("redaction lock poisoned");
        if !entries.iter().any(|existing| existing == value) {
            entries.push(value.to_string());
        }
    }

    /// Scrub registered values from `text`. Replacement is naive substring
    /// matching — sufficient for the v1 invariant (no credential plaintext
    /// in log streams) but does not protect against splitting attacks.
    pub fn redact(&self, text: &str) -> String {
        let entries = self.entries.read().expect("redaction lock poisoned");
        let mut out = text.to_string();
        for entry in entries.iter() {
            if !entry.is_empty() {
                out = out.replace(entry.as_str(), "***");
            }
        }
        out
    }
}

impl Default for RedactionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-global redaction registry. Log writers and receipt builders
/// pass output through `global_redaction().redact(text)` before write.
#[allow(dead_code)]
pub fn global_redaction() -> &'static RedactionRegistry {
    static GLOBAL: Mutex<Option<&'static RedactionRegistry>> = Mutex::new(None);
    let mut guard = GLOBAL.lock().expect("redaction global poisoned");
    if guard.is_none() {
        let registry = Box::leak(Box::new(RedactionRegistry::new()));
        *guard = Some(registry);
    }
    guard.expect("registry initialized above")
}

// ----------------------------------------------------------- zeroize impl --

impl Drop for ResolvedSecret {
    fn drop(&mut self) {
        // `Zeroizing` already wipes on drop; the explicit zeroize ensures
        // any heap-residue from string capacity past length is also cleared
        // (Zeroizing wipes the whole capacity).
        self.inner.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn parse(raw: &str) -> TemplatedString {
        TemplatedString::parse(raw).expect("parse template")
    }

    #[test]
    fn resolves_env_template_against_scope_and_host() {
        let tmpl = parse("{{env.PG_PASSWORD}}");
        let scope: BTreeSet<&str> = BTreeSet::from(["PG_PASSWORD"]);
        let host = MapHostEnv::new(&[("PG_PASSWORD", "s3cret")]);
        let resolved = resolve_credential_template(&tmpl, &scope, &host).expect("resolve");
        assert_eq!(resolved.as_str(), "s3cret");
    }

    #[test]
    fn resolves_mixed_literal_and_env() {
        let tmpl = parse("prefix-{{env.TOK}}-suffix");
        let scope: BTreeSet<&str> = BTreeSet::from(["TOK"]);
        let host = MapHostEnv::new(&[("TOK", "abc")]);
        let resolved = resolve_credential_template(&tmpl, &scope, &host).expect("resolve");
        assert_eq!(resolved.as_str(), "prefix-abc-suffix");
    }

    #[test]
    fn rejects_env_key_out_of_scope() {
        let tmpl = parse("{{env.NOPE}}");
        let scope: BTreeSet<&str> = BTreeSet::new();
        let host = MapHostEnv::new(&[("NOPE", "x")]);
        let err = resolve_credential_template(&tmpl, &scope, &host)
            .expect_err("must reject out of scope");
        assert!(
            matches!(err, CredentialError::EnvKeyOutOfScope { ref key } if key == "NOPE"),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_missing_host_env() {
        let tmpl = parse("{{env.GONE}}");
        let scope: BTreeSet<&str> = BTreeSet::from(["GONE"]);
        let host = MapHostEnv::new(&[]);
        let err =
            resolve_credential_template(&tmpl, &scope, &host).expect_err("must reject missing");
        assert!(
            matches!(err, CredentialError::EnvKeyMissing { ref key } if key == "GONE"),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_non_env_template_expressions() {
        let tmpl = parse("{{params.database}}");
        let scope: BTreeSet<&str> = BTreeSet::new();
        let host = MapHostEnv::new(&[]);
        let err = resolve_credential_template(&tmpl, &scope, &host)
            .expect_err("must reject non-env expr");
        assert!(
            matches!(err, CredentialError::UnsupportedTemplateExpr { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn temp_file_channel_writes_with_mode_0600_and_unlinks_on_drop() {
        let dir = tempfile::tempdir().expect("temp dir");
        let secret = ResolvedSecret::new("abc".to_string());
        let path_for_check;
        {
            let mref = materialize_credential(
                secret,
                MaterializationChannel::TempFile {
                    state_dir: dir.path().to_path_buf(),
                    cred_key: "password".to_string(),
                },
            )
            .expect("materialize");
            let path = mref.file_path().expect("file path");
            path_for_check = path.to_path_buf();
            let perms = std::fs::metadata(path).expect("metadata").permissions();
            assert_eq!(perms.mode() & 0o777, 0o600, "mode must be 0600");
            assert_eq!(
                std::fs::read_to_string(path).expect("read"),
                "abc",
                "file content must be the resolved secret"
            );
        }
        // After drop, the file must be gone.
        assert!(
            !path_for_check.exists(),
            "temp credential file must be unlinked on drop"
        );
    }

    #[test]
    fn env_var_channel_returns_key_only() {
        let secret = ResolvedSecret::new("abc".to_string());
        let mref = materialize_credential(
            secret,
            MaterializationChannel::EnvVar {
                provider_env_key: "PGPASSWORD".to_string(),
            },
        )
        .expect("materialize");
        assert_eq!(mref.env_key(), Some("PGPASSWORD"));
        assert!(mref.file_path().is_none());
        assert_eq!(mref.reveal(), "abc");
    }

    #[test]
    fn stdin_channel_returns_value_only() {
        let secret = ResolvedSecret::new("abc".to_string());
        let mref =
            materialize_credential(secret, MaterializationChannel::Stdin).expect("materialize");
        assert!(mref.file_path().is_none());
        assert!(mref.env_key().is_none());
        assert_eq!(mref.reveal(), "abc");
    }

    #[test]
    fn redaction_registry_replaces_registered_values() {
        let registry = RedactionRegistry::new();
        registry.register("supersecret");
        registry.register("apitoken-12345");
        let scrubbed = registry
            .redact("ERROR connecting with password=supersecret token=apitoken-12345 to db");
        assert!(!scrubbed.contains("supersecret"), "got: {scrubbed}");
        assert!(!scrubbed.contains("apitoken-12345"), "got: {scrubbed}");
        assert!(scrubbed.contains("password=***"), "got: {scrubbed}");
        assert!(scrubbed.contains("token=***"), "got: {scrubbed}");
    }

    #[test]
    fn redaction_ignores_too_short_entries() {
        // Registering a 1-char string should not cause global mangling.
        let registry = RedactionRegistry::new();
        registry.register("a");
        let out = registry.redact("a quick brown fox");
        assert_eq!(out, "a quick brown fox");
    }

    #[test]
    fn temp_file_channel_sanitizes_unsafe_credential_keys() {
        let dir = tempfile::tempdir().expect("temp dir");
        let secret = ResolvedSecret::new("abc".to_string());
        let mref = materialize_credential(
            secret,
            MaterializationChannel::TempFile {
                state_dir: dir.path().to_path_buf(),
                // Path traversal / shell-meta chars must be sanitized.
                cred_key: "../etc/passwd".to_string(),
            },
        )
        .expect("materialize");
        let path = mref.file_path().expect("path");
        let file_name = path.file_name().expect("file_name").to_str().unwrap();
        assert!(file_name.starts_with(".ato-cred-"));
        assert!(!file_name.contains("..") && !file_name.contains('/'));
        // And the file lives inside state_dir, not above it.
        assert_eq!(path.parent().unwrap(), dir.path());
    }

    #[test]
    fn resolved_secret_debug_does_not_leak_value() {
        let secret = ResolvedSecret::new("supersecret".to_string());
        let debug = format!("{:?}", secret);
        assert!(
            !debug.contains("supersecret"),
            "Debug must not leak: {debug}"
        );
        assert!(
            debug.contains("***") || debug.contains("len"),
            "got: {debug}"
        );
    }
}
