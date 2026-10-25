//! Private Binding projection for an owned HTTP Port. Only declaration metadata,
//! status and bounded equality booleans become observations. A Session contains
//! one attempt's private captures and host-only cookies for one assigned Port.
use crate::{HttpEvent, encode_request};
use ato_adapter_api::AdapterError;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, SystemTime},
};
use zeroize::{Zeroize, Zeroizing};

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_STATUS_LINE: usize = 512;
const MAX_HEADER_BYTES: usize = 16 * 1024;
fn is_false(value: &bool) -> bool {
    !*value
}
fn fail(message: &'static str) -> AdapterError {
    AdapterError::Operation(message.into())
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarKind {
    String,
    Boolean,
    Integer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderEncoding {
    Direct,
    Bearer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderBinding {
    pub binding: String,
    pub encoding: HeaderEncoding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectKeySelector {
    pub pointer: String,
    pub where_pointer: String,
    pub binding: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseBinding {
    pub binding: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub json_pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_text_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_object_key: Option<ObjectKeySelector>,
    pub scalar: ScalarKind,
    pub max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseCheck {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub json_pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_name: Option<String>,
    pub binding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalar: Option<ScalarKind>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTemplate {
    pub method: Method,
    pub path: String,
    /// Public JSON field to declared Binding input name, never its value.
    pub json_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub header_bindings: BTreeMap<String, HeaderBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub path_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cookie_bindings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub json_types: BTreeMap<String, ScalarKind>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub cookies: bool,
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
    pub fn binding_names(&self) -> impl Iterator<Item = &String> {
        self.json_bindings
            .values()
            .chain(self.path_bindings.values())
            .chain(self.cookie_bindings.values())
            .chain(self.header_bindings.values().map(|h| &h.binding))
    }
    pub fn validate(&self) -> Result<(), AdapterError> {
        if !self.path.starts_with('/')
            || self.path.starts_with("//")
            || self.path.len() > 2048
            || self
                .path
                .bytes()
                .any(|b| !b.is_ascii_graphic() || b"?#\\".contains(&b))
            || self.path.split('/').any(|s| s == "..")
            || (!self.path_bindings.is_empty() && self.path.bytes().any(|b| b == b'%'))
            || self.json_bindings.len() > 32
            || (self.method == Method::Get && !self.json_bindings.is_empty())
            || self
                .json_bindings
                .iter()
                .any(|(field, binding)| !name_valid(field, false) || !name_valid(binding, true))
        {
            return Err(fail("invalid bound HTTP request template"));
        }
        if self.header_bindings.len() > 8
            || self.path_bindings.len() > 8
            || self.cookie_bindings.len() > 8
            || (!self.cookies && !self.cookie_bindings.is_empty())
            || self
                .cookie_bindings
                .iter()
                .any(|(cookie, binding)| !cookie_name_valid(cookie) || !name_valid(binding, true))
            || self
                .header_bindings
                .iter()
                .any(|(header, value)| !header_valid(header) || !name_valid(&value.binding, true))
            || self.path_bindings.iter().any(|(slot, binding)| {
                !name_valid(slot, true)
                    || !name_valid(binding, true)
                    || !self
                        .path
                        .split('/')
                        .any(|segment| segment == format!("{{{slot}}}"))
            })
            || self
                .json_types
                .keys()
                .any(|field| !self.json_bindings.contains_key(field))
            || (!self.path_bindings.is_empty()
                && self.path.split('/').any(|segment| {
                    if segment.contains(['{', '}']) {
                        segment
                            .strip_prefix('{')
                            .and_then(|s| s.strip_suffix('}'))
                            .is_none_or(|slot| !self.path_bindings.contains_key(slot))
                    } else {
                        false
                    }
                }))
        {
            return Err(fail("invalid private HTTP binding template"));
        }
        Ok(())
    }
}

fn cookie_name_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

fn header_valid(header: &str) -> bool {
    !header.is_empty()
        && header.len() <= 64
        && header
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !matches!(
            header,
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "cookie"
                | "set-cookie"
                | "content-type"
                | "accept"
                | "expect"
                | "upgrade"
                | "te"
                | "trailer"
                | "proxy-authorization"
                | "proxy-connection"
                | "forwarded"
        )
        && !header.starts_with("x-forwarded-")
}
fn pointer_valid(pointer: &str) -> bool {
    pointer.starts_with('/')
        && pointer.len() <= 256
        && pointer
            .split('/')
            .skip(1)
            .all(|part| name_valid(part, false))
}

fn check_valid(check: &ResponseCheck) -> bool {
    name_valid(&check.binding, true)
        && ((!check.json_pointer.is_empty()) as u8 + check.header_name.is_some() as u8 == 1)
        && (check.json_pointer.is_empty() || pointer_valid(&check.json_pointer))
        && check.header_name.as_ref().is_none_or(|header| {
            !header.is_empty()
                && header.len() <= 64
                && header
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && check
                    .scalar
                    .is_none_or(|scalar| scalar == ScalarKind::String)
        })
}

fn capture_valid(capture: &ResponseBinding) -> bool {
    let selectors = usize::from(!capture.json_pointer.is_empty())
        + usize::from(capture.html_text_id.is_some())
        + usize::from(capture.json_object_key.is_some());
    selectors == 1
        && name_valid(&capture.binding, true)
        && (1..=4096).contains(&capture.max_bytes)
        && (capture.json_pointer.is_empty() || pointer_valid(&capture.json_pointer))
        && capture
            .html_text_id
            .as_ref()
            .is_none_or(|id| name_valid(id, false) && capture.scalar == ScalarKind::String)
        && capture.json_object_key.as_ref().is_none_or(|selector| {
            (selector.pointer.is_empty() || pointer_valid(&selector.pointer))
                && pointer_valid(&selector.where_pointer)
                && name_valid(&selector.binding, true)
                && capture.scalar == ScalarKind::String
        })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Observation {
    RequestTemplate { template: RequestTemplate },
    ResponseStatus { status: u16 },
    ResponseCheck { check: ResponseCheck, matched: bool },
}

struct PrivateRequest(HttpEvent);
impl Drop for PrivateRequest {
    fn drop(&mut self) {
        if let HttpEvent::Request {
            path,
            headers,
            body,
            ..
        } = &mut self.0
        {
            path.zeroize();
            for value in headers.values_mut() {
                value.zeroize();
            }
            body.zeroize();
        }
    }
}
struct PrivateScalar {
    kind: ScalarKind,
    text: Zeroizing<String>,
}
impl PrivateScalar {
    fn from_input(text: &str, kind: ScalarKind, cap: usize) -> Result<Self, AdapterError> {
        if text.is_empty() || text.len() > cap || text.contains('\0') {
            return Err(fail("bound HTTP input is unavailable or invalid"));
        }
        if kind != ScalarKind::String {
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(|_| fail("bound HTTP input scalar invalid"))?;
            match kind {
                ScalarKind::Boolean if value.is_boolean() => {}
                ScalarKind::Integer if value.as_i64().is_some() => {}
                _ => return Err(fail("bound HTTP input scalar invalid")),
            }
        }
        Ok(Self {
            kind,
            text: Zeroizing::new(text.to_owned()),
        })
    }
    fn from_json(
        value: &serde_json::Value,
        kind: ScalarKind,
        cap: usize,
    ) -> Result<Self, AdapterError> {
        let text = match kind {
            ScalarKind::String => value.as_str().map(str::to_owned),
            ScalarKind::Boolean => value.as_bool().map(|v| v.to_string()),
            ScalarKind::Integer => value.as_i64().map(|v| v.to_string()),
        }
        .ok_or_else(|| fail("bound HTTP response scalar invalid"))?;
        let text = Zeroizing::new(text);
        Self::from_input(&text, kind, cap)
    }
    fn json(&self) -> serde_json::Value {
        if self.kind == ScalarKind::String {
            serde_json::Value::String(self.text.to_string())
        } else {
            serde_json::from_str(&self.text).expect("validated private scalar")
        }
    }
}
impl fmt::Debug for PrivateScalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateScalar([REDACTED])")
    }
}
struct PrivateJson(serde_json::Value);
fn scrub_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(scrub_json),
        serde_json::Value::Object(map) => {
            for (mut key, mut value) in std::mem::take(map) {
                key.zeroize();
                scrub_json(&mut value);
            }
        }
        _ => {}
    }
}
impl Drop for PrivateJson {
    fn drop(&mut self) {
        scrub_json(&mut self.0);
    }
}
struct Cookie {
    name: Zeroizing<String>,
    value: Zeroizing<String>,
    path: Zeroizing<String>,
    expires: Option<SystemTime>,
}

/// Non-serializable private state. Construct a fresh Session for each attempt
/// and logical Port; neither captures nor cookies survive an attempt boundary.
pub struct Session {
    upstream: SocketAddr,
    captures: BTreeMap<String, PrivateScalar>,
    cookies: Vec<Cookie>,
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BoundHttpSession([REDACTED])")
    }
}
impl Session {
    pub fn new(upstream: SocketAddr) -> Result<Self, AdapterError> {
        if !upstream.ip().is_loopback() || upstream.port() == 0 {
            return Err(fail("bound HTTP target must be an assigned loopback Port"));
        }
        Ok(Self {
            upstream,
            captures: BTreeMap::new(),
            cookies: vec![],
        })
    }
    pub fn has_binding(&self, name: &str) -> bool {
        self.captures.contains_key(name)
    }
    fn value<'a>(
        &self,
        name: &str,
        kind: Option<ScalarKind>,
        resolve: &mut impl FnMut(&str) -> Result<&'a str, AdapterError>,
    ) -> Result<PrivateScalar, AdapterError> {
        if let Some(value) = self.captures.get(name) {
            if kind.is_some_and(|kind| kind != value.kind) {
                return Err(fail("bound HTTP captured scalar kind mismatch"));
            }
            PrivateScalar::from_input(&value.text, value.kind, 16384)
        } else {
            PrivateScalar::from_input(resolve(name)?, kind.unwrap_or(ScalarKind::String), 16384)
        }
    }
    pub fn invoke<'a>(
        &mut self,
        template: &RequestTemplate,
        captures: &[ResponseBinding],
        checks: &[ResponseCheck],
        mut resolve: impl FnMut(&str) -> Result<&'a str, AdapterError>,
        mut remaining: impl FnMut() -> Result<Duration, AdapterError>,
        mut observe: impl FnMut(Observation) -> Result<(), AdapterError>,
    ) -> Result<u16, AdapterError> {
        template.validate()?;
        let has_html = captures
            .iter()
            .any(|capture| capture.html_text_id.is_some());
        if (has_html
            && (captures
                .iter()
                .any(|capture| capture.html_text_id.is_none())
                || checks.iter().any(|check| check.header_name.is_none())))
            || captures.len() > 8
            || checks.len() > 8
            || captures
                .iter()
                .any(|c| !capture_valid(c) || self.captures.contains_key(&c.binding))
            || checks.iter().any(|c| !check_valid(c))
            || self.captures.len() + captures.len() > 16
        {
            return Err(fail("bound HTTP response declaration invalid"));
        }
        let mut captured_names = std::collections::BTreeSet::new();
        if captures.iter().any(|c| !captured_names.insert(&c.binding)) {
            return Err(fail("bound HTTP response declaration invalid"));
        }
        allowance(&mut remaining)?;
        let mut path = Zeroizing::new(template.path.clone());
        for (slot, binding) in &template.path_bindings {
            let value = self.value(binding, None, &mut resolve)?;
            if value.text.len() > 128
                || matches!(value.text.as_str(), "." | "..")
                || !value
                    .text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err(fail("bound HTTP path segment invalid"));
            }
            *path = path
                .split('/')
                .map(|segment| {
                    if segment == format!("{{{slot}}}") {
                        value.text.as_str()
                    } else {
                        segment
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
        }
        if path.len() > 2048 {
            return Err(fail("bound HTTP rendered path exceeds limit"));
        }
        let mut body_json = PrivateJson(serde_json::json!({}));
        for (field, binding) in &template.json_bindings {
            let value = self.value(
                binding,
                template.json_types.get(field).copied(),
                &mut resolve,
            )?;
            body_json.0[field] = value.json();
        }
        let body = Zeroizing::new(if template.method == Method::Get {
            vec![]
        } else {
            serde_json::to_vec(&body_json.0).map_err(|_| fail("bound HTTP body encoding failed"))?
        });
        if body.len() > MAX_BODY_BYTES {
            return Err(fail("bound HTTP body exceeds its declared limit"));
        }
        let mut headers = BTreeMap::from([
            ("host".into(), self.upstream.to_string()),
            (
                "accept".into(),
                if has_html {
                    "text/html"
                } else {
                    "application/json"
                }
                .into(),
            ),
            ("content-type".into(), "application/json".into()),
            ("connection".into(), "close".into()),
        ]);
        for (header, binding) in &template.header_bindings {
            let value = self.value(&binding.binding, None, &mut resolve)?;
            if value.text.len() > 4096
                || !value.text.bytes().all(|b| {
                    b.is_ascii_graphic()
                        || (binding.encoding == HeaderEncoding::Direct && b == b' ')
                })
            {
                return Err(fail("bound HTTP private header invalid"));
            }
            headers.insert(
                header.clone(),
                match binding.encoding {
                    HeaderEncoding::Direct => value.text.to_string(),
                    HeaderEncoding::Bearer => format!("Bearer {}", value.text.as_str()),
                },
            );
        }
        for (name, binding) in &template.cookie_bindings {
            let value = self.value(binding, None, &mut resolve)?;
            if value.text.len() > 2048
                || !value
                    .text
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && !b"\";,\\".contains(&b))
            {
                return Err(fail("bound HTTP private cookie invalid"));
            }
            self.cookies.retain(|cookie| cookie.name.as_str() != name);
            self.cookies.push(Cookie {
                name: Zeroizing::new(name.clone()),
                value: value.text,
                path: Zeroizing::new("/".into()),
                expires: None,
            });
        }
        if self.cookies.len() > 16
            || self
                .cookies
                .iter()
                .map(|c| c.name.len() + c.value.len() + c.path.len())
                .sum::<usize>()
                > 8192
        {
            self.cookies.clear();
            return Err(fail("bound HTTP cookie jar bound exceeded"));
        }
        if template.cookies {
            self.cookies
                .retain(|c| c.expires.is_none_or(|expiry| expiry > SystemTime::now()));
            let cookies = Zeroizing::new(
                self.cookies
                    .iter()
                    .filter(|c| {
                        path.as_str() == c.path.as_str()
                            || (path.starts_with(c.path.as_str())
                                && (c.path.ends_with('/')
                                    || path.as_bytes().get(c.path.len()) == Some(&b'/')))
                    })
                    .map(|c| format!("{}={}", c.name.as_str(), c.value.as_str()))
                    .collect::<Vec<_>>()
                    .join("; "),
            );
            if !cookies.is_empty() {
                headers.insert("cookie".into(), cookies.to_string());
            }
        }
        let mut predicates = Vec::new();
        for capture in captures {
            predicates.push(
                capture
                    .json_object_key
                    .as_ref()
                    .map(|selector| self.value(&selector.binding, None, &mut resolve))
                    .transpose()?,
            );
        }
        let mut expected = Vec::new();
        for check in checks {
            let value = self.value(&check.binding, check.scalar, &mut resolve)?;
            if check.header_name.is_some() && value.kind != ScalarKind::String {
                return Err(fail("bound HTTP response header equality scalar invalid"));
            }
            expected.push(value);
        }
        let request = PrivateRequest(HttpEvent::Request {
            method: match template.method {
                Method::Get => "GET",
                Method::Post => "POST",
                Method::Put => "PUT",
                Method::Patch => "PATCH",
                Method::Delete => "DELETE",
            }
            .into(),
            path: path.to_string(),
            headers,
            body: body.to_vec(),
        });
        let wire = Zeroizing::new(encode_request(&request.0)?);
        allowance(&mut remaining)?;
        observe(Observation::RequestTemplate {
            template: template.clone(),
        })?;
        let mut stream = TcpStream::connect_timeout(&self.upstream, allowance(&mut remaining)?)
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
        let line = read_line(&mut stream, &mut remaining, MAX_STATUS_LINE)?;
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
        if !template.cookies && captures.is_empty() && checks.is_empty() {
            return Ok(status);
        }
        let mut cookie_lines = Vec::new();
        let mut private_headers = BTreeMap::new();
        let mut length = None;
        let mut chunked = false;
        let mut content_type = None;
        let mut read_bytes = 0;
        loop {
            let line = read_line(&mut stream, &mut remaining, MAX_HEADER_BYTES)?;
            read_bytes += line.len();
            if read_bytes > MAX_HEADER_BYTES {
                return Err(fail("bound HTTP response header bound exceeded"));
            }
            if line.as_slice() == b"\r\n" {
                break;
            }
            let text = std::str::from_utf8(&line)
                .map_err(|_| fail("bound HTTP response header invalid"))?;
            let (name, value) = text
                .trim_end_matches(['\r', '\n'])
                .split_once(':')
                .ok_or_else(|| fail("bound HTTP response header invalid"))?;
            let value = value.trim();
            let name = name.to_ascii_lowercase();
            if checks
                .iter()
                .any(|check| check.header_name.as_deref() == Some(name.as_str()))
                && private_headers
                    .insert(name.clone(), Zeroizing::new(value.to_owned()))
                    .is_some()
            {
                return Err(fail("bound HTTP response header equality ambiguous"));
            }
            match name.as_str() {
                "content-length" => {
                    if length.is_some() {
                        return Err(fail("bound HTTP response framing ambiguous"));
                    }
                    length = Some(
                        value
                            .parse::<usize>()
                            .map_err(|_| fail("bound HTTP response framing invalid"))?,
                    );
                }
                "transfer-encoding" => {
                    if chunked || !value.eq_ignore_ascii_case("chunked") {
                        return Err(fail("bound HTTP response framing invalid"));
                    }
                    chunked = true;
                }
                "content-type" => {
                    if content_type.is_some() {
                        return Err(fail("bound HTTP response type ambiguous"));
                    }
                    content_type = value
                        .split(';')
                        .next()
                        .map(|v| v.trim().to_ascii_lowercase());
                }
                "set-cookie" if template.cookies => {
                    cookie_lines.push(Zeroizing::new(value.to_owned()));
                }
                _ => {}
            }
        }
        if chunked && length.is_some() || length.is_some_and(|n| n > MAX_BODY_BYTES) {
            return Err(fail("bound HTTP response framing ambiguous or oversized"));
        }
        for (check, expected) in checks
            .iter()
            .zip(&expected)
            .filter(|(check, _)| check.header_name.is_some())
        {
            let matched = private_headers
                .get(
                    check
                        .header_name
                        .as_ref()
                        .expect("validated header selector"),
                )
                .is_some_and(|value| value.as_str() == expected.text.as_str());
            observe(Observation::ResponseCheck {
                check: check.clone(),
                matched,
            })?;
            if !matched {
                return Err(fail("bound HTTP response header equality rejected"));
            }
        }
        if captures.is_empty() && checks.iter().all(|check| check.header_name.is_some()) {
            self.store_cookies(&cookie_lines, &path)?;
            return Ok(status);
        }
        let body = read_body(&mut stream, &mut remaining, length, chunked)?;
        let mut pending = Vec::new();
        if has_html {
            if content_type.as_deref() != Some("text/html") {
                return Err(fail("bound HTTP response HTML type unavailable"));
            }
            let text = std::str::from_utf8(&body)
                .map_err(|_| fail("bound HTTP response HTML encoding invalid"))?;
            let document = scraper::Html::parse_document(text);
            if !document.errors.is_empty() {
                return Err(fail("bound HTTP response HTML invalid"));
            }
            for capture in captures {
                let id = capture
                    .html_text_id
                    .as_ref()
                    .expect("validated HTML selector");
                let mut matches = document
                    .tree
                    .nodes()
                    .filter_map(scraper::ElementRef::wrap)
                    .filter(|element| element.value().attr("id") == Some(id.as_str()));
                let element = matches
                    .next()
                    .ok_or_else(|| fail("bound HTTP response HTML text unavailable"))?;
                if matches.next().is_some()
                    || element.child_elements().next().is_some()
                    || matches!(
                        element.value().name(),
                        "script" | "style" | "template" | "noscript"
                    )
                {
                    return Err(fail(
                        "bound HTTP response HTML text ambiguous or unsupported",
                    ));
                }
                let value = Zeroizing::new(element.text().collect::<String>());
                pending.push((
                    capture.binding.clone(),
                    PrivateScalar::from_input(value.trim(), ScalarKind::String, capture.max_bytes)?,
                ));
            }
        } else {
            if content_type.as_deref() != Some("application/json") {
                return Err(fail("bound HTTP response JSON type unavailable"));
            }
            let StrictJson(parsed) = serde_json::from_slice(&body)
                .map_err(|_| fail("bound HTTP response JSON invalid"))?;
            let json = PrivateJson(parsed);
            for (capture, predicate) in captures.iter().zip(predicates) {
                let value = if let Some(selector) = &capture.json_object_key {
                    let object = json
                        .0
                        .pointer(&selector.pointer)
                        .and_then(serde_json::Value::as_object)
                        .ok_or_else(|| fail("bound HTTP response object unavailable"))?;
                    if object.len() > 128 {
                        return Err(fail("bound HTTP response object bound exceeded"));
                    }
                    let expected = PrivateJson(
                        predicate
                            .expect("validated object selector predicate")
                            .json(),
                    );
                    let mut matches = object.iter().filter(|(_, value)| {
                        value
                            .pointer(&selector.where_pointer)
                            .is_some_and(|value| value == &expected.0)
                    });
                    let (key, _) = matches
                        .next()
                        .ok_or_else(|| fail("bound HTTP response object key unavailable"))?;
                    if matches.next().is_some()
                        || matches!(key.as_str(), "." | "..")
                        || !key
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    {
                        return Err(fail("bound HTTP response object key ambiguous or invalid"));
                    }
                    PrivateScalar::from_input(key, ScalarKind::String, capture.max_bytes.min(128))?
                } else {
                    let value = json
                        .0
                        .pointer(&capture.json_pointer)
                        .ok_or_else(|| fail("bound HTTP response scalar unavailable"))?;
                    PrivateScalar::from_json(value, capture.scalar, capture.max_bytes)?
                };
                pending.push((capture.binding.clone(), value));
            }
            for (check, expected) in checks
                .iter()
                .zip(expected)
                .filter(|(check, _)| check.header_name.is_none())
            {
                let expected = PrivateJson(expected.json());
                let matched = json
                    .0
                    .pointer(&check.json_pointer)
                    .is_some_and(|v| v == &expected.0);
                observe(Observation::ResponseCheck {
                    check: check.clone(),
                    matched,
                })?;
                if !matched {
                    return Err(fail("bound HTTP response equality rejected"));
                }
            }
        }
        self.store_cookies(&cookie_lines, &path)?;
        for (name, value) in pending {
            self.captures.insert(name, value);
        }
        Ok(status)
    }

    fn store_cookies(
        &mut self,
        lines: &[Zeroizing<String>],
        request_path: &str,
    ) -> Result<(), AdapterError> {
        let now = SystemTime::now();
        let mut pending = Vec::new();
        for line in lines {
            let mut parts = line.split(';');
            let (name, value) = parts
                .next()
                .and_then(|p| p.split_once('='))
                .ok_or_else(|| fail("bound HTTP cookie invalid"))?;
            if name.is_empty()
                || name.len() > 128
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || value.len() > 2048
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_graphic() && !b"\";,\\".contains(&b))
            {
                return Err(fail("bound HTTP cookie invalid"));
            }
            let mut path = Zeroizing::new(
                request_path
                    .rsplit_once('/')
                    .map(|(p, _)| if p.is_empty() { "/" } else { p })
                    .unwrap_or("/")
                    .to_owned(),
            );
            let mut secure = false;
            let mut expires = None;
            let mut max_age = None;
            let mut seen = std::collections::BTreeSet::new();
            for part in parts {
                let (attribute, attr_value) =
                    part.trim().split_once('=').unwrap_or((part.trim(), ""));
                let attribute = attribute.to_ascii_lowercase();
                if !seen.insert(attribute.clone()) {
                    return Err(fail("bound HTTP cookie attributes ambiguous"));
                }
                match attribute.as_str() {
                    "domain" => return Err(fail("bound HTTP cookie domain unsupported")),
                    "path" => {
                        RequestTemplate {
                            path: attr_value.into(),
                            ..Default::default()
                        }
                        .validate()?;
                        *path = attr_value.into();
                    }
                    "secure" if attr_value.is_empty() => secure = true,
                    "max-age" => {
                        max_age = Some(
                            attr_value
                                .parse::<i64>()
                                .map_err(|_| fail("bound HTTP cookie expiry invalid"))?,
                        )
                    }
                    "expires" => {
                        expires = Some(
                            httpdate::parse_http_date(attr_value)
                                .map_err(|_| fail("bound HTTP cookie expiry invalid"))?,
                        )
                    }
                    "httponly" if attr_value.is_empty() => {}
                    "samesite"
                        if ["lax", "strict", "none"]
                            .iter()
                            .any(|v| attr_value.eq_ignore_ascii_case(v)) => {}
                    _ => return Err(fail("bound HTTP cookie attribute unsupported")),
                }
            }
            if let Some(age) = max_age {
                expires = Some(if age <= 0 {
                    now
                } else {
                    now.checked_add(Duration::from_secs(age as u64))
                        .ok_or_else(|| fail("bound HTTP cookie expiry invalid"))?
                });
            }
            let discard = secure || expires.is_some_and(|expiry| expiry <= now);
            pending.push((
                Cookie {
                    name: Zeroizing::new(name.to_owned()),
                    value: Zeroizing::new(value.to_owned()),
                    path,
                    expires,
                },
                discard,
            ));
        }
        for (cookie, discard) in pending {
            self.cookies.retain(|stored| {
                stored.name.as_str() != cookie.name.as_str()
                    || stored.path.as_str() != cookie.path.as_str()
            });
            if !discard {
                self.cookies.push(cookie);
            }
        }
        self.cookies
            .retain(|c| c.expires.is_none_or(|expiry| expiry > now));
        if self.cookies.len() > 16
            || self
                .cookies
                .iter()
                .map(|c| c.name.len() + c.value.len() + c.path.len())
                .sum::<usize>()
                > 8192
        {
            self.cookies.clear();
            return Err(fail("bound HTTP cookie jar bound exceeded"));
        }
        Ok(())
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
fn read_line(
    stream: &mut TcpStream,
    remaining: &mut impl FnMut() -> Result<Duration, AdapterError>,
    cap: usize,
) -> Result<Zeroizing<Vec<u8>>, AdapterError> {
    let mut line = Zeroizing::new(Vec::new());
    loop {
        stream
            .set_read_timeout(Some(allowance(remaining)?))
            .map_err(|_| fail("bound HTTP timeout configuration failed"))?;
        let mut byte = [0];
        if stream
            .read(&mut byte)
            .map_err(|_| fail("bound HTTP response unavailable"))?
            == 0
        {
            return Err(fail("bound HTTP response unavailable"));
        }
        line.push(byte[0]);
        if line.len() > cap {
            return Err(fail("bound HTTP response line bound exceeded"));
        }
        if byte[0] == b'\n' {
            return Ok(line);
        }
    }
}
fn read_bytes(
    stream: &mut TcpStream,
    remaining: &mut impl FnMut() -> Result<Duration, AdapterError>,
    size: usize,
) -> Result<Zeroizing<Vec<u8>>, AdapterError> {
    if size > MAX_BODY_BYTES {
        return Err(fail("bound HTTP response body bound exceeded"));
    }
    let mut bytes = Zeroizing::new(vec![0; size]);
    let mut offset = 0;
    while offset < size {
        stream
            .set_read_timeout(Some(allowance(remaining)?))
            .map_err(|_| fail("bound HTTP timeout configuration failed"))?;
        let count = stream
            .read(&mut bytes[offset..])
            .map_err(|_| fail("bound HTTP response unavailable"))?;
        if count == 0 {
            return Err(fail("bound HTTP response unavailable"));
        }
        offset += count;
    }
    Ok(bytes)
}
fn read_body(
    stream: &mut TcpStream,
    remaining: &mut impl FnMut() -> Result<Duration, AdapterError>,
    length: Option<usize>,
    chunked: bool,
) -> Result<Zeroizing<Vec<u8>>, AdapterError> {
    if chunked {
        let mut body = Zeroizing::new(Vec::new());
        loop {
            let line = read_line(stream, remaining, 128)?;
            let text = std::str::from_utf8(&line)
                .map_err(|_| fail("bound HTTP chunk invalid"))?
                .trim();
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(fail("bound HTTP chunk invalid"));
            }
            let size =
                usize::from_str_radix(text, 16).map_err(|_| fail("bound HTTP chunk invalid"))?;
            if body.len().saturating_add(size) > MAX_BODY_BYTES {
                return Err(fail("bound HTTP response body bound exceeded"));
            }
            if size == 0 {
                if read_line(stream, remaining, MAX_HEADER_BYTES)?.as_slice() != b"\r\n" {
                    return Err(fail("bound HTTP trailers unsupported"));
                }
                break;
            }
            body.extend_from_slice(&read_bytes(stream, remaining, size)?);
            if read_bytes(stream, remaining, 2)?.as_slice() != b"\r\n" {
                return Err(fail("bound HTTP chunk invalid"));
            }
        }
        Ok(body)
    } else if let Some(length) = length {
        read_bytes(stream, remaining, length)
    } else {
        let mut body = Zeroizing::new(Vec::new());
        loop {
            stream
                .set_read_timeout(Some(allowance(remaining)?))
                .map_err(|_| fail("bound HTTP timeout configuration failed"))?;
            let mut chunk = [0; 4096];
            let size = stream
                .read(&mut chunk)
                .map_err(|_| fail("bound HTTP response unavailable"))?;
            if body.len() + size > MAX_BODY_BYTES {
                chunk.zeroize();
                return Err(fail("bound HTTP response body bound exceeded"));
            }
            body.extend_from_slice(&chunk[..size]);
            chunk.zeroize();
            if size == 0 {
                return Ok(body);
            }
        }
    }
}

// JSON members must be unambiguous before they can select a private binding.
struct StrictJson(serde_json::Value);
impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictJson;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bounded JSON value")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| StrictJson(n.into()))
                    .ok_or_else(|| E::custom("invalid JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictJson(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StrictJson(serde_json::Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = vec![];
                while let Some(StrictJson(value)) = a.next_element()? {
                    values.push(value);
                }
                Ok(StrictJson(values.into()))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut map = serde_json::Map::new();
                while let Some((key, StrictJson(value))) = a.next_entry::<String, StrictJson>()? {
                    if map.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON member"));
                    }
                }
                Ok(StrictJson(map.into()))
            }
        }
        de.deserialize_any(Visitor)
    }
}

/// Compatibility entry point: one request without persistent captures/cookies.
pub fn invoke<'a>(
    upstream: SocketAddr,
    template: &RequestTemplate,
    resolve: impl FnMut(&str) -> Result<&'a str, AdapterError>,
    remaining: impl FnMut() -> Result<Duration, AdapterError>,
    observe: impl FnMut(Observation) -> Result<(), AdapterError>,
) -> Result<u16, AdapterError> {
    Session::new(upstream)?.invoke(template, &[], &[], resolve, remaining, observe)
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
            ..Default::default()
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

    fn receive_request(socket: &mut TcpStream) -> (String, serde_json::Value) {
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let length: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).unwrap();
        (
            head,
            if body.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&body).unwrap()
            },
        )
    }
    fn json_response(socket: &mut TcpStream, status: u16, headers: &str, body: &str) {
        write!(socket, "HTTP/1.1 {status} private-reason-canary\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()).unwrap();
    }
    fn capture(binding: &str, pointer: &str, scalar: ScalarKind) -> ResponseBinding {
        ResponseBinding {
            binding: binding.into(),
            json_pointer: pointer.into(),
            html_text_id: None,
            json_object_key: None,
            scalar,
            max_bytes: 128,
        }
    }

    #[test]
    fn transient_scalars_cookies_and_headers_remain_private_and_attempt_isolated() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (head, body) = receive_request(&mut socket);
            assert!(head.starts_with("POST /entities "));
            assert_eq!(body["password"], "private-input-canary");
            json_response(
                &mut socket,
                201,
                "Set-Cookie: session=private-cookie-canary; Path=/; HttpOnly; SameSite=Lax; Max-Age=600; Expires=Thu, 01 Jan 1970 00:00:00 GMT\r\n",
                r#"{"id":"private-id-canary","enabled":true,"token":"private-token-canary"}"#,
            );
            drop(socket);
            let (mut socket, _) = listener.accept().unwrap();
            let (head, body) = receive_request(&mut socket);
            assert!(head.starts_with("PATCH /entities/private-id-canary "));
            assert!(head.contains("authorization: Bearer private-token-canary\r\n"));
            assert!(head.contains("cookie: session=private-cookie-canary\r\n"));
            assert_eq!(body["enabled"], true);
            json_response(
                &mut socket,
                200,
                "",
                r#"{"enabled":true,"destination":"https://private-destination-canary.invalid/path"}"#,
            );
            drop(socket);
            let (mut socket, _) = listener.accept().unwrap();
            let (head, _) = receive_request(&mut socket);
            assert!(head.starts_with("GET /entities "));
            assert!(!head.contains("cookie:"));
            json_response(&mut socket, 200, "", "{}");
        });
        let mut session = Session::new(target).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut evidence = vec![];
        let first = RequestTemplate {
            path: "/entities".into(),
            cookies: true,
            ..template()
        };
        session
            .invoke(
                &first,
                &[
                    capture("ENTITY_ID", "/id", ScalarKind::String),
                    capture("ENABLED", "/enabled", ScalarKind::Boolean),
                    capture("TOKEN", "/token", ScalarKind::String),
                ],
                &[],
                |_| Ok("private-input-canary"),
                || Ok(deadline.saturating_duration_since(Instant::now())),
                |o| {
                    evidence.push(o);
                    Ok(())
                },
            )
            .unwrap();
        let second = RequestTemplate {
            method: Method::Patch,
            path: "/entities/{id}".into(),
            path_bindings: BTreeMap::from([("id".into(), "ENTITY_ID".into())]),
            json_bindings: BTreeMap::from([("enabled".into(), "ENABLED".into())]),
            header_bindings: BTreeMap::from([(
                "authorization".into(),
                HeaderBinding {
                    binding: "TOKEN".into(),
                    encoding: HeaderEncoding::Bearer,
                },
            )]),
            cookies: true,
            ..Default::default()
        };
        session
            .invoke(
                &second,
                &[],
                &[
                    ResponseCheck {
                        json_pointer: "/enabled".into(),
                        header_name: None,
                        binding: "ENABLED".into(),
                        scalar: None,
                    },
                    ResponseCheck {
                        json_pointer: "/destination".into(),
                        header_name: None,
                        binding: "DESTINATION".into(),
                        scalar: None,
                    },
                ],
                |name| {
                    assert_eq!(name, "DESTINATION");
                    Ok("https://private-destination-canary.invalid/path")
                },
                || Ok(deadline.saturating_duration_since(Instant::now())),
                |o| {
                    evidence.push(o);
                    Ok(())
                },
            )
            .unwrap();
        assert!(session.has_binding("ENTITY_ID"));
        assert!(!format!("{session:?}").contains("canary"));
        let public = serde_json::to_string(&evidence).unwrap();
        assert!(!public.contains("canary"));
        assert!(!public.contains("127.0.0.1"));
        assert!(public.contains("\"matched\":true"));
        let mut fresh = Session::new(target).unwrap();
        assert!(!fresh.has_binding("ENTITY_ID"));
        fresh
            .invoke(
                &RequestTemplate {
                    path: "/entities".into(),
                    cookies: true,
                    ..Default::default()
                },
                &[],
                &[],
                |_| panic!("fresh GET has no bindings"),
                || Ok(deadline.saturating_duration_since(Instant::now())),
                |_| Ok(()),
            )
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn unsafe_private_path_and_header_values_refuse_before_dispatch() {
        for value in [
            "../outside",
            "https://external.invalid",
            "a/b",
            "%2e%2e",
            "..",
            "x?secret=y",
            "x\r\nInjected: y",
        ] {
            let mut session = Session::new("127.0.0.1:1".parse().unwrap()).unwrap();
            let request = RequestTemplate {
                path: "/entities/{id}".into(),
                path_bindings: BTreeMap::from([("id".into(), "ENTITY".into())]),
                ..Default::default()
            };
            let error = session
                .invoke(
                    &request,
                    &[],
                    &[],
                    |_| Ok(value),
                    || Ok(Duration::from_secs(1)),
                    |_| panic!("invalid path must not dispatch"),
                )
                .unwrap_err();
            assert!(!format!("{error:?}").contains(value));
        }
        for value in [
            "private-header-canary\r\nInjected: secret",
            "private-header-canary\0",
            "private-header-canary with space",
        ] {
            let mut session = Session::new("127.0.0.1:1".parse().unwrap()).unwrap();
            let request = RequestTemplate {
                path: "/entities".into(),
                header_bindings: BTreeMap::from([(
                    "authorization".into(),
                    HeaderBinding {
                        binding: "TOKEN".into(),
                        encoding: HeaderEncoding::Bearer,
                    },
                )]),
                ..Default::default()
            };
            let error = session
                .invoke(
                    &request,
                    &[],
                    &[],
                    |_| Ok(value),
                    || Ok(Duration::from_secs(1)),
                    |_| panic!("invalid header must not dispatch"),
                )
                .unwrap_err();
            assert!(!format!("{error:?}").contains("canary"));
        }
    }

    #[test]
    fn invalid_response_cannot_capture_secrets_or_replay_mutation() {
        for response in [
            "HTTP/1.1 201 OK\r\nContent-Type: application/json\r\nContent-Length: 28\r\n\r\n{\"id\":\"private-body-canary\"}",
            "HTTP/1.1 201 OK\r\nContent-Type: application/json\r\nContent-Length: 70000\r\n\r\n",
            "HTTP/1.1 201 OK\r\nContent-Type: application/json\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n",
            "HTTP/1.1 201 OK\r\nContent-Type: application/json\r\nContent-Type: text/plain\r\n\r\nprivate-body-canary",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                receive_request(&mut socket);
                let _ = socket.write_all(response.as_bytes());
                drop(socket);
                listener.set_nonblocking(true).unwrap();
                assert!(listener.accept().is_err());
            });
            let mut session = Session::new(target).unwrap();
            let mut evidence = vec![];
            let error = session
                .invoke(
                    &template(),
                    &[capture("ENTITY", "/id", ScalarKind::Integer)],
                    &[],
                    |_| Ok("private-input-canary"),
                    || Ok(Duration::from_secs(2)),
                    |o| {
                        evidence.push(o);
                        Ok(())
                    },
                )
                .unwrap_err();
            server.join().unwrap();
            assert!(!session.has_binding("ENTITY"));
            assert!(!format!("{error:?}").contains("canary"));
            assert!(!serde_json::to_string(&evidence).unwrap().contains("canary"));
        }
        for body in [
            r#"{"id":"private-body-canary","id":"other"}"#,
            r#"{"id":[]}"#,
            r#"{"id":""}"#,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                receive_request(&mut socket);
                json_response(&mut socket, 201, "", body);
            });
            let mut session = Session::new(target).unwrap();
            let error = session
                .invoke(
                    &template(),
                    &[capture("ENTITY", "/id", ScalarKind::String)],
                    &[],
                    |_| Ok("private-input-canary"),
                    || Ok(Duration::from_secs(2)),
                    |_| Ok(()),
                )
                .unwrap_err();
            server.join().unwrap();
            assert!(!session.has_binding("ENTITY"));
            assert!(!format!("{error:?}").contains("canary"));
        }
    }

    #[test]
    fn cookie_expiry_domain_scope_and_bounds_fail_closed() {
        let mut session = Session::new("127.0.0.1:1".parse().unwrap()).unwrap();
        session
            .store_cookies(
                &[Zeroizing::new(
                    "session=private-cookie-canary; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT"
                        .into(),
                )],
                "/",
            )
            .unwrap();
        assert!(session.cookies.is_empty());
        session
            .store_cookies(
                &[Zeroizing::new(
                    "session=private-cookie-canary; Path=/; Max-Age=300".into(),
                )],
                "/",
            )
            .unwrap();
        assert_eq!(session.cookies.len(), 1);
        session
            .store_cookies(
                &[Zeroizing::new(
                    "session=private-cookie-canary; Path=/; Max-Age=0".into(),
                )],
                "/",
            )
            .unwrap();
        assert!(session.cookies.is_empty());
        for line in [
            "session=private-cookie-canary; Domain=external.invalid",
            "session=private-cookie-canary; Path=/../outside",
            "session=private-cookie-canary; Max-Age=unknown",
            "session=private-cookie-canary; Path=/; Path=/other",
        ] {
            let error = session
                .store_cookies(&[Zeroizing::new(line.into())], "/")
                .unwrap_err();
            assert!(!format!("{error:?}").contains("canary"));
        }
        for i in 0..16 {
            session
                .store_cookies(
                    &[Zeroizing::new(format!(
                        "key{i}=private-cookie-canary; Path=/"
                    ))],
                    "/",
                )
                .unwrap();
        }
        assert!(
            session
                .store_cookies(
                    &[Zeroizing::new(
                        "overflow=private-cookie-canary; Path=/".into()
                    )],
                    "/"
                )
                .is_err()
        );
        assert!(session.cookies.is_empty());
    }

    #[test]
    fn explicit_methods_typed_initial_checks_and_chunked_response_use_one_transport() {
        for (method, expected) in [(Method::Put, "PUT"), (Method::Delete, "DELETE")] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let (head, body) = receive_request(&mut socket);
                assert!(head.starts_with(&format!("{expected} /entities ")));
                assert_eq!(body["enabled"], true);
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n{\"enabled\":true}\r\n0\r\n\r\n").unwrap();
            });
            let mut session = Session::new(target).unwrap();
            let mut evidence = vec![];
            session
                .invoke(
                    &RequestTemplate {
                        method,
                        path: "/entities".into(),
                        json_bindings: BTreeMap::from([("enabled".into(), "EXPECTED".into())]),
                        json_types: BTreeMap::from([("enabled".into(), ScalarKind::Boolean)]),
                        ..Default::default()
                    },
                    &[],
                    &[ResponseCheck {
                        json_pointer: "/enabled".into(),
                        header_name: None,
                        binding: "EXPECTED".into(),
                        scalar: Some(ScalarKind::Boolean),
                    }],
                    |_| Ok("true"),
                    || Ok(Duration::from_secs(2)),
                    |o| {
                        evidence.push(o);
                        Ok(())
                    },
                )
                .unwrap();
            server.join().unwrap();
            assert!(matches!(
                evidence.last(),
                Some(Observation::ResponseCheck { matched: true, .. })
            ));
        }
    }

    fn html_capture() -> ResponseBinding {
        ResponseBinding {
            binding: "KEY".into(),
            json_pointer: String::new(),
            html_text_id: Some("access-key".into()),
            json_object_key: None,
            scalar: ScalarKind::String,
            max_bytes: 128,
        }
    }
    fn object_key_capture() -> ResponseBinding {
        ResponseBinding {
            binding: "ENTITY_ID".into(),
            json_pointer: String::new(),
            html_text_id: None,
            json_object_key: Some(ObjectKeySelector {
                pointer: String::new(),
                where_pointer: "/url".into(),
                binding: "EXPECTED_URL".into(),
            }),
            scalar: ScalarKind::String,
            max_bytes: 128,
        }
    }
    #[test]
    fn exact_html_leaf_and_object_key_predicate_feed_only_private_http_bindings() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let (head, _) = receive_request(&mut socket);
            assert!(head.starts_with("GET /settings "));
            let body = r#"<!DOCTYPE html><html><head><title>Settings</title></head><body><span id="access-key">private-key-canary</span></body></html>"#;
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            drop(socket);
            let (mut socket, _) = listener.accept().unwrap();
            let (head, _) = receive_request(&mut socket);
            assert!(head.contains("x-api-key: private-key-canary\r\n"));
            json_response(
                &mut socket,
                200,
                "",
                r#"{"other":{"url":"https://other.invalid/"},"private-id-canary":{"url":"https://private-url-canary.invalid/path"}}"#,
            );
            drop(socket);
            let (mut socket, _) = listener.accept().unwrap();
            let (head, _) = receive_request(&mut socket);
            assert!(head.starts_with("GET /entities/private-id-canary "));
            assert!(head.contains("cookie: token=private-key-canary\r\n"));
            socket.write_all(b"HTTP/1.1 302 OK\r\nLocation: https://private-url-canary.invalid/path\r\nContent-Length: 0\r\n\r\n").unwrap();
            drop(socket);
            listener.set_nonblocking(true).unwrap();
            assert!(listener.accept().is_err(), "redirect must not be followed");
        });
        let mut session = Session::new(target).unwrap();
        let mut evidence = vec![];
        let deadline = Instant::now() + Duration::from_secs(2);
        session
            .invoke(
                &RequestTemplate {
                    path: "/settings".into(),
                    ..Default::default()
                },
                &[html_capture()],
                &[],
                |_| panic!("HTML capture needs no inputs"),
                || Ok(deadline.saturating_duration_since(Instant::now())),
                |o| {
                    evidence.push(o);
                    Ok(())
                },
            )
            .unwrap();
        session
            .invoke(
                &RequestTemplate {
                    path: "/entities".into(),
                    header_bindings: BTreeMap::from([(
                        "x-api-key".into(),
                        HeaderBinding {
                            binding: "KEY".into(),
                            encoding: HeaderEncoding::Direct,
                        },
                    )]),
                    ..Default::default()
                },
                &[object_key_capture()],
                &[],
                |name| {
                    assert_eq!(name, "EXPECTED_URL");
                    Ok("https://private-url-canary.invalid/path")
                },
                || Ok(deadline.saturating_duration_since(Instant::now())),
                |o| {
                    evidence.push(o);
                    Ok(())
                },
            )
            .unwrap();
        let check = ResponseCheck {
            json_pointer: String::new(),
            header_name: Some("location".into()),
            binding: "EXPECTED_URL".into(),
            scalar: None,
        };
        assert_eq!(
            session
                .invoke(
                    &RequestTemplate {
                        path: "/entities/{id}".into(),
                        path_bindings: BTreeMap::from([("id".into(), "ENTITY_ID".into())]),
                        cookie_bindings: BTreeMap::from([("token".into(), "KEY".into())]),
                        cookies: true,
                        ..Default::default()
                    },
                    &[],
                    &[check],
                    |_| Ok("https://private-url-canary.invalid/path"),
                    || Ok(deadline.saturating_duration_since(Instant::now())),
                    |o| {
                        evidence.push(o);
                        Ok(())
                    }
                )
                .unwrap(),
            302
        );
        server.join().unwrap();
        assert!(!serde_json::to_string(&evidence).unwrap().contains("canary"));
        assert!(matches!(
            evidence.last(),
            Some(Observation::ResponseCheck { matched: true, .. })
        ));
    }

    #[test]
    fn html_capture_refuses_parse_errors_ambiguous_nested_and_executable_elements() {
        for inner in [
            r#"<span id="access-key">private-key-canary</span><span id="access-key">second</span>"#,
            r#"<span id="access-key"><b>private-key-canary</b></span>"#,
            r#"<script id="access-key">private-key-canary</script>"#,
            r#"<span id="access-key" id="other">private-key-canary</span>"#,
            r#"<span id="access-key"> </span>"#,
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                receive_request(&mut socket);
                let body = format!(
                    "<!DOCTYPE html><html><head><title>Settings</title></head><body>{inner}</body></html>"
                );
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            });
            let mut session = Session::new(target).unwrap();
            let error = session
                .invoke(
                    &RequestTemplate {
                        path: "/settings".into(),
                        ..Default::default()
                    },
                    &[html_capture()],
                    &[],
                    |_| panic!("no inputs"),
                    || Ok(Duration::from_secs(2)),
                    |_| Ok(()),
                )
                .unwrap_err();
            server.join().unwrap();
            assert!(!session.has_binding("KEY"));
            assert!(!format!("{error:?}").contains("canary"));
        }
    }

    #[test]
    fn object_key_capture_requires_exactly_one_match_bounded_keys_and_entry_count() {
        let mut oversized = serde_json::Map::new();
        for i in 0..129 {
            oversized.insert(
                format!("key{i}"),
                serde_json::json!({"url":"private-url-canary"}),
            );
        }
        for body in [
            r#"{"a":{"url":"private-url-canary"},"b":{"url":"private-url-canary"}}"#.to_owned(),
            r#"{"a":{"url":"other"}}"#.to_owned(),
            r#"{"private-id-canary":{"url":"other","url":"private-url-canary"}}"#.to_owned(),
            r#"{"private-id-canary":{"url":"private-url-canary"},"private-id-canary":{"url":"other"}}"#.to_owned(),
            r#"{"../private-key-canary":{"url":"private-url-canary"}}"#.to_owned(),
            serde_json::to_string(&oversized).unwrap(),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                receive_request(&mut socket);
                json_response(&mut socket, 200, "", &body);
            });
            let mut session = Session::new(target).unwrap();
            let error = session
                .invoke(
                    &RequestTemplate {
                        path: "/entities".into(),
                        ..Default::default()
                    },
                    &[object_key_capture()],
                    &[],
                    |_| Ok("private-url-canary"),
                    || Ok(Duration::from_secs(2)),
                    |_| Ok(()),
                )
                .unwrap_err();
            server.join().unwrap();
            assert!(!session.has_binding("ENTITY_ID"));
            assert!(!format!("{error:?}").contains("canary"));
        }
    }

    #[test]
    fn response_header_equality_is_private_single_valued_and_never_follows_redirects() {
        for headers in [
            "Location: private-url-canary\r\nLocation: private-url-canary\r\n",
            "Location: wrong-private-url-canary\r\n",
            "",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let target = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                receive_request(&mut socket);
                write!(
                    socket,
                    "HTTP/1.1 302 OK\r\n{headers}Content-Length: 0\r\n\r\n"
                )
                .unwrap();
                drop(socket);
                listener.set_nonblocking(true).unwrap();
                assert!(listener.accept().is_err());
            });
            let mut session = Session::new(target).unwrap();
            let mut evidence = vec![];
            let error = session
                .invoke(
                    &RequestTemplate {
                        path: "/entities".into(),
                        ..Default::default()
                    },
                    &[],
                    &[ResponseCheck {
                        json_pointer: String::new(),
                        header_name: Some("location".into()),
                        binding: "EXPECTED_URL".into(),
                        scalar: None,
                    }],
                    |_| Ok("private-url-canary"),
                    || Ok(Duration::from_secs(2)),
                    |o| {
                        evidence.push(o);
                        Ok(())
                    },
                )
                .unwrap_err();
            server.join().unwrap();
            assert!(!format!("{error:?}").contains("canary"));
            assert!(!serde_json::to_string(&evidence).unwrap().contains("canary"));
        }
    }
}
