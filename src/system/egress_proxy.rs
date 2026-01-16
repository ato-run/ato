use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

// v3.0: EgressPolicyRegistry moved to capsule-cli
// This proxy is now a standalone component without policy resolution
// use crate::security::egress_policy::EgressPolicyRegistry;
use base64::Engine as _;

fn host_matches_domain(host: &str, domain: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_lowercase();
    let domain = domain.trim().trim_end_matches('.').to_lowercase();
    if host == domain {
        return true;
    }
    host.ends_with(&format!(".{}", domain))
}

#[derive(Clone)]
pub struct EgressProxy {
    port: u16,
    whitelist: Arc<RwLock<Vec<String>>>,
}

impl EgressProxy {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            whitelist: Arc::new(RwLock::new(vec![])),
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let listener = TcpListener::bind(addr).await?;
        info!("Egress Proxy listening on {}", addr);

        let whitelist = self.whitelist.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((socket, _)) => {
                        let wl = whitelist.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(socket, wl).await {
                                debug!("Proxy connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => error!("Proxy accept error: {}", e),
                }
            }
        });

        Ok(())
    }

    pub fn add_to_whitelist(&self, domain: String) {
        if let Ok(mut wl) = self.whitelist.write() {
            wl.push(domain);
        }
    }
}

async fn handle_connection(
    mut client_socket: TcpStream,
    whitelist: Arc<RwLock<Vec<String>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Simple HTTP/CONNECT parsing
    let mut buf = [0u8; 4096];
    let n = client_socket.peek(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let request_str = String::from_utf8_lossy(&buf[..n]);

    let target_host = extract_target_host(&request_str);
    let _auth = extract_basic_proxy_auth(&request_str); // v3.0: Currently unused

    if let Some(host) = target_host {
        let allowed_by_default = {
            let wl = whitelist.read().unwrap();
            wl.iter().any(|domain| host_matches_domain(&host, domain))
        };

        // v3.0: Policy registry check removed - policies are now resolved by capsule-cli
        // and passed via sandbox_rules.json
        let allowed_by_policy = false;
        /*
        let allowed_by_policy = auth
            .as_ref()
            .and_then(|(u, p)| EgressPolicyRegistry::global().allowlist_for_basic_auth(u, p))
            .map(|allowlist| {
                allowlist
                    .iter()
                    .any(|domain| host_matches_domain(&host, domain))
            })
            .unwrap_or(false);
        */

        if !(allowed_by_default || allowed_by_policy) {
            warn!(
                "[Egress Proxy] Blocked connection to '{}' (not in allowlist)",
                host
            );
            let response = build_block_response(&host);
            client_socket.write_all(&response).await?;
            return Ok(());
        }

        info!("Allowed connection to: {}", host);

        // If CONNECT, we need to establish tunnel
        if request_str.starts_with("CONNECT") {
            // Read the actual request to consume it from the buffer?
            // No, peek didn't consume. We need to read it out.
            let mut read_buf = vec![0u8; n];
            client_socket.read_exact(&mut read_buf).await?; // Consume the CONNECT request header

            // Find the end of the header (\r\n\r\n) if it wasn't fully in the first peek?
            // Simplified: Assuming standard CONNECT request fits in 4k and we read it all.
            // In reality, we should read until double CRLF.

            // Connect to target
            // We need the port.
            let target = request_str
                .split_whitespace()
                .nth(1)
                .ok_or("Invalid CONNECT")?;
            let target_socket = TcpStream::connect(target).await?;

            // Send 200 OK to client
            client_socket
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;

            // Tunnel
            let (mut client_reader, mut client_writer) = client_socket.into_split();
            let (mut target_reader, mut target_writer) = target_socket.into_split();

            let client_to_target = tokio::io::copy(&mut client_reader, &mut target_writer);
            let target_to_client = tokio::io::copy(&mut target_reader, &mut client_writer);

            tokio::try_join!(client_to_target, target_to_client)?;
        } else {
            // Normal HTTP Proxying (Forwarding)
            // We need to parse the full URL or just forward to the Host?
            // Simple approach: Connect to Host:80 and forward everything.
            let host_port = if host.contains(':') {
                host.to_string()
            } else {
                format!("{}:80", host)
            };
            let target_socket = TcpStream::connect(host_port).await?;

            // We didn't consume the buffer yet (peek).
            // But we can't easily "un-peek".
            // We need to read from client_socket and write to target_socket.
            // But 'copy' consumes.

            // Wait, we need to ensure we don't lose the initial bytes.
            // Since we only peeked, the data is still in the socket buffer.
            // So we can just copy!

            let (mut client_reader, mut client_writer) = client_socket.into_split();
            let (mut target_reader, mut target_writer) = target_socket.into_split();

            let client_to_target = tokio::io::copy(&mut client_reader, &mut target_writer);
            let target_to_client = tokio::io::copy(&mut target_reader, &mut client_writer);

            tokio::try_join!(client_to_target, target_to_client)?;
        }
    } else {
        warn!("Could not determine target host, blocking.");
        client_socket
            .write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n")
            .await?;
    }

    Ok(())
}

fn extract_target_host(request_str: &str) -> Option<String> {
    fn normalize_extracted_host(authority: &str) -> Option<String> {
        let authority = authority.trim();
        if authority.is_empty() {
            return None;
        }

        let host = if authority.starts_with('[') {
            // Bracketed IPv6: "[::1]:443" -> "[::1]"
            let end = authority.find(']')?;
            &authority[..=end]
        } else if let Some((host, port)) = authority.rsplit_once(':') {
            // Strip :port only if it looks like a numeric port.
            if !host.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        } else {
            authority
        };

        let host = host.trim().to_lowercase();
        let host = host.trim_end_matches('.');
        if host.is_empty() {
            None
        } else {
            Some(host.to_string())
        }
    }

    // Check for CONNECT (HTTPS)
    if request_str.starts_with("CONNECT") {
        // Parse "CONNECT host:port HTTP/1.1"
        let host_port = request_str.split_whitespace().nth(1)?;
        return normalize_extracted_host(host_port);
    }

    // Assume HTTP, look for Host header
    request_str
        .lines()
        .find(|l| l.to_lowercase().starts_with("host:"))
        .and_then(|l| l.split_once(':').map(|x| x.1))
        .and_then(normalize_extracted_host)
}

fn extract_basic_proxy_auth(request_str: &str) -> Option<(String, String)> {
    let header_value = request_str
        .lines()
        .find(|l| l.to_lowercase().starts_with("proxy-authorization:"))?
        .split_once(':')?
        .1
        .trim();

    let (scheme, b64) = header_value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }

    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn build_block_response(host: &str) -> Vec<u8> {
    let body = format!(
        "{{\"error\":\"egress_blocked\",\"host\":\"{}\",\"reason\":\"domain not in allowlist\"}}",
        host
    );
    format!(
        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_target_host_connect() {
        let req = "CONNECT example.com:443 HTTP/1.1\r\n\r\n";
        assert_eq!(extract_target_host(req), Some("example.com".to_string()));
    }

    #[test]
    fn extract_target_host_http() {
        let req = "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n";
        assert_eq!(extract_target_host(req), Some("example.com".to_string()));
    }

    #[test]
    fn extract_target_host_http_strips_port_and_trailing_dot() {
        let req = "GET http://example.com/ HTTP/1.1\r\nHost: Example.COM.:8080\r\n\r\n";
        assert_eq!(extract_target_host(req), Some("example.com".to_string()));
    }

    #[test]
    fn extract_target_host_connect_ipv6_bracketed() {
        let req = "CONNECT [::1]:443 HTTP/1.1\r\n\r\n";
        assert_eq!(extract_target_host(req), Some("[::1]".to_string()));
    }

    #[test]
    fn extract_target_host_http_ipv6_bracketed() {
        let req = "GET http://[::1]/ HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n";
        assert_eq!(extract_target_host(req), Some("[::1]".to_string()));
    }

    #[test]
    fn extract_basic_proxy_auth_parses_basic() {
        // aladdin:opensesame -> YWxhZGRpbjpvcGVuc2VzYW1l
        let req = "CONNECT example.com:443 HTTP/1.1\r\nProxy-Authorization: Basic YWxhZGRpbjpvcGVuc2VzYW1l\r\n\r\n";
        assert_eq!(
            extract_basic_proxy_auth(req),
            Some(("aladdin".to_string(), "opensesame".to_string()))
        );
    }

    #[test]
    fn default_whitelist_is_empty() {
        let proxy = EgressProxy::new(8080);
        let wl = proxy.whitelist.read().expect("whitelist lock poisoned");
        assert!(
            wl.is_empty(),
            "default whitelist must block external hosts by default"
        );
    }

    #[test]
    fn block_response_includes_host_and_json() {
        let bytes = build_block_response("example.com");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 403"));
        assert!(s.contains("Content-Type: application/json"));
        assert!(s.contains("example.com"));
        assert!(s.contains("egress_blocked"));
    }

    #[test]
    fn host_matches_domain_requires_label_boundary() {
        assert!(host_matches_domain("github.com", "github.com"));
        assert!(host_matches_domain("api.github.com", "github.com"));
        assert!(!host_matches_domain("evilgithub.com", "github.com"));
        assert!(!host_matches_domain("github.com.evil", "github.com"));
    }
}
