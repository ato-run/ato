//! `atoview://` proxy for sandboxed app-view frames.
//!
//! The HTTP client owns the app-view cookie. Redirect policy revalidates every
//! target, so an allowed session host cannot redirect the native client into an
//! arbitrary network location.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Mutex;

use tauri::http::{Request, Response, StatusCode};

pub const ATOVIEW_SCHEME: &str = "atoview";
const ALLOWED_HOST_SUFFIX: &str = ".app.ato.run";
const APP_VIEW_TOKEN_PARAM: &str = "app_view_token";
const MAX_REDIRECTS: usize = 5;

pub struct AppViewProxy {
    client: reqwest::Client,
    authed_hosts: Mutex<HashSet<String>>,
}

impl AppViewProxy {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("app-view redirect limit exceeded");
                }
                if allowed_upstream_url(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("app-view redirect target is not allowed")
                }
            }))
            .build()
            .expect("build app-view proxy http client");
        Self {
            client,
            authed_hosts: Mutex::new(HashSet::new()),
        }
    }

    pub async fn handle(&self, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
        match self.try_proxy(request).await {
            Ok(response) => response,
            Err(status) => error_response(status),
        }
    }

    async fn try_proxy(
        &self,
        request: Request<Vec<u8>>,
    ) -> Result<Response<Cow<'static, [u8]>>, StatusCode> {
        let uri = request.uri();
        let host = uri.host().ok_or(StatusCode::BAD_REQUEST)?.to_owned();
        if !allowed_app_view_host(&host) {
            return Err(StatusCode::FORBIDDEN);
        }
        let path_and_query = uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/");
        let mut upstream = format!("https://{host}{path_and_query}");

        let already_authed = self
            .authed_hosts
            .lock()
            .map(|hosts| hosts.contains(&host))
            .unwrap_or(false);
        if already_authed {
            upstream = strip_query_param(&upstream, APP_VIEW_TOKEN_PARAM);
        }

        let method = reqwest::Method::from_bytes(request.method().as_str().as_bytes())
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        let mut outbound = self.client.request(method, &upstream);
        for (name, value) in request.headers() {
            if should_drop_request_header(name.as_str()) {
                continue;
            }
            if let Ok(value) = value.to_str() {
                outbound = outbound.header(name.as_str(), value);
            }
        }
        let body = request.into_body();
        if !body.is_empty() {
            outbound = outbound.body(body);
        }

        let response = outbound.send().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        if let Ok(mut hosts) = self.authed_hosts.lock() {
            hosts.insert(host);
        }

        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut builder = Response::builder().status(status);
        for (name, value) in response.headers() {
            if !should_drop_response_header(name.as_str()) {
                builder = builder.header(name.as_str(), value.as_bytes());
            }
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        builder
            .body(Cow::Owned(bytes.to_vec()))
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }
}

impl Default for AppViewProxy {
    fn default() -> Self {
        Self::new()
    }
}

fn allowed_upstream_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https" && url.host_str().is_some_and(allowed_app_view_host)
}

fn allowed_app_view_host(host: &str) -> bool {
    host.strip_suffix(ALLOWED_HOST_SUFFIX)
        .is_some_and(|session| !session.is_empty() && !session.contains('.'))
}

fn should_drop_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "cookie" | "origin" | "referer" | "connection" | "content-length"
    )
}

fn should_drop_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "set-cookie"
            | "x-frame-options"
            | "content-security-policy"
            | "content-security-policy-report-only"
            | "connection"
            | "transfer-encoding"
            | "content-length"
            | "keep-alive"
    )
}

fn strip_query_param(url: &str, param: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_owned();
    };
    let kept = query
        .split('&')
        .filter(|pair| pair.split_once('=').map_or(*pair, |(key, _)| key) != param)
        .collect::<Vec<_>>();
    if kept.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{}", kept.join("&"))
    }
}

fn error_response(status: StatusCode) -> Response<Cow<'static, [u8]>> {
    let body = format!(
        "{{\"error\":\"app_view_proxy_error\",\"status\":{}}}",
        status.as_u16()
    );
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Cow::Owned(body.into_bytes()))
        .expect("build proxy error response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist_requires_one_session_label() {
        assert!(allowed_app_view_host("session-1.app.ato.run"));
        assert!(!allowed_app_view_host("app.ato.run"));
        assert!(!allowed_app_view_host("nested.session.app.ato.run"));
        assert!(!allowed_app_view_host("session.app.ato.run.evil.example"));
    }

    #[test]
    fn redirect_allowlist_requires_https() {
        assert!(allowed_upstream_url(
            &"https://session-1.app.ato.run/path".parse().unwrap()
        ));
        assert!(!allowed_upstream_url(
            &"http://session-1.app.ato.run/path".parse().unwrap()
        ));
        assert!(!allowed_upstream_url(
            &"https://evil.example/path".parse().unwrap()
        ));
    }

    #[test]
    fn strips_only_the_named_query_parameter() {
        assert_eq!(
            strip_query_param(
                "https://s.app.ato.run/p?a=1&app_view_token=abc&b=2",
                "app_view_token"
            ),
            "https://s.app.ato.run/p?a=1&b=2"
        );
    }
}
