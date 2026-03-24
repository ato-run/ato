use super::{
    NacelleBootstrapPolicy, AUTO_BOOTSTRAP_ENV, DEFAULT_NACELLE_RELEASE_BASE_URL,
    DISABLE_NETWORK_BOOTSTRAP_ENV, NACELLE_RELEASE_BASE_URL_ENV, NACELLE_VERSION_ENV, OFFLINE_ENV,
    PINNED_NACELLE_VERSION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutoBootstrapMode {
    Auto,
    Force,
    Disabled,
}

pub(crate) fn resolve_auto_bootstrap_policy_from_env() -> NacelleBootstrapPolicy {
    let mode = parse_auto_bootstrap_mode(std::env::var(AUTO_BOOTSTRAP_ENV).ok().as_deref());
    let version =
        non_empty_env(NACELLE_VERSION_ENV).unwrap_or_else(|| PINNED_NACELLE_VERSION.to_string());
    let release_base_url = configured_nacelle_release_base_url();
    let ci = env_is_truthy("CI");
    let offline = env_is_truthy(OFFLINE_ENV) || env_is_truthy(DISABLE_NETWORK_BOOTSTRAP_ENV);

    resolve_auto_bootstrap_policy(mode, version, release_base_url, ci, offline)
}

pub(super) fn resolve_auto_bootstrap_policy(
    mode: AutoBootstrapMode,
    version: String,
    release_base_url: String,
    ci: bool,
    offline: bool,
) -> NacelleBootstrapPolicy {
    let disabled_reason = if mode == AutoBootstrapMode::Disabled {
        Some(format!("{} disables network bootstrap", AUTO_BOOTSTRAP_ENV))
    } else if offline {
        Some(format!(
            "{} or {} is set",
            OFFLINE_ENV, DISABLE_NETWORK_BOOTSTRAP_ENV
        ))
    } else if ci && mode != AutoBootstrapMode::Force {
        Some("CI environment requires prefetched nacelle".to_string())
    } else {
        None
    };

    NacelleBootstrapPolicy {
        version,
        release_base_url,
        network_allowed: disabled_reason.is_none(),
        disabled_reason,
    }
}

pub(super) fn parse_auto_bootstrap_mode(value: Option<&str>) -> AutoBootstrapMode {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return AutoBootstrapMode::Auto;
    };
    let normalized = value.to_ascii_lowercase();
    match normalized.as_str() {
        "0" | "false" | "off" | "never" | "disable" | "disabled" => AutoBootstrapMode::Disabled,
        "1" | "true" | "on" | "always" | "force" | "enabled" => AutoBootstrapMode::Force,
        _ => AutoBootstrapMode::Auto,
    }
}

pub(super) fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn configured_nacelle_release_base_url() -> String {
    non_empty_env(NACELLE_RELEASE_BASE_URL_ENV)
        .unwrap_or_else(|| DEFAULT_NACELLE_RELEASE_BASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

pub(super) fn env_is_truthy(key: &str) -> bool {
    non_empty_env(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}
