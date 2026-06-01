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

/// RAII guard that installs `ATO_UI_OVERRIDE_PORT=<port>` for its scope,
/// then restores the previous environment value on drop. Used by the
/// warm-launch fast path in `app_control::session` and by the run pipeline's
/// execute phase to make the chosen port visible to the in-process
/// `override_port` reads (and inherited by the child at spawn) without leaking
/// the value past the launch it belongs to.
///
/// The previous value is captured at construction time and restored
/// verbatim — empty / missing / non-numeric strings all round-trip.
///
/// Env-safety: `ATO_UI_OVERRIDE_PORT` is mutated *only* through these guards,
/// and the CLI performs at most one launch at a time within a process, so the
/// only writers run at guard construction / drop and every other access is a
/// read. Restoring on drop guarantees one run's port never bleeds into the
/// next. (Threading the port through `RuntimeLaunchContext` instead of process
/// env — removing the global channel entirely — is the preferred long-term
/// fix but touches every `override_port` reader.)
#[must_use = "PortOverrideGuard restores the env var when dropped; bind it to keep the override active"]
pub struct PortOverrideGuard {
    previous: Option<String>,
}

impl Drop for PortOverrideGuard {
    fn drop(&mut self) {
        // SAFETY: see the type-level note — `ATO_UI_OVERRIDE_PORT` is written
        // only by these guards and only one launch runs at a time, so there is
        // no concurrent writer racing this restore.
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var(ENV_OVERRIDE_PORT, value) },
            None => unsafe { std::env::remove_var(ENV_OVERRIDE_PORT) },
        }
    }
}

/// Install a scoped `ATO_UI_OVERRIDE_PORT` override. The current value
/// (if any) is captured and restored when the returned guard is dropped.
pub fn scoped_override_port(port: u16) -> PortOverrideGuard {
    let previous = std::env::var(ENV_OVERRIDE_PORT).ok();
    // SAFETY: see `PortOverrideGuard` — the override is written only through
    // these guards and the CLI runs one launch at a time, so no other thread
    // is writing `ATO_UI_OVERRIDE_PORT` concurrently.
    unsafe {
        std::env::set_var(ENV_OVERRIDE_PORT, port.to_string());
    }
    PortOverrideGuard { previous }
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
    use super::{
        merged_env, override_env, override_port, scoped_id_override, scoped_override_port,
    };
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn scoped_override_port_restores_and_does_not_leak_between_runs() {
        let _guard = env_lock().lock().expect("env lock");
        // Clean slate so the assertions are deterministic.
        unsafe {
            std::env::remove_var("ATO_UI_OVERRIDE_PORT");
        }
        assert_eq!(override_port(None), None);

        // First run installs an auto-assigned port through the guard.
        {
            let _port_guard = scoped_override_port(4321);
            assert_eq!(override_port(None), Some(4321));
        }

        // After the guard drops (the run completed) the override must be gone,
        // so a second sequential run in the same process does not inherit it.
        assert_eq!(override_port(None), None);
    }

    #[test]
    fn override_port_prefers_env_value() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("ATO_UI_OVERRIDE_PORT", "4010");
        }
        assert_eq!(override_port(Some(3000)), Some(4010));
        unsafe {
            std::env::remove_var("ATO_UI_OVERRIDE_PORT");
        }
    }

    #[test]
    fn override_env_reads_json_map() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("ATO_UI_OVERRIDE_ENV_JSON", r#"{"PORT":"4100","DEBUG":"1"}"#);
        }
        let parsed = override_env();
        assert_eq!(parsed.get("PORT").map(String::as_str), Some("4100"));
        assert_eq!(parsed.get("DEBUG").map(String::as_str), Some("1"));
        unsafe {
            std::env::remove_var("ATO_UI_OVERRIDE_ENV_JSON");
        }
    }

    #[test]
    fn merged_env_overrides_existing_values() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("ATO_UI_OVERRIDE_ENV_JSON", r#"{"PORT":"4200"}"#);
        }

        let mut base = HashMap::new();
        base.insert("PORT".to_string(), "3000".to_string());
        base.insert("NODE_ENV".to_string(), "production".to_string());
        let merged = merged_env(base);

        assert_eq!(merged.get("PORT").map(String::as_str), Some("4200"));
        assert_eq!(
            merged.get("NODE_ENV").map(String::as_str),
            Some("production")
        );

        unsafe {
            std::env::remove_var("ATO_UI_OVERRIDE_ENV_JSON");
        }
    }

    #[test]
    fn scoped_id_override_reads_env() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::set_var("ATO_UI_SCOPED_ID", "capsules/hello-web");
        }
        assert_eq!(scoped_id_override().as_deref(), Some("capsules/hello-web"));
        unsafe {
            std::env::remove_var("ATO_UI_SCOPED_ID");
        }
    }
}
