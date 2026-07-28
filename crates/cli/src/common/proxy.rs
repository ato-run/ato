#[cfg(test)]
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};

const ENV_SOCKS_PORT: &str = "ATO_TSNET_SOCKS_PORT";

/// SOCKS port resolved by the in-process sidecar (`maybe_start_sidecar`).
///
/// The sidecar may be asked to bind port `0` (OS-assigned); the *resolved*
/// port is only known after it reports ready. This used to be published back
/// through `std::env::set_var(ATO_TSNET_SOCKS_PORT, ..)`, but that ran while
/// the sidecar's Tokio runtime worker threads were still alive — undefined
/// behaviour under the Rust 2024 `set_var` contract. The value is consumed
/// purely in-process (by `proxy_env_from_env`), so a thread-safe process
/// global is the correct channel and no environment mutation is needed.
static RESOLVED_SOCKS_PORT: AtomicU32 = AtomicU32::new(0);

/// Publish the SOCKS port resolved by the in-process sidecar. `0` means unset.
pub fn set_resolved_socks_port(port: u16) {
    RESOLVED_SOCKS_PORT.store(u32::from(port), Ordering::Relaxed);
}

fn resolved_socks_port() -> Option<u16> {
    match RESOLVED_SOCKS_PORT.load(Ordering::Relaxed) {
        0 => None,
        port => u16::try_from(port).ok(),
    }
}

#[derive(Debug, Clone)]
pub struct ProxyEnv {
    pub http_proxy: String,
    pub https_proxy: String,
    pub all_proxy: String,
    pub no_proxy: String,
}

pub fn proxy_env_for_socks5(port: u16, extra_no_proxy: &[String]) -> ProxyEnv {
    let proxy_url = format!("socks5h://127.0.0.1:{port}");
    let mut entries: Vec<String> = vec!["localhost".to_string(), "127.0.0.1".to_string()];

    for entry in extra_no_proxy {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !entries.iter().any(|existing| existing == trimmed) {
            entries.push(trimmed.to_string());
        }
    }

    if let Ok(existing_no_proxy) = std::env::var("NO_PROXY") {
        for entry in existing_no_proxy.split(',') {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !entries.iter().any(|existing| existing == trimmed) {
                entries.push(trimmed.to_string());
            }
        }
    }

    ProxyEnv {
        http_proxy: proxy_url.clone(),
        https_proxy: proxy_url.clone(),
        all_proxy: proxy_url,
        no_proxy: entries.join(","),
    }
}

pub fn proxy_env_from_env(extra_no_proxy: &[String]) -> Result<Option<ProxyEnv>> {
    // Prefer the port resolved by the in-process sidecar. Fall back to an
    // inherited ATO_TSNET_SOCKS_PORT for child processes that received a
    // pre-resolved port from their parent's environment.
    if let Some(port) = resolved_socks_port() {
        return Ok(Some(proxy_env_for_socks5(port, extra_no_proxy)));
    }
    let raw = match std::env::var(ENV_SOCKS_PORT) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let port: u16 = trimmed
        .parse()
        .with_context(|| format!("invalid {ENV_SOCKS_PORT}: {trimmed}"))?;

    Ok(Some(proxy_env_for_socks5(port, extra_no_proxy)))
}

/// Build a `ProxyEnv` pointing at a local HTTP CONNECT proxy.
///
/// Used for `ato-netd` egress proxy injection. All proxy vars point to
/// `http://127.0.0.1:<port>` rather than a `socks5h://` URL.
pub fn proxy_env_for_http_connect(port: u16, extra_no_proxy: &[String]) -> ProxyEnv {
    let proxy_url = format!("http://127.0.0.1:{port}");
    let mut entries: Vec<String> = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];

    for entry in extra_no_proxy {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !entries.iter().any(|existing| existing == trimmed) {
            entries.push(trimmed.to_string());
        }
    }

    if let Ok(existing) = std::env::var("NO_PROXY") {
        for entry in existing.split(',') {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !entries.iter().any(|existing| existing == trimmed) {
                entries.push(trimmed.to_string());
            }
        }
    }

    ProxyEnv {
        http_proxy: proxy_url.clone(),
        https_proxy: proxy_url.clone(),
        all_proxy: proxy_url,
        no_proxy: entries.join(","),
    }
}

/// Return all 8 proxy environment variable pairs (uppercase + lowercase) for
/// injecting into a workload's environment. The caller can pass these directly
/// to `RuntimeLaunchContext::extend_injected_env`.
pub fn proxy_env_to_pairs(proxy: &ProxyEnv) -> Vec<(String, String)> {
    vec![
        ("HTTP_PROXY".to_string(), proxy.http_proxy.clone()),
        ("HTTPS_PROXY".to_string(), proxy.https_proxy.clone()),
        ("ALL_PROXY".to_string(), proxy.all_proxy.clone()),
        ("NO_PROXY".to_string(), proxy.no_proxy.clone()),
        ("http_proxy".to_string(), proxy.http_proxy.clone()),
        ("https_proxy".to_string(), proxy.https_proxy.clone()),
        ("all_proxy".to_string(), proxy.all_proxy.clone()),
        ("no_proxy".to_string(), proxy.no_proxy.clone()),
    ]
}

/// Build a `ProxyEnv` pointing at the `ato-netd` HTTP CONNECT proxy for use inside
/// OCI containers.
///
/// Unlike [`proxy_env_for_http_connect`] which uses `127.0.0.1`, containers cannot
/// reach the loopback address of the host.  `host.containers.internal` resolves to
/// the host gateway via `--add-host=host.containers.internal:host-gateway` and is
/// the Podman-standard way to reach host services from inside a container.
///
/// `extra_no_proxy` should contain all peer service aliases in the orchestration
/// network so that inter-service traffic stays inside the container network and
/// does not round-trip through the egress proxy.
pub fn proxy_env_for_oci_container(port: u16, extra_no_proxy: &[&str]) -> ProxyEnv {
    let proxy_url = format!("http://host.containers.internal:{port}");
    let mut entries: Vec<String> = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];

    for entry in extra_no_proxy {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !entries.iter().any(|existing| existing == trimmed) {
            entries.push(trimmed.to_string());
        }
    }

    ProxyEnv {
        http_proxy: proxy_url.clone(),
        https_proxy: proxy_url.clone(),
        all_proxy: proxy_url,
        no_proxy: entries.join(","),
    }
}

/// The constant `--add-host` entry that allows OCI containers to reach host services.
pub const OCI_HOST_GATEWAY_ENTRY: &str = "host.containers.internal:host-gateway";

/// The list of 8 proxy variable names (uppercase + lowercase) that must be stripped
/// from a container env when `egress_proxy = false`.
pub const PROXY_ENV_KEYS: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

pub fn apply_proxy_env(cmd: &mut std::process::Command, proxy: &ProxyEnv) {
    cmd.env("HTTP_PROXY", &proxy.http_proxy)
        .env("HTTPS_PROXY", &proxy.https_proxy)
        .env("ALL_PROXY", &proxy.all_proxy)
        .env("NO_PROXY", &proxy.no_proxy)
        .env("http_proxy", &proxy.http_proxy)
        .env("https_proxy", &proxy.https_proxy)
        .env("all_proxy", &proxy.all_proxy)
        .env("no_proxy", &proxy.no_proxy);
}

#[cfg(test)]
pub fn extend_env_map(env: &mut HashMap<String, String>, proxy: &ProxyEnv) {
    env.insert("HTTP_PROXY".to_string(), proxy.http_proxy.clone());
    env.insert("HTTPS_PROXY".to_string(), proxy.https_proxy.clone());
    env.insert("ALL_PROXY".to_string(), proxy.all_proxy.clone());
    env.insert("NO_PROXY".to_string(), proxy.no_proxy.clone());
    env.insert("http_proxy".to_string(), proxy.http_proxy.clone());
    env.insert("https_proxy".to_string(), proxy.https_proxy.clone());
    env.insert("all_proxy".to_string(), proxy.all_proxy.clone());
    env.insert("no_proxy".to_string(), proxy.no_proxy.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        original: String,
        // Hold the crate-wide env lock for the guard's whole lifetime. `NO_PROXY`
        // is a process-global variable, and the sibling
        // `proxy_env_reads_existing_no_proxy` mutates it too; under libtest's
        // parallel scheduler that guard's `remove_var`-on-drop could fire mid-read
        // here, dropping `existing.com` and failing the assert. Sharing the one
        // documented lock (`crate::tests::env_lock`) serializes every env-touching
        // test across the crate. Declared last so it is released AFTER the env
        // restore below: `Drop::drop` runs before fields are dropped, so the
        // restore still happens while the lock is held.
        _env_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var(self.key);
            }
            if !self.original.is_empty() {
                unsafe {
                    std::env::set_var(self.key, &self.original);
                }
            }
        }
    }

    fn env_guard(key: &'static str, value: &str) -> EnvGuard {
        let env_lock = crate::tests::env_lock().lock().expect("env lock");
        let original = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        EnvGuard {
            key,
            original: original.unwrap_or_default(),
            _env_lock: env_lock,
        }
    }

    #[test]
    fn resolved_socks_port_drives_proxy_env_without_env_var() {
        // The in-process sidecar publishes its resolved port through the
        // thread-safe global rather than `std::env::set_var`; the proxy
        // injection path must read it from there. This takes precedence over
        // any inherited ATO_TSNET_SOCKS_PORT.
        set_resolved_socks_port(1085);
        let env = proxy_env_from_env(&[])
            .expect("proxy_env_from_env should not error")
            .expect("a resolved socks port must yield proxy env");
        assert_eq!(env.http_proxy, "socks5h://127.0.0.1:1085");
        assert_eq!(env.all_proxy, "socks5h://127.0.0.1:1085");
        // Reset so the process-global does not leak into sibling tests.
        set_resolved_socks_port(0);
    }

    #[test]
    fn proxy_env_builds_expected_urls() {
        let env = proxy_env_for_socks5(1080, &[]);
        assert_eq!(env.http_proxy, "socks5h://127.0.0.1:1080");
        assert_eq!(env.https_proxy, "socks5h://127.0.0.1:1080");
        assert_eq!(env.all_proxy, "socks5h://127.0.0.1:1080");
        assert!(env.no_proxy.contains("localhost"));
        assert!(env.no_proxy.contains("127.0.0.1"));
    }

    #[test]
    fn proxy_env_dedupes_no_proxy_entries() {
        let extras = vec!["localhost".to_string(), "example.com".to_string()];
        let env = proxy_env_for_socks5(8080, &extras);
        let parts: Vec<&str> = env.no_proxy.split(',').collect();
        assert!(parts.contains(&"localhost"));
        assert!(parts.contains(&"example.com"));
        let localhost_count = parts.iter().filter(|p| **p == "localhost").count();
        assert_eq!(localhost_count, 1);
    }

    #[test]
    fn extend_env_map_inserts_proxy_values() {
        let env = proxy_env_for_socks5(3128, &[]);
        let mut map = HashMap::new();
        extend_env_map(&mut map, &env);
        assert_eq!(map.get("HTTP_PROXY"), Some(&env.http_proxy));
        assert_eq!(map.get("HTTPS_PROXY"), Some(&env.https_proxy));
        assert_eq!(map.get("ALL_PROXY"), Some(&env.all_proxy));
        assert_eq!(map.get("NO_PROXY"), Some(&env.no_proxy));
    }

    #[test]
    fn proxy_env_reads_existing_no_proxy() {
        let _guard = env_guard("NO_PROXY", "existing.com,other.com");

        let env = proxy_env_for_socks5(1080, &[]);
        let parts: Vec<&str> = env.no_proxy.split(',').collect();

        assert!(parts.contains(&"localhost"));
        assert!(parts.contains(&"127.0.0.1"));
        assert!(parts.contains(&"existing.com"));
        assert!(parts.contains(&"other.com"));
    }

    #[test]
    fn proxy_env_appends_to_existing_no_proxy() {
        let _guard = env_guard("NO_PROXY", "existing.com");

        let extras = vec!["new.entry.com".to_string()];
        let env = proxy_env_for_socks5(1080, &extras);
        let parts: Vec<&str> = env.no_proxy.split(',').collect();

        assert!(parts.contains(&"localhost"));
        assert!(parts.contains(&"127.0.0.1"));
        assert!(parts.contains(&"existing.com"));
        assert!(parts.contains(&"new.entry.com"));
    }

    #[test]
    fn http_connect_proxy_env_builds_expected_url() {
        let env = proxy_env_for_http_connect(8888, &[]);
        assert_eq!(env.http_proxy, "http://127.0.0.1:8888");
        assert_eq!(env.https_proxy, "http://127.0.0.1:8888");
        assert_eq!(env.all_proxy, "http://127.0.0.1:8888");
        let parts: Vec<&str> = env.no_proxy.split(',').collect();
        assert!(parts.contains(&"localhost"));
        assert!(parts.contains(&"127.0.0.1"));
        assert!(parts.contains(&"::1"));
    }

    #[test]
    fn http_connect_proxy_env_merges_extra_no_proxy() {
        let extras = vec!["internal.corp".to_string(), "127.0.0.1".to_string()];
        let env = proxy_env_for_http_connect(9999, &extras);
        let parts: Vec<&str> = env.no_proxy.split(',').collect();
        assert!(parts.contains(&"internal.corp"));
        // dedup: 127.0.0.1 should appear exactly once
        let count = parts.iter().filter(|p| **p == "127.0.0.1").count();
        assert_eq!(count, 1);
    }

    #[test]
    fn proxy_env_to_pairs_returns_8_entries() {
        let env = proxy_env_for_http_connect(7777, &[]);
        let pairs = proxy_env_to_pairs(&env);
        assert_eq!(pairs.len(), 8);
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"HTTP_PROXY"));
        assert!(keys.contains(&"HTTPS_PROXY"));
        assert!(keys.contains(&"ALL_PROXY"));
        assert!(keys.contains(&"NO_PROXY"));
        assert!(keys.contains(&"http_proxy"));
        assert!(keys.contains(&"https_proxy"));
        assert!(keys.contains(&"all_proxy"));
        assert!(keys.contains(&"no_proxy"));
        // uppercase and lowercase values must match
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("HTTP_PROXY"), get("http_proxy"));
        assert_eq!(get("NO_PROXY"), get("no_proxy"));
    }

    #[test]
    fn extend_env_map_inserts_lowercase_variants() {
        let env = proxy_env_for_socks5(3128, &[]);
        let mut map = HashMap::new();
        extend_env_map(&mut map, &env);
        assert_eq!(map.get("HTTP_PROXY"), Some(&env.http_proxy));
        assert_eq!(map.get("http_proxy"), Some(&env.http_proxy));
        assert_eq!(map.get("NO_PROXY"), Some(&env.no_proxy));
        assert_eq!(map.get("no_proxy"), Some(&env.no_proxy));
    }
}
