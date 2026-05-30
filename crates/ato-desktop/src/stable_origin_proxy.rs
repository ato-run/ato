#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use url::Url;

use crate::state::GuestRoute;

const MAX_DNS_LABEL_LEN: usize = 63;
const MAX_SLUG_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StableOriginRoute {
    pub logical_capsule_key: String,
    pub stable_host_label: String,
    pub upstream: Url,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum StableOriginError {
    #[error("invalid upstream scheme: {0}")]
    InvalidUpstreamScheme(String),
    #[error("invalid stable host label: {0}")]
    InvalidHostLabel(String),
    #[error("unknown stable host label: {0}")]
    UnknownHostLabel(String),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StableOriginRouteTable {
    inner: Arc<Mutex<StableOriginRouteTableInner>>,
}

#[derive(Debug, Default)]
struct StableOriginRouteTableInner {
    by_host: HashMap<String, StableOriginRoute>,
    host_by_key: HashMap<String, String>,
}

impl StableOriginRouteTable {
    pub(crate) fn register_or_swap(
        &self,
        logical_capsule_key: impl Into<String>,
        upstream: Url,
    ) -> Result<StableOriginRoute, StableOriginError> {
        ensure_proxyable_upstream_scheme(&upstream)?;
        let logical_capsule_key = logical_capsule_key.into();
        let stable_host_label = stable_host_label_for_key(&logical_capsule_key);

        let mut table = self
            .inner
            .lock()
            .expect("stable origin route table poisoned");
        let host = table
            .host_by_key
            .entry(logical_capsule_key.clone())
            .or_insert_with(|| stable_host_label.clone())
            .clone();

        let route = StableOriginRoute {
            logical_capsule_key: logical_capsule_key.clone(),
            stable_host_label: host.clone(),
            upstream,
        };
        table.by_host.insert(host, route.clone());
        Ok(route)
    }

    pub(crate) fn swap_upstream_for_host(
        &self,
        stable_host_label: &str,
        upstream: Url,
    ) -> Result<StableOriginRoute, StableOriginError> {
        ensure_proxyable_upstream_scheme(&upstream)?;
        let stable_host_label = normalize_host_label(stable_host_label)?;
        let mut table = self
            .inner
            .lock()
            .expect("stable origin route table poisoned");
        let route = table
            .by_host
            .get_mut(&stable_host_label)
            .ok_or_else(|| StableOriginError::UnknownHostLabel(stable_host_label.clone()))?;
        route.upstream = upstream;
        Ok(route.clone())
    }

    pub(crate) fn validate_and_resolve_host(
        &self,
        request_host: &str,
    ) -> Result<StableOriginRoute, StableOriginError> {
        let stable_host_label = normalize_host_label(request_host)?;
        let table = self
            .inner
            .lock()
            .expect("stable origin route table poisoned");
        table
            .by_host
            .get(&stable_host_label)
            .cloned()
            .ok_or(StableOriginError::UnknownHostLabel(stable_host_label))
    }
}

pub(crate) fn logical_capsule_key_for_stable_origin(route: &GuestRoute) -> Option<String> {
    match route {
        GuestRoute::CapsuleHandle { handle, .. } => Some(format!("handle:{handle}")),
        GuestRoute::Capsule { session, .. } => Some(format!("session:{session}")),
        GuestRoute::CapsuleUrl { .. }
        | GuestRoute::ExternalUrl(_)
        | GuestRoute::Terminal { .. } => None,
    }
}

pub(crate) fn stable_host_label_for_key(logical_capsule_key: &str) -> String {
    let slug = host_slug(logical_capsule_key);
    let hash = fnv1a64(logical_capsule_key.as_bytes());
    let label = format!("{slug}-{hash:016x}");
    label[..label.len().min(MAX_DNS_LABEL_LEN)].to_string()
}

fn host_slug(logical_capsule_key: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in logical_capsule_key.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' {
            if !prev_dash {
                slug.push('-');
                prev_dash = true;
            }
        } else {
            slug.push(normalized);
            prev_dash = false;
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "capsule".to_string()
    } else {
        slug.to_string()
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn ensure_proxyable_upstream_scheme(upstream: &Url) -> Result<(), StableOriginError> {
    match upstream.scheme() {
        "http" | "https" | "ws" | "wss" => Ok(()),
        other => Err(StableOriginError::InvalidUpstreamScheme(other.to_string())),
    }
}

fn normalize_host_label(raw: &str) -> Result<String, StableOriginError> {
    let host = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if !is_valid_dns_label(&host) {
        return Err(StableOriginError::InvalidHostLabel(raw.to_string()));
    }
    Ok(host)
}

fn is_valid_dns_label(host: &str) -> bool {
    if host.is_empty() || host.len() > MAX_DNS_LABEL_LEN {
        return false;
    }
    let bytes = host.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_label_derivation_is_deterministic_and_dns_safe() {
        let key = "handle:capsule://ato.run/org/demo@1.2.3";
        let a = stable_host_label_for_key(key);
        let b = stable_host_label_for_key(key);
        assert_eq!(a, b);
        assert!(is_valid_dns_label(&a), "label must be DNS-safe: {a}");
    }

    #[test]
    fn validate_host_rejects_invalid_and_unknown_labels() {
        let table = StableOriginRouteTable::default();
        let route = table
            .register_or_swap(
                "handle:capsule://ato.run/org/demo@1.2.3",
                Url::parse("http://127.0.0.1:3000").expect("url"),
            )
            .expect("register");
        assert_eq!(
            table.validate_and_resolve_host("bad host"),
            Err(StableOriginError::InvalidHostLabel("bad host".to_string()))
        );
        assert_eq!(
            table.validate_and_resolve_host("missing-host"),
            Err(StableOriginError::UnknownHostLabel(
                "missing-host".to_string()
            ))
        );
        let resolved = table
            .validate_and_resolve_host(&route.stable_host_label)
            .expect("resolve");
        assert_eq!(resolved.logical_capsule_key, route.logical_capsule_key);
    }

    #[test]
    fn route_swap_updates_upstream_without_changing_stable_host() {
        let table = StableOriginRouteTable::default();
        let initial = table
            .register_or_swap(
                "handle:capsule://ato.run/org/demo@1.2.3",
                Url::parse("http://127.0.0.1:3000").expect("url"),
            )
            .expect("register");
        let swapped = table
            .register_or_swap(
                "handle:capsule://ato.run/org/demo@1.2.3",
                Url::parse("http://127.0.0.1:4000").expect("url"),
            )
            .expect("swap");
        assert_eq!(initial.stable_host_label, swapped.stable_host_label);
        assert_eq!(swapped.upstream.as_str(), "http://127.0.0.1:4000/");
    }

    #[test]
    fn route_swap_by_host_supports_websocket_upstreams() {
        let table = StableOriginRouteTable::default();
        let initial = table
            .register_or_swap(
                "handle:capsule://ato.run/org/demo@1.2.3",
                Url::parse("http://127.0.0.1:3000").expect("url"),
            )
            .expect("register");
        let swapped = table
            .swap_upstream_for_host(
                &initial.stable_host_label,
                Url::parse("ws://127.0.0.1:9001/socket").expect("url"),
            )
            .expect("swap by host");
        assert_eq!(
            swapped.upstream.as_str(),
            "ws://127.0.0.1:9001/socket",
            "ws upstream should be preserved for pass-through"
        );
    }

    #[test]
    fn stable_origin_scope_excludes_external_routes() {
        let external = GuestRoute::ExternalUrl(Url::parse("https://example.com").expect("url"));
        let capsule_url = GuestRoute::CapsuleUrl {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            url: Url::parse("http://127.0.0.1:3000").expect("url"),
        };
        let handle = GuestRoute::CapsuleHandle {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
        };
        assert_eq!(logical_capsule_key_for_stable_origin(&external), None);
        assert_eq!(logical_capsule_key_for_stable_origin(&capsule_url), None);
        assert_eq!(
            logical_capsule_key_for_stable_origin(&handle),
            Some("handle:capsule://org/demo@1.0.0".to_string())
        );
    }
}
