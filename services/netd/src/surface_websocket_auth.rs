//! Shared exact-origin, assertion, replay, and subprotocol WebSocket boundary.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};

use http::{
    HeaderValue, StatusCode,
    header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
};
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use url::Url;

use crate::surface_authorization::{SurfaceAccessAuthorizer, SurfaceGatewayScope};

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
        }
    }
}

impl Callback for SurfaceHandshakeAuthorizer {
    fn on_request(
        self,
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        authorize_upgrade(
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
) -> Result<(), UpgradeRejection> {
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
    consumed.insert(access.grant_id);
    Ok(())
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
