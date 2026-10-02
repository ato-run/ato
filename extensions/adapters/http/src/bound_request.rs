//! Private Binding projection for an owned HTTP Port. Capture only templates
//! and response status; raw request/response credentials never become Records.
use crate::{HttpEvent, encode_request};
use ato_adapter_api::AdapterError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;
use zeroize::{Zeroize, Zeroizing};

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_STATUS_LINE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTemplate {
    pub method: Method,
    pub path: String,
    /// Public JSON field to declared Binding input name, never its value.
    pub json_bindings: BTreeMap<String, String>,
}

fn fail(message: &'static str) -> AdapterError {
    AdapterError::Operation(message.into())
}
fn name_valid(name: &str, binding: bool) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with("ATO_")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || (!binding && b".-".contains(&b)))
}
impl RequestTemplate {
    pub fn validate(&self) -> Result<(), AdapterError> {
        if !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.len() > 2048
            || self
                .path
                .bytes()
                .any(|b| !b.is_ascii_graphic() || b"?#\\".contains(&b))
            || self.path.split('/').any(|s| s == "..")
            || self.json_bindings.len() > 32
            || (self.method == Method::Get && !self.json_bindings.is_empty())
            || self
                .json_bindings
                .iter()
                .any(|(field, binding)| !name_valid(field, false) || !name_valid(binding, true))
        {
            return Err(fail("invalid bound HTTP request template"));
        }
        Ok(())
    }
}

/// The HTTP Adapter owns this domain-specific payload; Kernel remains opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    RequestTemplate { template: RequestTemplate },
    ResponseStatus { status: u16 },
}

struct PrivateRequest(HttpEvent);
impl Drop for PrivateRequest {
    fn drop(&mut self) {
        if let HttpEvent::Request { body, .. } = &mut self.0 {
            body.zeroize();
        }
    }
}

fn allowance(
    remaining: &mut impl FnMut() -> Result<Duration, AdapterError>,
) -> Result<Duration, AdapterError> {
    let duration = remaining()?;
    if duration.is_zero() {
        return Err(fail("bound HTTP execution deadline exceeded"));
    }
    Ok(duration)
}

/// One request, with no method-based replay. The trusted caller resolves the
/// logical Port and grants; neither the template nor this function grants access.
/// `remaining` must read the caller's frozen monotonic execution allowance on
/// every invocation, not return a freshly reset timeout.
pub fn invoke<'a>(
    upstream: SocketAddr,
    template: &RequestTemplate,
    mut resolve: impl FnMut(&str) -> Result<&'a str, AdapterError>,
    mut remaining: impl FnMut() -> Result<Duration, AdapterError>,
    mut observe: impl FnMut(Observation) -> Result<(), AdapterError>,
) -> Result<u16, AdapterError> {
    template.validate()?;
    if !upstream.ip().is_loopback() || upstream.port() == 0 {
        return Err(fail("bound HTTP target must be an assigned loopback Port"));
    }
    allowance(&mut remaining)?;
    let mut values = BTreeMap::new();
    for (field, binding) in &template.json_bindings {
        allowance(&mut remaining)?;
        let value = resolve(binding)?;
        if value.is_empty() || value.len() > 16384 || value.contains('\0') {
            return Err(fail("bound HTTP input is unavailable or invalid"));
        }
        values.insert(field.as_str(), value);
    }
    let body = if template.method == Method::Post {
        serde_json::to_vec(&values).map_err(|_| fail("bound HTTP body encoding failed"))?
    } else {
        Vec::new()
    };
    let body = Zeroizing::new(body);
    if body.len() > MAX_BODY_BYTES {
        return Err(fail("bound HTTP body exceeds its declared limit"));
    }
    let request = PrivateRequest(HttpEvent::Request {
        method: match template.method {
            Method::Get => "GET",
            Method::Post => "POST",
        }
        .into(),
        path: template.path.clone(),
        headers: BTreeMap::from([
            ("host".into(), upstream.to_string()),
            ("accept".into(), "application/json".into()),
            ("content-type".into(), "application/json".into()),
            ("connection".into(), "close".into()),
        ]),
        body: body.to_vec(),
    });
    let wire = Zeroizing::new(encode_request(&request.0)?);
    allowance(&mut remaining)?;
    observe(Observation::RequestTemplate {
        template: template.clone(),
    })?;
    let mut stream = TcpStream::connect_timeout(&upstream, allowance(&mut remaining)?)
        .map_err(|_| fail("bound HTTP connection failed"))?;
    let mut offset = 0;
    while offset < wire.len() {
        stream
            .set_write_timeout(Some(allowance(&mut remaining)?))
            .map_err(|_| fail("bound HTTP timeout configuration failed"))?;
        let count = stream
            .write(&wire[offset..])
            .map_err(|_| fail("bound HTTP request delivery uncertain"))?;
        if count == 0 {
            return Err(fail("bound HTTP request delivery uncertain"));
        }
        offset += count;
    }
    // Do not capture response headers/body, which may contain freshly minted
    // tokens. Even an adversarial status reason cannot enter safe evidence.
    let mut line = Zeroizing::new(Vec::new());
    loop {
        stream
            .set_read_timeout(Some(allowance(&mut remaining)?))
            .map_err(|_| fail("bound HTTP timeout configuration failed"))?;
        let mut byte = [0u8; 1];
        if stream
            .read(&mut byte)
            .map_err(|_| fail("bound HTTP response unavailable"))?
            == 0
        {
            return Err(fail("bound HTTP response unavailable"));
        }
        line.push(byte[0]);
        if line.len() > MAX_STATUS_LINE {
            return Err(fail("bound HTTP response status invalid"));
        }
        if byte[0] == b'\n' {
            break;
        }
    }
    let text =
        std::str::from_utf8(&line).map_err(|_| fail("bound HTTP response status invalid"))?;
    let mut parts = text.split_ascii_whitespace();
    if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err(fail("bound HTTP response status invalid"));
    }
    let status = parts
        .next()
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..=599).contains(s))
        .ok_or_else(|| fail("bound HTTP response status invalid"))?;
    observe(Observation::ResponseStatus { status })?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn template() -> RequestTemplate {
        RequestTemplate {
            method: Method::Post,
            path: "/source-declared/owner".into(),
            json_bindings: BTreeMap::from([("password".into(), "OWNER_PASSWORD".into())]),
        }
    }

    #[test]
    fn private_body_uses_existing_wire_encoder_and_response_secrets_are_not_captured() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut input = Vec::new();
            loop {
                let mut byte = [0u8; 1];
                socket.read_exact(&mut byte).unwrap();
                input.push(byte[0]);
                if input.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = String::from_utf8(input).unwrap();
            let length: usize = header
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["password"],
                "private-input-canary"
            );
            socket.write_all(b"HTTP/1.1 201 private-reason-canary\r\nSet-Cookie: private-token-canary\r\nContent-Length: 20\r\n\r\nprivate-token-canary").unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut evidence = vec![];
        let status = invoke(
            target,
            &template(),
            |_| Ok("private-input-canary"),
            || Ok(deadline.saturating_duration_since(Instant::now())),
            |o| {
                evidence.push(o);
                Ok(())
            },
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(status, 201);
        assert_eq!(evidence.len(), 2);
        let json = serde_json::to_string(&evidence).unwrap();
        assert!(!json.contains("canary"));
        assert!(!json.contains("127.0.0.1"));
        assert!(json.contains("OWNER_PASSWORD"));
    }

    #[test]
    fn expired_budget_and_unassigned_targets_do_not_resolve_inputs_or_dispatch() {
        for (target, budget) in [
            ("127.0.0.1:1", Duration::ZERO),
            ("192.0.2.1:1", Duration::from_secs(1)),
        ] {
            assert!(
                invoke(
                    target.parse().unwrap(),
                    &template(),
                    |_| panic!("no input resolution"),
                    || Ok(budget),
                    |_| panic!("no dispatch observation")
                )
                .is_err()
            );
        }
        for path in [
            "//external.example/a",
            "http://external.example",
            "/a\nInjected",
            "/?credential=x",
            "/a/../b",
        ] {
            let mut request = template();
            request.path = path.into();
            assert!(request.validate().is_err());
        }
    }

    #[test]
    fn response_loss_uses_original_allowance_without_automatic_post_retry() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(150));
            drop(socket);
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err());
        });
        let deadline = Instant::now() + Duration::from_millis(30);
        let mut evidence = vec![];
        assert!(
            invoke(
                target,
                &template(),
                |_| Ok("private-value"),
                || Ok(deadline.saturating_duration_since(Instant::now())),
                |o| {
                    evidence.push(o);
                    Ok(())
                }
            )
            .is_err()
        );
        server.join().unwrap();
        assert_eq!(evidence.len(), 1);
    }
}
