//! L3 (#912): the **SecretResolver** boundary. Decouples the preview's
//! `ATO_BINDING_<name>` env read from the bind flow so a Vault / cloud-secret /
//! user-store / config resolver can slot in later without touching the run gate.
//!
//! Contract for every resolver: it maps a declared binding **name** → its **value**,
//! **fails closed** on a missing/unresolvable binding, and **never** logs, records, or
//! includes the value in an error (errors carry the name + reason only). The receipt
//! records only [`SecretResolver::kind`] + binding names + statuses — never a value.

use anyhow::Result;
use protocol::binding_lease::SecretValue;

/// Resolves a declared binding name to its secret value for vsock delivery.
pub(crate) trait SecretResolver {
    /// Resolve `binding_name` to its value; a missing/unresolvable binding is `Err`
    /// (fail-closed). Implementations must not put the value in the error.
    fn resolve(&self, binding_name: &str) -> Result<SecretValue>;
    /// Stable resolver id recorded in the binding receipt (never a value).
    fn kind(&self) -> &'static str;
}

/// The **preview** resolver: reads `ATO_BINDING_<name>` env vars. Preview-only; a real
/// deployment swaps in a proper secret backend (below).
pub(crate) struct EnvSecretResolver;

impl SecretResolver for EnvSecretResolver {
    fn resolve(&self, binding_name: &str) -> Result<SecretValue> {
        let env = format!("ATO_BINDING_{binding_name}");
        let value = std::env::var(&env)
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "binding '{binding_name}' has no value; set {env} (preview secret source)"
                )
            })?;
        Ok(SecretValue::new(value))
    }

    fn kind(&self) -> &'static str {
        "env"
    }
}

// Future resolvers — the boundary these will fill. They are intentionally not wired
// into the run gate yet (the preview only uses `EnvSecretResolver`); each returns a
// fail-closed "not implemented" until its real backend lands, so an accidental use can
// never silently expose an unbound session.
macro_rules! unimplemented_resolver {
    ($name:ident, $kind:literal, $doc:literal) => {
        #[doc = $doc]
        #[allow(dead_code)]
        pub(crate) struct $name;
        impl SecretResolver for $name {
            fn resolve(&self, binding_name: &str) -> Result<SecretValue> {
                anyhow::bail!(
                    "{} resolver for '{binding_name}' is not implemented yet (fail-closed)",
                    $kind
                )
            }
            fn kind(&self) -> &'static str {
                $kind
            }
        }
    };
}

unimplemented_resolver!(
    VaultSecretResolver,
    "vault",
    "Vault-backed resolver (future)."
);
unimplemented_resolver!(
    CloudSecretResolver,
    "cloud",
    "Cloud secret-manager resolver (future)."
);

/// v1.2 PR 2: the **SecretStore-backed** resolver — the default secret source
/// for Ready-State bindings. Reads the value from the HOST-LOCAL age-encrypted
/// SecretStore under the capsule-scoped namespace (`rs-<hash16>`, see
/// [`super::binding_grants::binding_namespace`]). By construction the value
/// never transits any control API: local `ato run` reads the user's store;
/// a managed runner reads the RUNNER host's store (same type, same boundary —
/// this is the L9 runner-local resolver). The chain's env backend also honors
/// `ATO_CRED_SECRETS_RS_<HASH16>__<NAME>` for headless/CI grants.
pub(crate) struct UserSecretStoreResolver {
    /// The capsule-scoped grant namespace (`rs-<hash16>`).
    namespace: String,
}

impl UserSecretStoreResolver {
    pub(crate) fn new(namespace: String) -> Self {
        Self { namespace }
    }
}

impl SecretResolver for UserSecretStoreResolver {
    fn resolve(&self, binding_name: &str) -> Result<SecretValue> {
        let store = crate::application::secrets::store::SecretStore::open()
            .map_err(|e| anyhow::anyhow!("secret store unavailable: {e}"))?;
        let value = store
            .get_in_namespace(binding_name, &self.namespace)
            .map_err(|e| anyhow::anyhow!("secret store read failed for '{binding_name}': {e}"))?
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "binding '{binding_name}' has no grant in namespace '{}'",
                    self.namespace
                )
            })?;
        Ok(SecretValue::new(value))
    }

    fn kind(&self) -> &'static str {
        "user_store"
    }
}

/// Select the Ready-State secret resolver. Default = the SecretStore-backed
/// [`UserSecretStoreResolver`] (host-local, grant-scoped). Setting
/// `ATO_READY_STATE_SECRET_SOURCE=env` opts into the preview
/// [`EnvSecretResolver`] (`ATO_BINDING_<name>` — tests/smokes only). Any other
/// value **fails closed** — a typo must never silently change where secrets
/// come from.
pub(crate) fn select_resolver(namespace: &str) -> Result<Box<dyn SecretResolver>> {
    match std::env::var("ATO_READY_STATE_SECRET_SOURCE")
        .ok()
        .as_deref()
    {
        None | Some("") | Some("user_store") => Ok(Box::new(UserSecretStoreResolver::new(
            namespace.to_string(),
        ))),
        Some("env") => Ok(Box::new(EnvSecretResolver)),
        Some(other) => anyhow::bail!(
            "unknown ATO_READY_STATE_SECRET_SOURCE '{other}' (expected 'user_store' or 'env') — \
             failing closed"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_resolver_reads_value_and_fails_closed_on_missing() {
        let set = |k: &str, v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        };
        let r = EnvSecretResolver;
        assert_eq!(r.kind(), "env");
        set("ATO_BINDING_api_key", Some("sk-l3-secret"));
        assert_eq!(r.resolve("api_key").unwrap().expose(), "sk-l3-secret");
        set("ATO_BINDING_api_key", None);
        let err = r.resolve("api_key").unwrap_err().to_string();
        assert!(
            err.contains("api_key") && err.contains("ATO_BINDING_api_key"),
            "{err}"
        );
        assert!(
            !err.contains("sk-l3-secret"),
            "error must not carry a value"
        );
    }

    #[test]
    fn future_resolvers_fail_closed() {
        assert!(VaultSecretResolver.resolve("x").is_err());
        assert!(CloudSecretResolver.resolve("x").is_err());
        assert_eq!(VaultSecretResolver.kind(), "vault");
    }

    #[test]
    fn user_store_resolver_reads_grant_via_env_chain_and_fails_closed_without() {
        // The credential chain's env backend maps namespace `secrets/rs-abc123`
        // to ATO_CRED_SECRETS_RS_ABC123__<KEY> — the headless/CI grant form.
        // This exercises the REAL store path without an age identity.
        let set = |k: &str, v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        };
        let r = UserSecretStoreResolver::new("rs-abc123".to_string());
        assert_eq!(r.kind(), "user_store");
        set("ATO_CRED_SECRETS_RS_ABC123__API_KEY", Some("sk-grant"));
        assert_eq!(r.resolve("API_KEY").unwrap().expose(), "sk-grant");
        set("ATO_CRED_SECRETS_RS_ABC123__API_KEY", None);
        let err = r.resolve("API_KEY").unwrap_err().to_string();
        assert!(
            err.contains("API_KEY") && err.contains("rs-abc123"),
            "{err}"
        );
        assert!(!err.contains("sk-grant"), "error must not carry a value");
        // A DIFFERENT capsule's namespace must not see this grant (per-app scope).
        set("ATO_CRED_SECRETS_RS_ABC123__API_KEY", Some("sk-grant"));
        let other = UserSecretStoreResolver::new("rs-ffff00".to_string());
        assert!(
            other.resolve("API_KEY").is_err(),
            "cross-namespace grant leak"
        );
        set("ATO_CRED_SECRETS_RS_ABC123__API_KEY", None);
    }

    #[test]
    fn select_resolver_defaults_to_user_store_and_fails_closed_on_unknown() {
        let set = |k: &str, v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        };
        set("ATO_READY_STATE_SECRET_SOURCE", None);
        assert_eq!(select_resolver("rs-x").unwrap().kind(), "user_store");
        set("ATO_READY_STATE_SECRET_SOURCE", Some("env"));
        assert_eq!(select_resolver("rs-x").unwrap().kind(), "env");
        set("ATO_READY_STATE_SECRET_SOURCE", Some("vault"));
        assert!(
            select_resolver("rs-x").is_err(),
            "typo must fail closed, not fall back"
        );
        set("ATO_READY_STATE_SECRET_SOURCE", None);
    }
}
