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
    /// Default deny; bound to the same signed User/execution assertion expiry.
    pub framebuffer_resize_until: Option<u64>,
    /// Unique assertion identifier (`jti`). A gateway consumes it once.
    pub grant_id: String,
    /// Expiry of the verified assertion in Unix seconds.
    pub grant_expires_at: u64,
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
