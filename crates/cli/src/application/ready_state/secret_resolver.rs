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
            .ok_or_else(|| anyhow::anyhow!("binding '{binding_name}' has no value; set {env} (preview secret source)"))?;
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
                anyhow::bail!("{} resolver for '{binding_name}' is not implemented yet (fail-closed)", $kind)
            }
            fn kind(&self) -> &'static str {
                $kind
            }
        }
    };
}

unimplemented_resolver!(VaultSecretResolver, "vault", "Vault-backed resolver (future).");
unimplemented_resolver!(UserSecretStoreResolver, "user_store", "Per-user secret store resolver (future).");
unimplemented_resolver!(CloudSecretResolver, "cloud", "Cloud secret-manager resolver (future).");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_resolver_reads_value_and_fails_closed_on_missing() {
        let set = |k: &str, v: Option<&str>| unsafe {
            match v { Some(v) => std::env::set_var(k, v), None => std::env::remove_var(k) }
        };
        let r = EnvSecretResolver;
        assert_eq!(r.kind(), "env");
        set("ATO_BINDING_api_key", Some("sk-l3-secret"));
        assert_eq!(r.resolve("api_key").unwrap().expose(), "sk-l3-secret");
        set("ATO_BINDING_api_key", None);
        let err = r.resolve("api_key").unwrap_err().to_string();
        assert!(err.contains("api_key") && err.contains("ATO_BINDING_api_key"), "{err}");
        assert!(!err.contains("sk-l3-secret"), "error must not carry a value");
    }

    #[test]
    fn future_resolvers_fail_closed() {
        assert!(VaultSecretResolver.resolve("x").is_err());
        assert!(UserSecretStoreResolver.resolve("x").is_err());
        assert!(CloudSecretResolver.resolve("x").is_err());
        assert_eq!(VaultSecretResolver.kind(), "vault");
    }
}
