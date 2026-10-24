//! Surface-kind-neutral authorization types shared by authenticated gateways.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceGatewayScope {
    pub session_id: String,
    pub surface_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedSurfaceAccess {
    pub principal: String,
    /// Only the existing signed User/execution claim may set this deadline.
    /// None is default deny; no remote-to-client permission is represented.
    pub clipboard_paste_until: Option<u64>,
    /// Unique assertion identifier (`jti`). A gateway consumes it once.
    pub grant_id: String,
}

#[derive(Debug, Clone, Copy, Error)]
#[error("surface assertion rejected")]
pub struct SurfaceAuthorizationError;

pub trait SurfaceAccessAuthorizer: Send + Sync + 'static {
    fn authorize(
        &self,
        assertion: &str,
        scope: &SurfaceGatewayScope,
    ) -> Result<AuthorizedSurfaceAccess, SurfaceAuthorizationError>;
}
