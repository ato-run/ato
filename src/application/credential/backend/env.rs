use anyhow::Result;

use super::traits::{BackendEntry, CredentialBackend, CredentialKey};

/// Read-only backend that reads `ATO_CRED_<NAMESPACE>__<KEY>` environment variables.
///
/// `<NAMESPACE>` is the hierarchical namespace uppercased with non-alphanumeric
/// characters (`/`, `:`, `-`, `.`) replaced by `_`. For example:
///   - namespace `"secrets/default"`, key `"FOO"` → `ATO_CRED_SECRETS_DEFAULT__FOO`
///   - namespace `"secrets/capsule:myapp"`, key `"DB_PASS"` → `ATO_CRED_SECRETS_CAPSULE_MYAPP__DB_PASS`
///   - namespace `"auth/session"`, key `"SESSION_TOKEN"` → `ATO_CRED_AUTH_SESSION__SESSION_TOKEN`
///
/// Legacy `ATO_SECRET_*` env vars are **not** accepted (breaking change from
/// pre-v0.5.x behavior). `ATO_TOKEN` is retained as a compatibility alias for
/// `auth/session`/`SESSION_TOKEN` — it's the documented way to pass a session
/// token in CI/headless environments and removing it would break downstream
/// scripts.
pub(crate) struct EnvBackend;

/// Per-(namespace, name) legacy aliases. When the canonical
/// `ATO_CRED_<NS>__<KEY>` variable is unset, fall back to these.
const LEGACY_ALIASES: &[(&str, &str, &str)] = &[("auth/session", "SESSION_TOKEN", "ATO_TOKEN")];

impl EnvBackend {
    pub(crate) fn new() -> Self {
        Self
    }

    fn env_key_for(namespace: &str, name: &str) -> String {
        format!("ATO_CRED_{}__{}", namespace_to_env_segment(namespace), name)
    }

    fn env_prefix_for(namespace: &str) -> String {
        format!("ATO_CRED_{}__", namespace_to_env_segment(namespace))
    }
}

impl CredentialBackend for EnvBackend {
    fn name(&self) -> &'static str {
        "env"
    }

    fn is_writable(&self) -> bool {
        false
    }

    fn get(&self, key: &CredentialKey) -> Result<Option<String>> {
        let env_key = Self::env_key_for(&key.namespace, &key.name);
        if let Some(value) = std::env::var(env_key).ok().filter(|v| !v.is_empty()) {
            return Ok(Some(value));
        }
        // Legacy alias fallback (e.g. ATO_TOKEN → auth/session/SESSION_TOKEN).
        for (ns, name, alias) in LEGACY_ALIASES {
            if key.namespace == *ns && key.name == *name {
                if let Some(value) = std::env::var(alias).ok().filter(|v| !v.is_empty()) {
                    return Ok(Some(value));
                }
            }
        }
        Ok(None)
    }

    fn set(
        &self,
        _key: &CredentialKey,
        _value: String,
        _desc: Option<&str>,
        _allow: Option<Vec<String>>,
        _deny: Option<Vec<String>>,
    ) -> Result<()> {
        anyhow::bail!("EnvBackend is read-only");
    }

    fn delete(&self, _key: &CredentialKey) -> Result<()> {
        anyhow::bail!("EnvBackend is read-only");
    }

    fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>> {
        let prefix = Self::env_prefix_for(namespace);
        let now = chrono::Utc::now().to_rfc3339();
        let entries = std::env::vars()
            .filter_map(|(k, v)| {
                if v.is_empty() {
                    return None;
                }
                k.strip_prefix(&prefix).map(|name| BackendEntry {
                    key: name.to_string(),
                    namespace: namespace.to_string(),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                    description: Some("from env".into()),
                    allow: None,
                    deny: None,
                })
            })
            .collect();
        Ok(entries)
    }

    fn update_acl(
        &self,
        _key: &CredentialKey,
        _allow: Option<Vec<String>>,
        _deny: Option<Vec<String>>,
    ) -> Result<()> {
        anyhow::bail!("EnvBackend is read-only");
    }
}

/// Convert a hierarchical namespace into a SCREAMING_SNAKE env segment.
///
/// Rules: uppercase ASCII letters, digits, and `_` are preserved; all other
/// characters (including `/`, `:`, `-`, `.`) become `_`. Empty segments are
/// kept (multiple `_` in a row allowed).
fn namespace_to_env_segment(namespace: &str) -> String {
    namespace
        .chars()
        .map(|c| match c {
            'a'..='z' => c.to_ascii_uppercase(),
            'A'..='Z' | '0'..='9' | '_' => c,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_to_env_segment_simple() {
        assert_eq!(
            namespace_to_env_segment("secrets/default"),
            "SECRETS_DEFAULT"
        );
    }

    #[test]
    fn namespace_to_env_segment_with_colon() {
        assert_eq!(
            namespace_to_env_segment("secrets/capsule:myapp"),
            "SECRETS_CAPSULE_MYAPP"
        );
    }

    #[test]
    fn namespace_to_env_segment_auth() {
        assert_eq!(namespace_to_env_segment("auth/session"), "AUTH_SESSION");
    }

    #[test]
    fn env_key_for_constructs_expected() {
        assert_eq!(
            EnvBackend::env_key_for("secrets/default", "FOO"),
            "ATO_CRED_SECRETS_DEFAULT__FOO"
        );
    }

    #[test]
    fn get_reads_from_env_when_set() {
        let _guard = crate::application::credential::test_env_lock()
            .lock()
            .unwrap();
        // Use a unique name to avoid polluting other tests.
        let var = "ATO_CRED_SECRETS_DEFAULT__CRED_ENV_BACKEND_TEST_GET";
        std::env::set_var(var, "hello");
        let backend = EnvBackend::new();
        let got = backend
            .get(&CredentialKey::new(
                "secrets/default",
                "CRED_ENV_BACKEND_TEST_GET",
            ))
            .unwrap();
        std::env::remove_var(var);
        assert_eq!(got, Some("hello".to_string()));
    }

    #[test]
    fn get_returns_none_when_unset() {
        let backend = EnvBackend::new();
        let got = backend
            .get(&CredentialKey::new(
                "secrets/default",
                "CRED_ENV_BACKEND_TEST_UNSET_XYZ",
            ))
            .unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn legacy_ato_secret_is_ignored() {
        let _guard = crate::application::credential::test_env_lock()
            .lock()
            .unwrap();
        // Breaking change: the old ATO_SECRET_FOO form must not be read.
        std::env::set_var("ATO_SECRET_LEGACY_SHIM_TEST", "old");
        let backend = EnvBackend::new();
        let got = backend
            .get(&CredentialKey::new("secrets/default", "LEGACY_SHIM_TEST"))
            .unwrap();
        std::env::remove_var("ATO_SECRET_LEGACY_SHIM_TEST");
        assert_eq!(got, None, "legacy ATO_SECRET_* must be ignored");
    }

    #[test]
    fn set_returns_error() {
        let backend = EnvBackend::new();
        let key = CredentialKey::new("secrets/default", "X");
        assert!(backend.set(&key, "v".into(), None, None, None).is_err());
    }

    #[test]
    fn ato_token_alias_resolves_session_token() {
        let _guard = crate::application::credential::test_env_lock()
            .lock()
            .unwrap();
        // Legacy alias: ATO_TOKEN maps to auth/session/SESSION_TOKEN.
        // Canonical var must be unset so the alias actually kicks in.
        std::env::remove_var("ATO_CRED_AUTH_SESSION__SESSION_TOKEN");
        std::env::set_var("ATO_TOKEN", "legacy-session-val");
        let backend = EnvBackend::new();
        let got = backend
            .get(&CredentialKey::new("auth/session", "SESSION_TOKEN"))
            .unwrap();
        std::env::remove_var("ATO_TOKEN");
        assert_eq!(got, Some("legacy-session-val".into()));
    }

    #[test]
    fn canonical_var_beats_ato_token_alias() {
        let _guard = crate::application::credential::test_env_lock()
            .lock()
            .unwrap();
        std::env::set_var("ATO_CRED_AUTH_SESSION__SESSION_TOKEN", "canonical-val");
        std::env::set_var("ATO_TOKEN", "legacy-val");
        let backend = EnvBackend::new();
        let got = backend
            .get(&CredentialKey::new("auth/session", "SESSION_TOKEN"))
            .unwrap();
        std::env::remove_var("ATO_CRED_AUTH_SESSION__SESSION_TOKEN");
        std::env::remove_var("ATO_TOKEN");
        assert_eq!(got, Some("canonical-val".into()));
    }
}
