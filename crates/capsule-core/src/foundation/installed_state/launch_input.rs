//! `capsule://` launch-condition query grammar (#508).
//!
//! A `capsule://<location>` URL resolves to a capsule.toml whose launch
//! conditions (port / secret / state / env / hardware …) are *owned by the
//! capsule*. The URL **query** supplies user/runtime-provided **inputs** to
//! those conditions:
//!
//! ```text
//! capsule://<location>?<condition-key>=<value>&<condition-key>=<value>
//! ```
//!
//! Query keys are launch-condition keys *directly* — there is no
//! `condition.* / placement.* / runtime.*` namespace and no `c/p/r` sugar.
//! Placement and runtime are Ato-runtime decisions, not capsule-protocol query
//! inputs. `.` is the only key separator (`::` is rejected). `auto` and `prompt`
//! are **not** accepted as explicit values — when an input is omitted, the
//! runtime performs automatic resolution / prompting per capsule.toml.
//!
//! Reserved condition keys: `port`, `port.<endpoint>`, `env.<name>`,
//! `secret.<name>`, `state.<key>`, `network.<key>`, `hardware.<key>`,
//! `capability.<key>`, `policy.<key>`. This slice supports `port`, `env`,
//! `secret`, `state`; the rest are reserved-but-rejected for now.
//!
//! Security: raw secret values, tokens, and raw host paths are rejected from
//! URLs. Secret/sensitive-env inputs accept only `required` or `grant:<id>`;
//! state accepts only `required`, `use-existing`, or `binding:<id>`.

use url::Url;

use crate::error::{CapsuleError, Result};

/// A parsed `capsule://` launch-condition query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsuleLaunchInput {
    /// `host` + `path` of the `capsule://` URL (e.g. `ato.run/koh0920/hello`).
    pub capsule_location: String,
    pub conditions: Vec<LaunchConditionInput>,
}

/// A single launch-condition input from the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConditionInput {
    pub kind: LaunchConditionInputKind,
    /// The per-kind key (endpoint for port, var name for env/secret, state key).
    pub key: String,
    pub value: LaunchConditionInputValue,
}

/// The kind of a launch-condition input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchConditionInputKind {
    Port,
    Env,
    Secret,
    State,
    Network,
    Hardware,
    Capability,
    Policy,
}

/// The value form of a launch-condition input. Never a raw secret/path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchConditionInputValue {
    /// The condition is required; the runtime/user must provide it (no locator).
    Required,
    /// Use an existing secret grant by id (`grant:<id>`).
    Grant(String),
    /// Use an existing logical state binding if available (`use-existing`).
    UseExisting,
    /// Use a known logical state binding by id (`binding:<id>`).
    Binding(String),
    /// A non-sensitive literal value (e.g. a port number, a plain env value).
    Literal(String),
}

/// Env-var names that imply a secret/credential (case-insensitive substring).
const SENSITIVE_ENV_MARKERS: &[&str] = &[
    "SECRET",
    "TOKEN",
    "API_KEY",
    "PASSWORD",
    "CREDENTIAL",
    "PRIVATE_KEY",
    "ACCESS_KEY",
];

/// Query-key first segments that are explicitly not part of the capsule launch
/// condition grammar (no namespacing, no sugar).
const FORBIDDEN_NAMESPACES: &[&str] = &["condition", "placement", "runtime", "c", "p", "r"];

fn parse_err(msg: impl Into<String>) -> CapsuleError {
    CapsuleError::Runtime(msg.into())
}

/// Parse a `capsule://<location>?<query>` URL into typed launch-condition inputs.
pub fn parse_capsule_launch_input(url: &str) -> Result<CapsuleLaunchInput> {
    let parsed = Url::parse(url).map_err(|e| parse_err(format!("invalid capsule URL: {e}")))?;
    if parsed.scheme() != "capsule" {
        return Err(parse_err(format!(
            "launch-condition query grammar is only for capsule:// (got '{}://')",
            parsed.scheme()
        )));
    }

    let host = parsed.host_str().unwrap_or("");
    let capsule_location = format!("{host}{}", parsed.path());

    let mut conditions = Vec::new();
    let mut seen: Vec<(LaunchConditionInputKind, String)> = Vec::new();
    for (key, value) in parsed.query_pairs() {
        let input = parse_condition_input(&key, &value)?;
        let identity = (input.kind, input.key.clone());
        if seen.contains(&identity) {
            return Err(parse_err(format!("duplicate condition key '{key}'")));
        }
        seen.push(identity);
        conditions.push(input);
    }

    Ok(CapsuleLaunchInput {
        capsule_location,
        conditions,
    })
}

fn parse_condition_input(key: &str, value: &str) -> Result<LaunchConditionInput> {
    if key.contains("::") {
        return Err(parse_err(format!(
            "'::' is not allowed in a capsule query key (got '{key}'); use '.' as the separator"
        )));
    }
    let (namespace, rest) = key.split_once('.').unwrap_or((key, ""));
    if FORBIDDEN_NAMESPACES.contains(&namespace) {
        return Err(parse_err(format!(
            "'{namespace}.*' is not a capsule launch-condition namespace; \
             query keys are condition keys directly (port/env/secret/state)"
        )));
    }
    if value == "auto" || value == "prompt" {
        return Err(parse_err(format!(
            "'{value}' is not allowed as an explicit query value for '{key}'; \
             omit the input to let the runtime resolve or prompt"
        )));
    }

    match namespace {
        "port" => {
            let endpoint = if rest.is_empty() { "main" } else { rest };
            let port = parse_port_value(value)?;
            Ok(LaunchConditionInput {
                kind: LaunchConditionInputKind::Port,
                key: endpoint.to_string(),
                value: LaunchConditionInputValue::Literal(port.to_string()),
            })
        }
        "env" => {
            let name = require_subkey(rest, "env")?;
            Ok(LaunchConditionInput {
                kind: LaunchConditionInputKind::Env,
                key: name.to_string(),
                value: parse_env_value(name, value)?,
            })
        }
        "secret" => {
            let name = require_subkey(rest, "secret")?;
            Ok(LaunchConditionInput {
                kind: LaunchConditionInputKind::Secret,
                key: name.to_string(),
                value: parse_secret_value(value)?,
            })
        }
        "state" => {
            let state_key = require_subkey(rest, "state")?;
            Ok(LaunchConditionInput {
                kind: LaunchConditionInputKind::State,
                key: state_key.to_string(),
                value: parse_state_value(value)?,
            })
        }
        "network" | "hardware" | "capability" | "policy" => Err(parse_err(format!(
            "reserved condition key '{namespace}' is not yet supported in capsule query inputs"
        ))),
        _ => Err(parse_err(format!("unknown capsule query key '{key}'"))),
    }
}

fn require_subkey<'a>(rest: &'a str, namespace: &str) -> Result<&'a str> {
    if rest.is_empty() {
        return Err(parse_err(format!("'{namespace}.<name>' requires a name")));
    }
    Ok(rest)
}

fn parse_port_value(value: &str) -> Result<u16> {
    let port: u16 = value
        .parse()
        .map_err(|_| parse_err(format!("port must be an integer 1..=65535, got '{value}'")))?;
    if port == 0 {
        return Err(parse_err(
            "port 0 (auto-assign) is not an explicit query value; omit to let the runtime assign",
        ));
    }
    Ok(port)
}

fn parse_secret_value(value: &str) -> Result<LaunchConditionInputValue> {
    if value == "required" {
        return Ok(LaunchConditionInputValue::Required);
    }
    if let Some(id) = value.strip_prefix("grant:") {
        validate_locator_id(id, "grant")?;
        return Ok(LaunchConditionInputValue::Grant(id.to_string()));
    }
    Err(parse_err(
        "secret input must be 'required' or 'grant:<id>'; \
         raw secret values / tokens are never accepted in a URL",
    ))
}

fn parse_state_value(value: &str) -> Result<LaunchConditionInputValue> {
    match value {
        "required" => Ok(LaunchConditionInputValue::Required),
        "use-existing" => Ok(LaunchConditionInputValue::UseExisting),
        _ if value.starts_with("binding:") => {
            let id = &value["binding:".len()..];
            if looks_like_host_path(id) {
                return Err(parse_err(
                    "state binding id must be a logical id, not a host path",
                ));
            }
            validate_locator_id(id, "binding")?;
            Ok(LaunchConditionInputValue::Binding(id.to_string()))
        }
        _ if looks_like_host_path(value) => Err(parse_err(
            "raw host paths are not allowed in state input; use 'use-existing' or 'binding:<id>'",
        )),
        _ => Err(parse_err(
            "state input must be 'required', 'use-existing', or 'binding:<id>'",
        )),
    }
}

fn parse_env_value(name: &str, value: &str) -> Result<LaunchConditionInputValue> {
    if is_sensitive_env_name(name) {
        if value == "required" {
            return Ok(LaunchConditionInputValue::Required);
        }
        if let Some(id) = value.strip_prefix("grant:") {
            validate_locator_id(id, "grant")?;
            return Ok(LaunchConditionInputValue::Grant(id.to_string()));
        }
        return Err(parse_err(format!(
            "env '{name}' looks sensitive; its input must be 'required' or 'grant:<id>', \
             never a literal value"
        )));
    }
    if looks_like_secret_value(value) {
        return Err(parse_err(format!(
            "env '{name}' value looks secret-like and is rejected; \
             pass sensitive values via 'grant:<id>'"
        )));
    }
    Ok(LaunchConditionInputValue::Literal(value.to_string()))
}

/// Validate a **reserved launch-condition key** — the same vocabulary as a
/// `capsule://` query key: `port`, `port.<endpoint>`, `env.<name>`,
/// `secret.<name>`, `state.<key>`, or a reserved
/// `network`/`hardware`/`capability`/`policy` key.
///
/// A condition key is **not** a URI and does not use a `#condition/...`
/// fragment: condition identity is the reserved key itself, exactly as written
/// in the `capsule://` query. `://`, `::`, path separators, and the forbidden
/// namespaces (condition/placement/runtime/c/p/r) are rejected — so an
/// `ato-secret://…`, `capsule://…#condition/…`, raw host path, or raw token can
/// never be used as a condition key.
pub fn validate_condition_key(condition_key: &str) -> Result<()> {
    if condition_key.is_empty() {
        return Err(parse_err("condition key must not be empty"));
    }
    if condition_key.contains("::") {
        return Err(parse_err(format!(
            "'::' is not allowed in a condition key (got '{condition_key}')"
        )));
    }
    if condition_key.contains("://") {
        return Err(parse_err(format!(
            "condition key must be a reserved key (e.g. secret.OPENAI_API_KEY), not a URI \
             (got '{condition_key}')"
        )));
    }
    if condition_key.contains('/') || condition_key.contains('\\') {
        return Err(parse_err(format!(
            "condition key must not contain a path separator (got '{condition_key}')"
        )));
    }
    let (namespace, rest) = condition_key.split_once('.').unwrap_or((condition_key, ""));
    if FORBIDDEN_NAMESPACES.contains(&namespace) {
        return Err(parse_err(format!(
            "'{namespace}.*' is not a launch-condition namespace; condition keys are reserved \
             keys directly (port/env/secret/state/…)"
        )));
    }
    match namespace {
        // `port` (main endpoint) or `port.<endpoint>`.
        "port" => Ok(()),
        "env" | "secret" | "state" | "network" | "hardware" | "capability" | "policy" => {
            if rest.is_empty() {
                return Err(parse_err(format!("'{namespace}.<name>' requires a name")));
            }
            Ok(())
        }
        _ => Err(parse_err(format!(
            "unknown condition key '{condition_key}'"
        ))),
    }
}

/// A logical locator id (grant id / binding id): non-empty, no whitespace, not a
/// host path, not a scheme URL, not a token-like raw value. Public so the DB
/// registry can enforce it at its boundary (callers are not trusted).
pub fn validate_locator_id(id: &str, what: &str) -> Result<()> {
    if id.is_empty() {
        return Err(parse_err(format!("{what} id must not be empty")));
    }
    if id.chars().any(char::is_whitespace) {
        return Err(parse_err(format!("{what} id must not contain whitespace")));
    }
    if id.contains("://") {
        return Err(parse_err(format!(
            "{what} id must be a logical id, not a URL/scheme"
        )));
    }
    if looks_like_host_path(id) {
        return Err(parse_err(format!(
            "{what} id must be a logical id, not a host path"
        )));
    }
    if looks_like_secret_value(id) {
        return Err(parse_err(format!(
            "{what} id looks like a raw token; pass a short logical id instead"
        )));
    }
    Ok(())
}

fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    SENSITIVE_ENV_MARKERS
        .iter()
        .any(|marker| upper.contains(marker))
}

/// Heuristic: does this value look like a raw secret / token (so it must not
/// appear in a URL)? Conservative — false positives only cost a clearer error.
fn looks_like_secret_value(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("sk-")
        || lower.starts_with("bearer ")
        || lower.starts_with("xoxb-")
        || lower.starts_with("ghp_")
        || lower.starts_with("eyj") // JWT
        || value.contains("://")
        || value.len() > 40
}

/// Heuristic: does this value look like a raw host filesystem path?
fn looks_like_host_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with('~')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains('\\')
        || value.contains("://")
        || (value.len() >= 2 && value.as_bytes()[1] == b':') // C:\ style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> CapsuleLaunchInput {
        parse_capsule_launch_input(url).expect("must parse")
    }

    const BASE: &str = "capsule://ato.run/koh0920/hello-capsule";

    #[test]
    fn capsule_launch_input_parses_port_number() {
        let input = parse(&format!("{BASE}?port=3001"));
        assert_eq!(input.capsule_location, "ato.run/koh0920/hello-capsule");
        assert_eq!(input.conditions.len(), 1);
        let c = &input.conditions[0];
        assert_eq!(c.kind, LaunchConditionInputKind::Port);
        assert_eq!(c.key, "main");
        assert_eq!(
            c.value,
            LaunchConditionInputValue::Literal("3001".to_string())
        );
    }

    #[test]
    fn capsule_launch_input_parses_endpoint_port() {
        let input = parse(&format!("{BASE}?port.admin=8080"));
        assert_eq!(input.conditions[0].key, "admin");
        assert_eq!(
            input.conditions[0].value,
            LaunchConditionInputValue::Literal("8080".to_string())
        );
    }

    #[test]
    fn capsule_launch_input_parses_secret_grant_id() {
        let input = parse(&format!(
            "{BASE}?secret.OPENAI_API_KEY=grant:openai-default"
        ));
        let c = &input.conditions[0];
        assert_eq!(c.kind, LaunchConditionInputKind::Secret);
        assert_eq!(c.key, "OPENAI_API_KEY");
        assert_eq!(
            c.value,
            LaunchConditionInputValue::Grant("openai-default".to_string())
        );
    }

    #[test]
    fn capsule_launch_input_parses_state_use_existing() {
        let input = parse(&format!("{BASE}?state.data=use-existing"));
        assert_eq!(
            input.conditions[0].value,
            LaunchConditionInputValue::UseExisting
        );
    }

    #[test]
    fn capsule_launch_input_parses_state_binding_id() {
        let input = parse(&format!("{BASE}?state.data=binding:user-data"));
        assert_eq!(
            input.conditions[0].value,
            LaunchConditionInputValue::Binding("user-data".to_string())
        );
    }

    #[test]
    fn capsule_launch_input_parses_env_literal_and_state() {
        let input = parse(&format!(
            "{BASE}?env.LOG_LEVEL=debug&state.data=binding:user-data"
        ));
        assert_eq!(input.conditions.len(), 2);
        let env = input
            .conditions
            .iter()
            .find(|c| c.kind == LaunchConditionInputKind::Env)
            .unwrap();
        assert_eq!(
            env.value,
            LaunchConditionInputValue::Literal("debug".to_string())
        );
    }

    #[test]
    fn capsule_launch_input_rejects_auto_value() {
        assert!(parse_capsule_launch_input(&format!("{BASE}?port=auto")).is_err());
    }

    #[test]
    fn capsule_launch_input_rejects_prompt_value() {
        assert!(
            parse_capsule_launch_input(&format!("{BASE}?secret.OPENAI_API_KEY=prompt")).is_err()
        );
    }

    #[test]
    fn capsule_launch_input_rejects_condition_namespace() {
        assert!(
            parse_capsule_launch_input(&format!("{BASE}?condition.secret.OPENAI_API_KEY=prompt"))
                .is_err()
        );
    }

    #[test]
    fn capsule_launch_input_rejects_placement_namespace() {
        assert!(
            parse_capsule_launch_input(&format!("{BASE}?placement.host=user.device.MacBook"))
                .is_err()
        );
    }

    #[test]
    fn capsule_launch_input_rejects_runtime_namespace() {
        assert!(parse_capsule_launch_input(&format!("{BASE}?runtime.profile=debug")).is_err());
    }

    #[test]
    fn capsule_launch_input_rejects_cpr_sugar_namespaces() {
        for ns in ["c", "p", "r"] {
            assert!(
                parse_capsule_launch_input(&format!("{BASE}?{ns}.secret.X=prompt")).is_err(),
                "{ns}.* sugar must be rejected"
            );
        }
    }

    #[test]
    fn capsule_launch_input_rejects_double_colon_query_keys() {
        assert!(
            parse_capsule_launch_input(&format!("{BASE}?secret::OPENAI_API_KEY=prompt")).is_err()
        );
    }

    #[test]
    fn capsule_launch_input_rejects_raw_secret_value() {
        assert!(
            parse_capsule_launch_input(&format!("{BASE}?secret.OPENAI_API_KEY=sk-abc123")).is_err()
        );
    }

    #[test]
    fn capsule_launch_input_rejects_raw_host_path_state() {
        assert!(parse_capsule_launch_input(&format!("{BASE}?state.data=/Users/koh/data")).is_err());
        assert!(parse_capsule_launch_input(&format!("{BASE}?state.data=binding:/home/x")).is_err());
    }

    #[test]
    fn capsule_launch_input_rejects_other_scheme() {
        assert!(parse_capsule_launch_input("ato://app/ipk/main?port=3001").is_err());
    }

    #[test]
    fn capsule_launch_input_rejects_port_zero_and_unknown_key() {
        assert!(parse_capsule_launch_input(&format!("{BASE}?port=0")).is_err());
        assert!(parse_capsule_launch_input(&format!("{BASE}?bogus.key=1")).is_err());
        assert!(parse_capsule_launch_input(&format!("{BASE}?network.vpc=x")).is_err());
    }

    #[test]
    fn sensitive_env_requires_grant_not_literal() {
        assert!(parse_capsule_launch_input(&format!("{BASE}?env.MY_TOKEN=abc")).is_err());
        let ok = parse(&format!("{BASE}?env.MY_TOKEN=grant:tok-1"));
        assert_eq!(
            ok.conditions[0].value,
            LaunchConditionInputValue::Grant("tok-1".to_string())
        );
    }

    #[test]
    fn condition_key_accepts_reserved_keys() {
        for key in [
            "port",
            "port.main",
            "env.LOG_LEVEL",
            "secret.OPENAI_API_KEY",
            "state.data",
            "network.egress",
        ] {
            validate_condition_key(key).unwrap_or_else(|e| panic!("'{key}' should be valid: {e}"));
        }
    }

    #[test]
    fn condition_key_rejects_uris_fragments_paths_and_namespaces() {
        // The previous `#condition/...` fragment form is no longer a condition
        // identity — a condition key is the reserved key itself, not a URI.
        assert!(validate_condition_key("capsule://x#condition/secret/K").is_err());
        assert!(validate_condition_key("ato-secret://store/openai").is_err());
        assert!(validate_condition_key("ato-state://app/data").is_err());
        assert!(validate_condition_key("file:///Users/koh/secret").is_err());
        assert!(validate_condition_key("/Users/koh/secret").is_err());
        assert!(validate_condition_key("condition.secret.K").is_err());
        assert!(validate_condition_key("secret::K").is_err());
        assert!(
            validate_condition_key("secret").is_err(),
            "secret needs a name"
        );
        assert!(validate_condition_key("bogus.key").is_err());
    }

    #[test]
    fn validate_locator_id_rejects_paths_tokens_and_schemes() {
        assert!(validate_locator_id("openai-default", "grant").is_ok());
        assert!(validate_locator_id("", "grant").is_err());
        assert!(validate_locator_id("sk-abc123", "grant").is_err());
        assert!(validate_locator_id("/Users/x", "binding").is_err());
        assert!(validate_locator_id("ato-secret://x", "grant").is_err());
        assert!(validate_locator_id("has space", "grant").is_err());
    }
}
