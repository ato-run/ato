//! Shared exact-origin, assertion, replay, and subprotocol WebSocket boundary.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use http::{
    HeaderValue, StatusCode,
    header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
};
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use url::Url;

use crate::surface_authorization::{
    AuthorizedSurfaceAccess, SurfaceAccessAuthorizer, SurfaceGatewayScope,
};

pub const SURFACE_ASSERTION_HEADER: &str = "x-ato-surface-assertion";
const MAX_CONSUMED_GRANTS: usize = 4096;

pub type ConsumedSurfaceGrants = Arc<Mutex<HashSet<String>>>;

pub fn new_consumed_surface_grants() -> ConsumedSurfaceGrants {
    Arc::new(Mutex::new(HashSet::new()))
}

pub fn is_normalized_allowed_origin(origin: &str) -> bool {
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.origin().ascii_serialization() != origin
    {
        return false;
    }
    let loopback_host = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    url.scheme() == "https" || (url.scheme() == "http" && loopback_host)
}

pub struct SurfaceHandshakeAuthorizer {
    allowed_origins: BTreeSet<String>,
    scope: SurfaceGatewayScope,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    consumed_grants: ConsumedSurfaceGrants,
    subprotocol: &'static str,
    require_subprotocol: bool,
    access_capture: Option<Arc<OnceLock<AuthorizedSurfaceAccess>>>,
}

impl SurfaceHandshakeAuthorizer {
    pub fn new(
        allowed_origins: BTreeSet<String>,
        scope: SurfaceGatewayScope,
        authorizer: Arc<dyn SurfaceAccessAuthorizer>,
        consumed_grants: ConsumedSurfaceGrants,
        subprotocol: &'static str,
        require_subprotocol: bool,
    ) -> Self {
        Self {
            allowed_origins,
            scope,
            authorizer,
            consumed_grants,
            subprotocol,
            require_subprotocol,
            access_capture: None,
        }
    }
}

impl SurfaceHandshakeAuthorizer {
    pub(crate) fn with_access_capture(
        mut self,
        capture: Arc<OnceLock<AuthorizedSurfaceAccess>>,
    ) -> Self {
        self.access_capture = Some(capture);
        self
    }
}

impl Callback for SurfaceHandshakeAuthorizer {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        let access = authorize_upgrade(
            request,
            &self.allowed_origins,
            &self.scope,
            self.authorizer.as_ref(),
            &self.consumed_grants,
        )
        .map_err(UpgradeRejection::into_response)?;

        if offered_subprotocol(request, self.subprotocol) {
            response.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_static(self.subprotocol),
            );
        } else if self.require_subprotocol {
            return Err(rejection(StatusCode::BAD_REQUEST));
        }
        if let Some(capture) = self.access_capture {
            capture
                .set(access)
                .map_err(|_| rejection(StatusCode::INTERNAL_SERVER_ERROR))?;
        }
        Ok(response)
    }
}

#[derive(Debug, Clone, Copy)]
enum UpgradeRejection {
    Unauthorized,
    Forbidden,
    Internal,
}

impl UpgradeRejection {
    fn into_response(self) -> ErrorResponse {
        rejection(match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        })
    }
}

fn authorize_upgrade(
    request: &Request,
    allowed_origins: &BTreeSet<String>,
    scope: &SurfaceGatewayScope,
    authorizer: &dyn SurfaceAccessAuthorizer,
    consumed_grants: &Mutex<HashSet<String>>,
) -> Result<AuthorizedSurfaceAccess, UpgradeRejection> {
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(UpgradeRejection::Forbidden)?;
    if !allowed_origins.contains(origin) {
        return Err(UpgradeRejection::Forbidden);
    }
    let assertion = request
        .headers()
        .get(SURFACE_ASSERTION_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(UpgradeRejection::Unauthorized)?;
    let access = authorizer
        .authorize(assertion, scope)
        .map_err(|_| UpgradeRejection::Unauthorized)?;
    if access.grant_id.trim().is_empty() || access.principal.trim().is_empty() {
        return Err(UpgradeRejection::Unauthorized);
    }
    let mut consumed = consumed_grants
        .lock()
        .map_err(|_| UpgradeRejection::Internal)?;
    if consumed.contains(&access.grant_id) {
        return Err(UpgradeRejection::Unauthorized);
    }
    if consumed.len() >= MAX_CONSUMED_GRANTS {
        return Err(UpgradeRejection::Internal);
    }
    consumed.insert(access.grant_id.clone());
    Ok(access)
}

fn offered_subprotocol(request: &Request, expected: &str) -> bool {
    request
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|part| part.trim() == expected))
}

fn rejection(status: StatusCode) -> ErrorResponse {
    http::Response::builder()
        .status(status)
        .header("cache-control", "no-store")
        .body(None)
        .unwrap_or_else(|_| http::Response::new(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface_authorization::{SurfaceAccessAuthorizer, SurfaceAuthorizationError};
    use std::collections::HashSet;
    use std::sync::Mutex;

    const LEASE_ORIGIN: &str = "https://staging.ato.run";
    const RUNNER_ORIGIN: &str = "https://runner.example";

    struct Fixed(Option<AuthorizedSurfaceAccess>);

    impl SurfaceAccessAuthorizer for Fixed {
        fn authorize(
            &self,
            assertion: &str,
            _scope: &SurfaceGatewayScope,
        ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError> {
            match (assertion, &self.0) {
                ("valid", Some(access)) => Ok(access.clone()),
                _ => Err(SurfaceAuthorizationError),
            }
        }
    }

    fn access(grant_id: &str) -> AuthorizedSurfaceAccess {
        AuthorizedSurfaceAccess {
            principal: "user-1".into(),
            clipboard_paste_until: None,
            framebuffer_resize_until: None,
            grant_id: grant_id.into(),
        }
    }

    fn scope() -> SurfaceGatewayScope {
        SurfaceGatewayScope {
            session_id: "session-1".into(),
            surface_id: "main".into(),
        }
    }

    fn origins() -> BTreeSet<String> {
        BTreeSet::from([LEASE_ORIGIN.to_string(), RUNNER_ORIGIN.to_string()])
    }

    fn request(origin: Option<&str>, assertion: Option<&str>) -> Request {
        let mut builder = Request::builder().uri("/__ato/surface");
        if let Some(origin) = origin {
            builder = builder.header(ORIGIN, origin);
        }
        if let Some(assertion) = assertion {
            builder = builder.header(SURFACE_ASSERTION_HEADER, assertion);
        }
        builder.body(()).unwrap()
    }

    fn authorize(
        request: &Request,
        consumed: &Mutex<HashSet<String>>,
    ) -> Result<AuthorizedSurfaceAccess, UpgradeRejection> {
        authorize_upgrade(
            request,
            &origins(),
            &scope(),
            &Fixed(Some(access("grant-1"))),
            consumed,
        )
    }

    #[test]
    fn accepts_both_exact_origins_with_a_valid_assertion() {
        for origin in [LEASE_ORIGIN, RUNNER_ORIGIN] {
            let consumed = Mutex::new(HashSet::new());
            let granted = authorize(&request(Some(origin), Some("valid")), &consumed)
                .expect("both exact origins must be accepted");
            assert_eq!(granted.grant_id, "grant-1");
            assert!(consumed.lock().unwrap().contains("grant-1"));
        }
    }

    #[test]
    fn refuses_any_other_origin_even_with_a_valid_assertion() {
        for origin in [
            "https://evil.example",
            "https://runner.example.evil",
            "http://runner.example",
            "https://runner.example:8443",
            "https://RUNNER.example",
            "null",
            "",
        ] {
            let consumed = Mutex::new(HashSet::new());
            assert!(
                matches!(
                    authorize(&request(Some(origin), Some("valid")), &consumed),
                    Err(UpgradeRejection::Forbidden)
                ),
                "origin {origin} must be refused"
            );
            assert!(
                consumed.lock().unwrap().is_empty(),
                "a refused handshake must not consume a grant"
            );
        }
    }

    #[test]
    fn refuses_a_missing_origin_outright() {
        let consumed = Mutex::new(HashSet::new());
        assert!(matches!(
            authorize(&request(None, Some("valid")), &consumed),
            Err(UpgradeRejection::Forbidden)
        ));
    }

    #[test]
    fn never_upgrades_without_a_valid_assertion_on_an_allowed_origin() {
        for assertion in [None, Some(""), Some("forged"), Some("expired")] {
            let consumed = Mutex::new(HashSet::new());
            assert!(
                matches!(
                    authorize(&request(Some(RUNNER_ORIGIN), assertion), &consumed),
                    Err(UpgradeRejection::Unauthorized)
                ),
                "assertion {assertion:?} must not authorize"
            );
            assert!(consumed.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn consumes_the_grant_exactly_once() {
        let consumed = Mutex::new(HashSet::new());
        assert!(authorize(&request(Some(RUNNER_ORIGIN), Some("valid")), &consumed).is_ok());
        assert!(matches!(
            authorize(&request(Some(RUNNER_ORIGIN), Some("valid")), &consumed),
            Err(UpgradeRejection::Unauthorized)
        ));
        assert_eq!(consumed.lock().unwrap().len(), 1);
    }
}
