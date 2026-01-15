use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::{Arc, RwLock};
use tracing::warn;
use uuid::Uuid;

/// Manages authentication tokens for the HTTP API
#[derive(Clone)]
pub struct AuthManager {
    token: Arc<RwLock<Option<String>>>,
}

impl AuthManager {
    /// Create a new AuthManager with a randomly generated token or from env var
    pub fn new() -> Self {
        let token =
            std::env::var("NACELLE_AUTH_TOKEN").unwrap_or_else(|_| Uuid::new_v4().to_string());

        tracing::info!("Using API auth token: {}", &token[..8]);

        // Write token to file for Desktop app to read
        if let Err(e) = Self::write_token_to_file(&token) {
            tracing::warn!("Failed to write token to file: {}", e);
        }

        Self {
            token: Arc::new(RwLock::new(Some(token))),
        }
    }

    /// Write token to app data directory for Desktop app
    ///
    /// On macOS: ~/Library/Application Support/dev.gumball.app/auth_token
    /// On Linux: ~/.local/share/dev.gumball.app/auth_token
    /// On Windows: %APPDATA%/dev.gumball.app/auth_token
    fn write_token_to_file(token: &str) -> std::io::Result<()> {
        use std::fs;
        use std::path::PathBuf;

        // Use platform-specific app data directory to match Tauri's app_local_data_dir()
        let app_data_dir = if cfg!(target_os = "macos") {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join("Library/Application Support/dev.gumball.app")
        } else if cfg!(target_os = "windows") {
            let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(appdata).join("dev.gumball.app")
        } else {
            // Linux and others
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local/share/dev.gumball.app")
        };

        fs::create_dir_all(&app_data_dir)?;

        let token_file = app_data_dir.join("auth_token");
        tracing::info!("Writing auth token to: {:?}", token_file);
        fs::write(&token_file, token)?;
        Ok(())
    }

    /// Get the current authentication token
    pub fn get_token(&self) -> Option<String> {
        self.token.read().ok()?.clone()
    }

    /// Regenerate the authentication token (useful for token rotation)
    pub fn regenerate(&self) -> String {
        let new_token = Uuid::new_v4().to_string();
        if let Ok(mut guard) = self.token.write() {
            *guard = Some(new_token.clone());
            tracing::info!("Regenerated API auth token: {}", &new_token[..8]);
        }
        new_token
    }

    /// Validate a provided token against the stored token
    pub fn validate(&self, provided: &str) -> bool {
        if let Some(expected) = self.get_token() {
            provided == expected
        } else {
            false
        }
    }
}

impl Default for AuthManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Axum middleware for token-based authentication
///
/// Validates the Authorization header for all routes except health checks.
/// Expected format: `Authorization: Bearer <token>`
pub async fn auth_middleware(auth_manager: Arc<AuthManager>, req: Request, next: Next) -> Response {
    let path = req.uri().path();

    // Skip auth for health check endpoint
    if path == "/health" || path == "/v1/status" {
        return next.run(req).await;
    }

    // Extract Authorization header
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok());

    // Check for Bearer token in header
    let provided_token = auth_header.and_then(|h| h.strip_prefix("Bearer "));

    // Simpler approach: Get token string from either source
    let token_str = if let Some(t) = provided_token {
        Some(t.to_string())
    } else {
        req.uri().query().and_then(|q| {
            serde_urlencoded::from_str::<std::collections::HashMap<String, String>>(q)
                .ok()
                .and_then(|p| p.get("token").cloned())
        })
    };

    match token_str {
        Some(token) => {
            if auth_manager.validate(&token) {
                // Valid token - allow request
                next.run(req).await
            } else {
                // Invalid token
                let expected = auth_manager.get_token().unwrap_or_default();
                warn!(
                    "Invalid auth token provided for {}. Received: '{}', Expected: '{}'",
                    path, token, expected
                );
                (StatusCode::UNAUTHORIZED, "Invalid authentication token").into_response()
            }
        }
        None => {
            // Missing token
            warn!("Missing auth token for {}", path);
            (StatusCode::UNAUTHORIZED, "Missing authentication token").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_manager_generates_token() {
        let manager = AuthManager::new();
        let token = manager.get_token();
        assert!(token.is_some());
        assert!(!token.unwrap().is_empty());
    }

    #[test]
    fn test_auth_manager_validates_correct_token() {
        let manager = AuthManager::new();
        let token = manager.get_token().unwrap();
        assert!(manager.validate(&token));
    }

    #[test]
    fn test_auth_manager_rejects_invalid_token() {
        let manager = AuthManager::new();
        assert!(!manager.validate("invalid-token"));
    }

    #[test]
    fn test_auth_manager_regenerate() {
        let manager = AuthManager::new();
        let old_token = manager.get_token().unwrap();
        let new_token = manager.regenerate();

        assert_ne!(old_token, new_token);
        assert!(manager.validate(&new_token));
        assert!(!manager.validate(&old_token));
    }
}
