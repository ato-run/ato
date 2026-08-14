//! Authenticated public-path readiness probe for Terminal Surface v1.

use std::sync::Arc;
use std::time::Duration;

use ato_ipc::terminal_surface::{TERMINAL_WEBSOCKET_SUBPROTOCOL, TerminalServerControl};
use futures_util::StreamExt;
use http::{
    HeaderValue,
    header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL},
};
use thiserror::Error;
use tokio::time::timeout;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

use crate::rfb_probe::pixel_gateway_probe_url;
use crate::surface_authorization::SurfaceAccessAuthorizer;
use crate::surface_websocket_auth::SURFACE_ASSERTION_HEADER;
use crate::terminal_gateway::{
    TerminalGatewayConfig, TerminalGatewayError, TerminalGatewayHandle, start_terminal_gateway,
};

pub struct TerminalGatewayProbeRequest<'a> {
    connect_url: &'a str,
    origin: &'a str,
    assertion: &'a str,
}

impl<'a> TerminalGatewayProbeRequest<'a> {
    pub fn new(connect_url: &'a str, origin: &'a str, assertion: &'a str) -> Self {
        Self {
            connect_url,
            origin,
            assertion,
        }
    }
}

#[derive(Debug, Error)]
pub enum TerminalProbeError {
    #[error("terminal readiness probe timed out")]
    Timeout,
    #[error("terminal readiness URL is invalid or contains credential material")]
    InvalidGatewayUrl,
    #[error("terminal readiness request headers are invalid")]
    InvalidGatewayHeaders,
    #[error("terminal readiness WebSocket failed: {0}")]
    WebSocket(#[source] tokio_tungstenite::tungstenite::Error),
    #[error("terminal gateway did not negotiate ato.terminal.v1")]
    MissingSubprotocol,
    #[error("terminal gateway returned an invalid readiness frame")]
    InvalidReadyFrame,
}

#[derive(Debug, Error)]
pub enum TerminalGatewayReadyError {
    #[error("terminal gateway failed to start: {0}")]
    Start(#[source] TerminalGatewayError),
    #[error("authenticated terminal gateway readiness probe failed: {0}")]
    Probe(#[source] TerminalProbeError),
    #[error("terminal readiness failed and gateway cleanup was not confirmed")]
    ProbeCleanup {
        #[source]
        probe: TerminalProbeError,
        cleanup: TerminalGatewayError,
    },
}

pub async fn start_ready_terminal_gateway(
    config: TerminalGatewayConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    request: TerminalGatewayProbeRequest<'_>,
    wait: Duration,
) -> Result<TerminalGatewayHandle, TerminalGatewayReadyError> {
    let handle = start_terminal_gateway(config, authorizer)
        .await
        .map_err(TerminalGatewayReadyError::Start)?;
    match wait_for_terminal_ready(request, wait).await {
        Ok(()) => Ok(handle),
        Err(probe) => match handle.stop().await {
            Ok(()) => Err(TerminalGatewayReadyError::Probe(probe)),
            Err(cleanup) => Err(TerminalGatewayReadyError::ProbeCleanup { probe, cleanup }),
        },
    }
}

pub async fn wait_for_terminal_ready(
    request: TerminalGatewayProbeRequest<'_>,
    wait: Duration,
) -> Result<(), TerminalProbeError> {
    timeout(wait, probe(request))
        .await
        .map_err(|_| TerminalProbeError::Timeout)?
}

async fn probe(request: TerminalGatewayProbeRequest<'_>) -> Result<(), TerminalProbeError> {
    let connect_url = pixel_gateway_probe_url(request.connect_url)
        .map_err(|_| TerminalProbeError::InvalidGatewayUrl)?;
    let mut websocket_request = connect_url
        .as_str()
        .into_client_request()
        .map_err(|_| TerminalProbeError::InvalidGatewayUrl)?;
    websocket_request.headers_mut().insert(
        ORIGIN,
        HeaderValue::from_str(request.origin)
            .map_err(|_| TerminalProbeError::InvalidGatewayHeaders)?,
    );
    websocket_request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static(TERMINAL_WEBSOCKET_SUBPROTOCOL),
    );
    websocket_request.headers_mut().insert(
        SURFACE_ASSERTION_HEADER,
        HeaderValue::from_str(request.assertion)
            .map_err(|_| TerminalProbeError::InvalidGatewayHeaders)?,
    );
    let (mut websocket, response) = connect_async(websocket_request)
        .await
        .map_err(TerminalProbeError::WebSocket)?;
    if response
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some(TERMINAL_WEBSOCKET_SUBPROTOCOL)
    {
        return Err(TerminalProbeError::MissingSubprotocol);
    }
    let message = websocket
        .next()
        .await
        .ok_or(TerminalProbeError::InvalidReadyFrame)?
        .map_err(TerminalProbeError::WebSocket)?;
    let Message::Text(text) = message else {
        return Err(TerminalProbeError::InvalidReadyFrame);
    };
    let control: TerminalServerControl =
        serde_json::from_str(text.as_ref()).map_err(|_| TerminalProbeError::InvalidReadyFrame)?;
    if !matches!(control, TerminalServerControl::Ready { .. }) || control.validate().is_err() {
        return Err(TerminalProbeError::InvalidReadyFrame);
    }
    let _ = websocket.close(None).await;
    Ok(())
}
