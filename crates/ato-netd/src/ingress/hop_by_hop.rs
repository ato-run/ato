//! Dynamic hop-by-hop header scrubbing (RFC 7230 §6.1).
//!
//! A proxy **must not** forward connection-specific headers across the
//! proxy boundary.  The standard set is hard-coded in `STANDARD_HOP_BY_HOP`
//! but RFC 7230 §6.1 also requires that any header name listed in the value
//! of a `Connection` header is treated as hop-by-hop for that message.
//!
//! # WebSocket passthrough
//!
//! Do **not** call `scrub_hop_by_hop` on WebSocket upgrade requests or
//! responses.  The `Connection: Upgrade` and `Upgrade: websocket` headers
//! must reach the upstream.  Use [`scrub_hop_by_hop_ws`] instead, which
//! keeps `Upgrade` and the `Connection: Upgrade` token.

use http::HeaderMap;

/// The standard set of hop-by-hop headers that must always be removed.
static STANDARD_HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Remove all hop-by-hop headers from `headers` (standard list + any
/// header names found inside the `Connection` header value).
///
/// Suitable for regular HTTP/1.1 proxy requests.
/// **Do not call for WebSocket upgrade messages.**
pub fn scrub_hop_by_hop(headers: &mut HeaderMap) {
    let extra = collect_connection_tokens(headers);

    for name in STANDARD_HOP_BY_HOP {
        headers.remove(*name);
    }
    for name in &extra {
        if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(header_name);
        }
    }
}

/// Lightweight hop-by-hop scrub for WebSocket upgrade paths.
///
/// Keeps `Upgrade` and `Connection: Upgrade` so the upstream can
/// negotiate the upgrade.  Removes all other standard hop-by-hop
/// headers and any extra tokens from `Connection`.
pub fn scrub_hop_by_hop_ws(headers: &mut HeaderMap) {
    let extra = collect_connection_tokens(headers);

    // Remove standard hop-by-hop headers EXCEPT Connection + Upgrade.
    for name in STANDARD_HOP_BY_HOP {
        if *name == "connection" || *name == "upgrade" {
            continue;
        }
        headers.remove(*name);
    }
    // Remove dynamic tokens but keep "upgrade".
    for name in &extra {
        if name.eq_ignore_ascii_case("upgrade") {
            continue;
        }
        if let Ok(header_name) = http::header::HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(header_name);
        }
    }
}

fn collect_connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("connection")
        .iter()
        .flat_map(|v| {
            v.to_str()
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                http::HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    #[test]
    fn removes_standard_hop_by_hop() {
        let mut headers = header_map(&[
            ("host", "example.com"),
            ("keep-alive", "timeout=5"),
            ("transfer-encoding", "chunked"),
        ]);
        scrub_hop_by_hop(&mut headers);
        assert!(headers.get("keep-alive").is_none());
        assert!(headers.get("transfer-encoding").is_none());
        assert!(headers.get("host").is_some(), "host should survive scrub");
    }

    #[test]
    fn removes_dynamic_connection_tokens() {
        let mut headers = header_map(&[
            ("connection", "keep-alive, x-custom-header"),
            ("x-custom-header", "value"),
            ("x-unrelated", "stay"),
        ]);
        scrub_hop_by_hop(&mut headers);
        assert!(headers.get("x-custom-header").is_none());
        assert!(headers.get("x-unrelated").is_some());
    }

    #[test]
    fn ws_scrub_preserves_upgrade_and_connection() {
        let mut headers = header_map(&[
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("keep-alive", "timeout=5"),
        ]);
        scrub_hop_by_hop_ws(&mut headers);
        assert!(
            headers.get("upgrade").is_some(),
            "Upgrade must survive ws scrub"
        );
        assert!(
            headers.get("connection").is_some(),
            "Connection must survive ws scrub"
        );
        assert!(
            headers.get("keep-alive").is_none(),
            "keep-alive must be removed"
        );
    }
}
