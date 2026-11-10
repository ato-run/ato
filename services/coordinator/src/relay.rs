//! Outbound relay protocol (G4): the origin side.
//!
//! The home Coordinator never accepts an inbound connection. It polls a relay
//! it dialed out to, receives opaque envelopes, and decides every request
//! itself. This module defines that envelope and the origin's checks; the
//! HTTPS long-poll transport and the hosted relay endpoint are follow-up
//! slices, so nothing here claims a production remote path yet.
//!
//! Invariants this slice enforces:
//!
//! - the relay is opaque: it carries bytes and must not persist or log the
//!   authorization value or the body;
//! - the origin is the only authority: every envelope is re-authorized here,
//!   not by the relay;
//! - fencing: an envelope for another `origin_epoch` is refused, so a replaced
//!   authority cannot be spoken to.

use std::sync::Mutex;

use hyper::Method;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::Coordinator;
use crate::http::{ApiState, handle_request};

pub const RELAY_ENVELOPE_SCHEMA: &str = "ato.relay-envelope/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRequest {
    pub schema: String,
    pub request_id: String,
    /// The authority generation this request was addressed to.
    pub origin_epoch: i64,
    pub method: String,
    pub path: String,
    /// Opaque to the relay; interpreted only by the origin.
    pub body: String,
    /// Opaque to the relay; interpreted only by the origin.
    pub authorization: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayResponse {
    pub schema: String,
    pub request_id: String,
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// The relay could not be reached; the caller must fail closed.
    Unavailable,
    /// The envelope is malformed or for another authority.
    Refused(&'static str),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::Unavailable => write!(formatter, "relay_unavailable"),
            RelayError::Refused(code) => write!(formatter, "{code}"),
        }
    }
}

/// The origin side: validates the envelope and dispatches it through the same
/// HTTP surface a local client uses.
pub struct Origin {
    pub epoch: i64,
    state: ApiState,
}

impl Origin {
    pub fn new(coordinator: Coordinator, epoch: i64, token: String) -> Self {
        let (shutdown, _receiver) = watch::channel(false);
        Self {
            epoch,
            state: ApiState {
                coordinator: Mutex::new(coordinator),
                token,
                shutdown,
            },
        }
    }

    pub async fn handle(&self, request: RelayRequest) -> RelayResponse {
        let refuse = |status: u16, code: &str| RelayResponse {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: request.request_id.clone(),
            status,
            body: serde_json::json!({ "error": code }).to_string(),
        };
        if request.schema != RELAY_ENVELOPE_SCHEMA {
            return refuse(400, "invalid_envelope");
        }
        if request.origin_epoch != self.epoch {
            return refuse(409, "origin_epoch_mismatch");
        }
        let method = match request.method.as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "DELETE" => Method::DELETE,
            _ => return refuse(400, "invalid_method"),
        };
        let response = handle_request(
            &self.state,
            &method,
            &request.path,
            request.authorization.as_deref(),
            request.body.as_bytes(),
        )
        .await;
        let status = response.status().as_u16();
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned())
            .unwrap_or_default();
        RelayResponse {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: request.request_id,
            status,
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new(
            Coordinator::open_in_memory().unwrap(),
            7,
            "test-token".into(),
        )
    }

    fn request(method: &str, path: &str, body: &str) -> RelayRequest {
        RelayRequest {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: "req-1".into(),
            origin_epoch: 7,
            method: method.into(),
            path: path.into(),
            body: body.into(),
            authorization: Some("Bearer test-token".into()),
        }
    }

    #[tokio::test]
    async fn refuses_an_envelope_for_another_authority() {
        let mut request = request("GET", "/v1/data-resources", "");
        request.origin_epoch = 6;
        let response = origin().handle(request).await;
        assert_eq!(response.status, 409);
        assert!(response.body.contains("origin_epoch_mismatch"));
    }

    #[tokio::test]
    async fn refuses_a_malformed_envelope() {
        let mut envelope = request("GET", "/health", "");
        envelope.schema = "ato.relay-envelope/2".into();
        assert_eq!(origin().handle(envelope).await.status, 400);
        let mut envelope = request("PUT", "/health", "");
        envelope.method = "PUT".into();
        assert_eq!(origin().handle(envelope).await.status, 400);
    }

    #[tokio::test]
    async fn the_relay_cannot_bypass_origin_authorization() {
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.authorization = None;
        assert_eq!(origin().handle(envelope).await.status, 401);
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.authorization = Some("Bearer wrong".into());
        assert_eq!(origin().handle(envelope).await.status, 401);
    }

    #[tokio::test]
    async fn dispatches_an_authorized_request_through_the_local_surface() {
        let origin = origin();
        let response = origin.handle(request("GET", "/health", "")).await;
        assert_eq!(response.status, 200);
        assert!(
            response.body.contains("\"status\":\"ok\""),
            "{}",
            response.body
        );

        let created = origin
            .handle(request(
                "POST",
                "/v1/data-resources",
                r#"{"protocol":"todo.board@1","document":{"tasks":{}}}"#,
            ))
            .await;
        assert_eq!(created.status, 201);
        assert!(created.body.contains("resource_id"), "{}", created.body);
    }
}
