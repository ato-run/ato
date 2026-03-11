use std::net::{IpAddr, Ipv4Addr};

use anyhow::{bail, Context, Result};

pub fn normalize_registry_url(raw: &str, label: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Registry URL cannot be empty");
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{}", trimmed)
    };

    let parsed = reqwest::Url::parse(&candidate)
        .with_context(|| format!("Invalid {} URL: {}", label, raw))?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        bail!(
            "Registry URL must use http or https scheme (got '{}')",
            parsed.scheme()
        );
    }
    if parsed.host_str().is_none() {
        bail!("Registry URL must include a host");
    }

    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

pub fn blocking_client_builder(base_url: &str) -> reqwest::blocking::ClientBuilder {
    let mut builder = reqwest::blocking::Client::builder();
    if should_bypass_proxy(base_url) {
        builder = builder.no_proxy();
    }
    builder
}

fn should_bypass_proxy(base_url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(base_url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    should_bypass_proxy_for_host(host)
}

fn should_bypass_proxy_for_host(host: &str) -> bool {
    let normalized = host.trim().trim_matches('.');
    if normalized.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let Ok(ip) = normalized.parse::<IpAddr>() else {
        return false;
    };

    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || is_shared_cgnat(ip)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_unspecified()
        }
    }
}

fn is_shared_cgnat(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && (64..=127).contains(&second)
}

#[cfg(test)]
mod tests {
    use super::{is_shared_cgnat, normalize_registry_url, should_bypass_proxy_for_host};
    use std::net::Ipv4Addr;

    #[test]
    fn normalize_registry_url_accepts_bare_host_and_port() {
        let url = normalize_registry_url("100.68.86.84:9090", "--registry").expect("normalize");
        assert_eq!(url, "http://100.68.86.84:9090");
    }

    #[test]
    fn normalize_registry_url_trims_trailing_slash() {
        let url =
            normalize_registry_url("http://127.0.0.1:8787/", "--registry").expect("normalize");
        assert_eq!(url, "http://127.0.0.1:8787");
    }

    #[test]
    fn should_bypass_proxy_for_local_hosts() {
        assert!(should_bypass_proxy_for_host("localhost"));
        assert!(should_bypass_proxy_for_host("127.0.0.1"));
        assert!(should_bypass_proxy_for_host("100.68.86.84"));
        assert!(should_bypass_proxy_for_host("192.168.1.20"));
        assert!(!should_bypass_proxy_for_host("api.ato.run"));
    }

    #[test]
    fn shared_cgnat_range_includes_tailscale_ips() {
        assert!(is_shared_cgnat(Ipv4Addr::new(100, 68, 86, 84)));
        assert!(!is_shared_cgnat(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!is_shared_cgnat(Ipv4Addr::new(100, 128, 0, 1)));
    }
}
