use std::collections::HashMap;

use serde::Deserialize;

const ENV_OVERRIDE_PORT: &str = "ATO_UI_OVERRIDE_PORT";
const ENV_OVERRIDE_ENV_JSON: &str = "ATO_UI_OVERRIDE_ENV_JSON";
const ENV_SCOPED_ID: &str = "ATO_UI_SCOPED_ID";

#[derive(Debug, Default, Deserialize)]
struct RawOverrides {
    #[serde(default)]
    env: HashMap<String, String>,
}

pub fn override_port(default: Option<u16>) -> Option<u16> {
    let Some(raw) = std::env::var(ENV_OVERRIDE_PORT).ok() else {
        return default;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return default;
    }
    trimmed.parse::<u16>().ok().or(default)
}

pub fn override_env() -> HashMap<String, String> {
    let Some(raw) = std::env::var(ENV_OVERRIDE_ENV_JSON).ok() else {
        return HashMap::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return HashMap::new();
    }

    if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(trimmed) {
        return map
            .into_iter()
            .filter(|(key, _)| !key.trim().is_empty())
            .collect();
    }

    if let Ok(raw) = serde_json::from_str::<RawOverrides>(trimmed) {
        return raw
            .env
            .into_iter()
            .filter(|(key, _)| !key.trim().is_empty())
            .collect();
    }

    HashMap::new()
}

pub fn merged_env(mut base: HashMap<String, String>) -> HashMap<String, String> {
    for (key, value) in override_env() {
        base.insert(key, value);
    }
    base
}

pub fn scoped_id_override() -> Option<String> {
    std::env::var(ENV_SCOPED_ID)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{merged_env, override_env, override_port, scoped_id_override};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn override_port_prefers_env_value() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var("ATO_UI_OVERRIDE_PORT", "4010");
        assert_eq!(override_port(Some(3000)), Some(4010));
        std::env::remove_var("ATO_UI_OVERRIDE_PORT");
    }

    #[test]
    fn override_env_reads_json_map() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var("ATO_UI_OVERRIDE_ENV_JSON", r#"{"PORT":"4100","DEBUG":"1"}"#);
        let parsed = override_env();
        assert_eq!(parsed.get("PORT").map(String::as_str), Some("4100"));
        assert_eq!(parsed.get("DEBUG").map(String::as_str), Some("1"));
        std::env::remove_var("ATO_UI_OVERRIDE_ENV_JSON");
    }

    #[test]
    fn merged_env_overrides_existing_values() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var("ATO_UI_OVERRIDE_ENV_JSON", r#"{"PORT":"4200"}"#);

        let mut base = HashMap::new();
        base.insert("PORT".to_string(), "3000".to_string());
        base.insert("NODE_ENV".to_string(), "production".to_string());
        let merged = merged_env(base);

        assert_eq!(merged.get("PORT").map(String::as_str), Some("4200"));
        assert_eq!(
            merged.get("NODE_ENV").map(String::as_str),
            Some("production")
        );

        std::env::remove_var("ATO_UI_OVERRIDE_ENV_JSON");
    }

    #[test]
    fn scoped_id_override_reads_env() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var("ATO_UI_SCOPED_ID", "capsules/hello-web");
        assert_eq!(scoped_id_override().as_deref(), Some("capsules/hello-web"));
        std::env::remove_var("ATO_UI_SCOPED_ID");
    }
}
