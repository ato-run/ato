//! AI-keyless grant (P3b approach B): resolve the well-known AI bindings from an
//! ato-api-minted per-run grant instead of the runner-host secret store.
//!
//! An app that wants "run with Ato AI" declares the WELL-KNOWN secrets in its
//! capsule.toml — `[secrets.openai_api_key] env = "OPENAI_API_KEY"` (and
//! optionally `[secrets.openai_base_url] env = "OPENAI_BASE_URL"`) — so the
//! binding names are sealed into the supervisor manifest like any other secret.
//! At lease claim, when the sealed names include [`AI_BINDING_API_KEY`], the
//! runner asks ato-api for this RUN's grant (`POST /v1/ai-gateway/grant`,
//! runner-token auth). The server authorizes ONLY a run whose launch opted in
//! (`ai_keyless: true`) — a plain launch gets 403 and resolution falls through
//! to the normal [`SecretResolver`] chain (BYOK from the runner-host store).
//! The minted key is valid only against Ato's metered gateway, per-run,
//! budget-capped, and revoked at run stop — so it never needs to live in any
//! store; it is held in memory for lease issuance/renewal and dropped with the
//! session.
//!
//! Same value-hygiene contract as every resolver: values never appear in logs,
//! errors, or receipts.

use std::collections::BTreeMap;

use anyhow::Result;
use ato_ipc::binding_lease::SecretValue;

use super::secret_resolver::SecretResolver;

/// Well-known binding name for the per-run Ato AI gateway key.
pub(crate) const AI_BINDING_API_KEY: &str = "openai_api_key";
/// Well-known binding name for the gateway base URL (optional in recipes).
pub(crate) const AI_BINDING_BASE_URL: &str = "openai_base_url";

/// True when the sealed binding names include the AI key binding — the trigger
/// for attempting a grant fetch at claim. Apps without it never cause a fetch.
pub(crate) fn wants_ai_grant(names: &[String]) -> bool {
    names.iter().any(|n| n == AI_BINDING_API_KEY)
}

/// Parse the grant endpoint's response into binding-name → value. Pure so it is
/// unit-testable without HTTP:
///   - 200 + `{api_key, base_url}` → `Some({openai_api_key, openai_base_url})`
///   - 401/403/404 → `None` (not opted in / flag off / unknown → BYOK fallback)
///   - anything else → `Err` (transient server trouble; caller logs + falls back)
///
/// The parsed values never appear in errors.
pub(crate) fn parse_grant_response(
    status: u16,
    body: &str,
) -> Result<Option<BTreeMap<String, String>>> {
    match status {
        200 => {
            let v: serde_json::Value = serde_json::from_str(body)
                .map_err(|e| anyhow::anyhow!("ai grant: invalid JSON response: {e}"))?;
            let api_key = v
                .get("api_key")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ai grant: response missing api_key"))?;
            let base_url = v
                .get("base_url")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("ai grant: response missing base_url"))?;
            let mut m = BTreeMap::new();
            m.insert(AI_BINDING_API_KEY.to_string(), api_key.to_string());
            m.insert(AI_BINDING_BASE_URL.to_string(), base_url.to_string());
            Ok(Some(m))
        }
        401 | 403 | 404 => Ok(None),
        other => anyhow::bail!("ai grant: unexpected status {other}"),
    }
}

/// Fetch this run's AI grant from ato-api. `Ok(None)` = not an AI-keyless run
/// (or the feature is off) — the caller falls through to the normal resolver
/// chain. A transport/server error also degrades to `Ok(None)` WITH a warning:
/// the launch must not hard-fail on a gateway hiccup when a BYOK grant might
/// serve; if neither source resolves, the normal preflight failure names the
/// missing binding.
pub(crate) async fn fetch_ai_grant(
    client: &reqwest::Client,
    api_base: &str,
    runner_token: &str,
    run_id: &str,
) -> Option<BTreeMap<String, String>> {
    let url = format!("{}/v1/ai-gateway/grant", api_base.trim_end_matches('/'));
    let res = client
        .post(&url)
        .bearer_auth(runner_token)
        .json(&serde_json::json!({ "run_id": run_id }))
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "ato::ready_state", error = %e, "ai grant fetch failed (falling back to store)");
            return None;
        }
    };
    let status = res.status().as_u16();
    let body = res.text().await.unwrap_or_default();
    match parse_grant_response(status, &body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "ato::ready_state", error = %e, "ai grant fetch degraded (falling back to store)");
            None
        }
    }
}

/// Resolver layered over the normal chain: the pre-fetched AI grant values win
/// for their well-known names; every other name delegates to `inner`. Values
/// live only in this struct (dropped with the session) and never in a store.
pub(crate) struct AiGrantResolver {
    values: BTreeMap<String, String>,
    inner: Box<dyn SecretResolver>,
}

impl AiGrantResolver {
    pub(crate) fn new(values: BTreeMap<String, String>, inner: Box<dyn SecretResolver>) -> Self {
        Self { values, inner }
    }
}

impl SecretResolver for AiGrantResolver {
    fn resolve(&self, binding_name: &str) -> Result<SecretValue> {
        match self.values.get(binding_name) {
            Some(v) => Ok(SecretValue::new(v.clone())),
            None => self.inner.resolve(binding_name),
        }
    }

    fn kind(&self) -> &'static str {
        "ato_ai"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ready_state::secret_resolver::EnvSecretResolver;

    #[test]
    fn wants_ai_grant_triggers_only_on_the_api_key_name() {
        assert!(wants_ai_grant(&["openai_api_key".into()]));
        assert!(wants_ai_grant(&[
            "db_url".into(),
            "openai_api_key".into(),
            "openai_base_url".into()
        ]));
        assert!(!wants_ai_grant(&["openai_base_url".into()]));
        assert!(!wants_ai_grant(&["db_url".into()]));
        assert!(!wants_ai_grant(&[]));
    }

    #[test]
    fn parse_grant_response_maps_statuses() {
        // 200 → both well-known names populated.
        let ok = parse_grant_response(
            200,
            r#"{"api_key":"tok-1","base_url":"https://api.test/v1/ai-gateway","env":{}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ok.get(AI_BINDING_API_KEY).map(String::as_str),
            Some("tok-1")
        );
        assert_eq!(
            ok.get(AI_BINDING_BASE_URL).map(String::as_str),
            Some("https://api.test/v1/ai-gateway")
        );
        // Not-authorized family → None (BYOK fallback), not an error.
        for s in [401, 403, 404] {
            assert!(parse_grant_response(s, "{}").unwrap().is_none(), "{s}");
        }
        // Server trouble → Err (caller warns + falls back).
        assert!(parse_grant_response(500, "boom").is_err());
        // 200 with a missing field is a contract violation → Err, and the error
        // must not echo any value.
        let err = parse_grant_response(200, r#"{"api_key":"tok-2"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("base_url"), "{err}");
        assert!(!err.contains("tok-2"), "error must not carry a value");
    }

    #[test]
    fn resolver_serves_ai_names_and_delegates_the_rest() {
        let set = |k: &str, v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        };
        let mut values = BTreeMap::new();
        values.insert(AI_BINDING_API_KEY.to_string(), "tok-xyz".to_string());
        values.insert(
            AI_BINDING_BASE_URL.to_string(),
            "https://api.test/v1/ai-gateway".to_string(),
        );
        let r = AiGrantResolver::new(values, Box::new(EnvSecretResolver));
        assert_eq!(r.kind(), "ato_ai");
        assert_eq!(r.resolve("openai_api_key").unwrap().expose(), "tok-xyz");
        assert_eq!(
            r.resolve("openai_base_url").unwrap().expose(),
            "https://api.test/v1/ai-gateway"
        );
        // A non-AI name delegates to the inner resolver (env-backed here).
        set("ATO_BINDING_db_url", Some("postgres://x"));
        assert_eq!(r.resolve("db_url").unwrap().expose(), "postgres://x");
        set("ATO_BINDING_db_url", None);
        let err = r.resolve("db_url").unwrap_err().to_string();
        assert!(err.contains("db_url"), "{err}");
        assert!(!err.contains("tok-xyz"), "error must not leak the AI value");
    }
}
