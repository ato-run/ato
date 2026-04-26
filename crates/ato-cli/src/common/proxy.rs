#[cfg(test)]
use std::collections::HashMap;

use anyhow::{Context, Result};

const ENV_SOCKS_PORT: &str = "ATO_TSNET_SOCKS_PORT";

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

pub fn apply_proxy_env(cmd: &mut std::process::Command, proxy: &ProxyEnv) {
    cmd.env("HTTP_PROXY", &proxy.http_proxy)
        .env("HTTPS_PROXY", &proxy.https_proxy)
        .env("ALL_PROXY", &proxy.all_proxy)
        .env("NO_PROXY", &proxy.no_proxy);
}

#[cfg(test)]
pub fn extend_env_map(env: &mut HashMap<String, String>, proxy: &ProxyEnv) {
    env.insert("HTTP_PROXY".to_string(), proxy.http_proxy.clone());
    env.insert("HTTPS_PROXY".to_string(), proxy.https_proxy.clone());
    env.insert("ALL_PROXY".to_string(), proxy.all_proxy.clone());
    env.insert("NO_PROXY".to_string(), proxy.no_proxy.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard(&'static str, String);

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
            if !self.1.is_empty() {
                std::env::set_var(self.0, &self.1);
            }
        }
    }

    fn env_guard(key: &'static str, value: &str) -> EnvGuard {
        let original = std::env::var(key).ok();
        std::env::set_var(key, value);
        EnvGuard(key, original.unwrap_or_default())
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
}
