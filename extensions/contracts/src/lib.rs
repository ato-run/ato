//! Extension-defined Realization acceptance verifiers.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Component, Path};
use std::time::Duration;

use ato_computation::ContentRef;
use ato_materializer_api::{
    ContractContext, ContractDescriptor, ContractResult, ContractVerifier, MaterializerError,
    Realization,
};
use ato_objects::verify_content;
use serde::Deserialize;

pub const HTTP_ENDPOINT_VERIFIER_ID: &str = "ato.contract.http@1";
pub const WORKSPACE_CONTENT_VERIFIER_ID: &str = "ato.contract.workspace@1";
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORKSPACE_FILE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub struct HttpEndpointVerifier;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpEndpointPayload {
    address: SocketAddr,
    path: String,
    expected_status: u16,
    #[serde(default)]
    expected_body_ref: Option<String>,
}

impl ContractVerifier for HttpEndpointVerifier {
    fn id(&self) -> &str {
        HTTP_ENDPOINT_VERIFIER_ID
    }

    fn verify(
        &self,
        descriptor: &ContractDescriptor,
        candidate: &mut dyn Realization,
        _context: &ContractContext<'_>,
    ) -> Result<ContractResult, MaterializerError> {
        let payload: HttpEndpointPayload = serde_json::from_value(descriptor.payload.clone())?;
        validate_internal_http_target(&payload)?;
        let (status, body) = request(&payload)?;
        if status != payload.expected_status {
            return Err(rejected(format!(
                "HTTP status mismatch: expected {}, got {status}",
                payload.expected_status
            )));
        }
        if let Some(reference) = payload.expected_body_ref {
            let reference = ContentRef::parse(reference)
                .map_err(|error| rejected(format!("invalid body reference: {error}")))?;
            verify_content(&reference, &body)
                .map_err(|error| rejected(format!("HTTP body digest mismatch: {error}")))?;
        }
        Ok(ContractResult {
            verifier_id: descriptor.verifier_id.clone(),
            target: candidate.target().clone(),
            summary: format!("HTTP {} {} PASS", payload.address, payload.path),
        })
    }
}

fn validate_internal_http_target(payload: &HttpEndpointPayload) -> Result<(), MaterializerError> {
    if !matches!(payload.address.ip(), IpAddr::V4(ip) if ip.is_loopback())
        && !matches!(payload.address.ip(), IpAddr::V6(ip) if ip.is_loopback())
    {
        return Err(rejected(
            "HTTP Contract address must be a candidate-internal loopback endpoint".to_owned(),
        ));
    }
    if !payload.path.starts_with('/') || payload.path.contains(['\r', '\n']) {
        return Err(rejected(
            "HTTP Contract path must be absolute and contain no line breaks".to_owned(),
        ));
    }
    Ok(())
}

fn request(payload: &HttpEndpointPayload) -> Result<(u16, Vec<u8>), MaterializerError> {
    let mut stream = TcpStream::connect_timeout(&payload.address, Duration::from_secs(5))
        .map_err(|error| rejected(format!("HTTP Contract connect failed: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| rejected(format!("HTTP Contract timeout setup failed: {error}")))?;
    stream
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: contract\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                payload.path
            )
            .as_bytes(),
        )
        .map_err(|error| rejected(format!("HTTP Contract request failed: {error}")))?;
    let mut response = Vec::new();
    stream
        .take((MAX_HTTP_BODY_BYTES + 64 * 1024 + 1) as u64)
        .read_to_end(&mut response)
        .map_err(|error| rejected(format!("HTTP Contract response failed: {error}")))?;
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| rejected("HTTP Contract response has no header boundary".to_owned()))?;
    if boundary > 64 * 1024 {
        return Err(rejected(
            "HTTP Contract response headers exceed 64 KiB".to_owned(),
        ));
    }
    let head = std::str::from_utf8(&response[..boundary])
        .map_err(|_| rejected("HTTP Contract response headers are not UTF-8".to_owned()))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| rejected("HTTP Contract response status is invalid".to_owned()))?;
    let body = response.split_off(boundary + 4);
    if body.len() > MAX_HTTP_BODY_BYTES {
        return Err(rejected(
            "HTTP Contract response body exceeds 8 MiB".to_owned(),
        ));
    }
    Ok((status, body))
}

#[derive(Default)]
pub struct WorkspaceContentVerifier;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceContentPayload {
    path: String,
    content_ref: String,
}

impl ContractVerifier for WorkspaceContentVerifier {
    fn id(&self) -> &str {
        WORKSPACE_CONTENT_VERIFIER_ID
    }

    fn verify(
        &self,
        descriptor: &ContractDescriptor,
        candidate: &mut dyn Realization,
        context: &ContractContext<'_>,
    ) -> Result<ContractResult, MaterializerError> {
        let payload: WorkspaceContentPayload = serde_json::from_value(descriptor.payload.clone())?;
        let path = safe_workspace_path(context.workspace, &payload.path)?;
        let metadata = fs::metadata(&path)
            .map_err(|error| rejected(format!("workspace Contract stat failed: {error}")))?;
        if !metadata.is_file() || metadata.len() > MAX_WORKSPACE_FILE_BYTES as u64 {
            return Err(rejected(
                "workspace Contract target must be a file of at most 64 MiB".to_owned(),
            ));
        }
        let bytes = fs::read(&path)
            .map_err(|error| rejected(format!("workspace Contract read failed: {error}")))?;
        let reference = ContentRef::parse(payload.content_ref)
            .map_err(|error| rejected(format!("invalid workspace content reference: {error}")))?;
        verify_content(&reference, &bytes)
            .map_err(|error| rejected(format!("workspace content digest mismatch: {error}")))?;
        Ok(ContractResult {
            verifier_id: descriptor.verifier_id.clone(),
            target: candidate.target().clone(),
            summary: format!("workspace {} PASS", payload.path),
        })
    }
}

fn safe_workspace_path(
    root: &Path,
    relative: &str,
) -> Result<std::path::PathBuf, MaterializerError> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(rejected(
            "workspace Contract path must be a non-empty normalized relative path".to_owned(),
        ));
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| rejected(format!("workspace root canonicalization failed: {error}")))?;
    let candidate = root.join(relative);
    let canonical_candidate = candidate
        .canonicalize()
        .map_err(|error| rejected(format!("workspace path canonicalization failed: {error}")))?;
    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(rejected(
            "workspace Contract path escapes the workspace boundary".to_owned(),
        ));
    }
    Ok(canonical_candidate)
}

fn rejected(reason: String) -> MaterializerError {
    MaterializerError::Operation(reason)
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::thread;

    use ato_computation::ComputationRef;
    use ato_materializer_api::{ContractVerifierRegistry, Realization, accept_candidate};
    use ato_objects::{MemoryObjectStore, blake3_reference};

    use super::*;

    struct Candidate(ComputationRef);

    impl Realization for Candidate {
        fn target(&self) -> &ComputationRef {
            &self.0
        }
        fn activate(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
        fn publish(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
        fn wait(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
        fn quiesce(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
    }

    fn computation() -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", "b".repeat(64))).unwrap()
    }

    #[test]
    fn workspace_verifier_checks_digest_without_path_escape() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("state.txt"), b"ready").unwrap();
        let contract = ContractDescriptor::new(
            WORKSPACE_CONTENT_VERIFIER_ID,
            serde_json::json!({
                "path": "state.txt",
                "content_ref": blake3_reference(b"ready").to_string()
            }),
        )
        .unwrap();
        let mut registry = ContractVerifierRegistry::default();
        registry
            .register(Arc::new(WorkspaceContentVerifier))
            .unwrap();
        let objects = MemoryObjectStore::default();
        let context = ContractContext {
            objects: &objects,
            workspace: workspace.path(),
        };

        let accepted = accept_candidate(
            Box::new(Candidate(computation())),
            &[contract],
            &registry,
            &context,
        )
        .unwrap();

        assert_eq!(
            accepted.contract_results()[0].verifier_id,
            WORKSPACE_CONTENT_VERIFIER_ID
        );
        assert!(safe_workspace_path(workspace.path(), "../outside").is_err());
    }

    #[test]
    fn http_verifier_rejects_non_loopback_targets_before_connect() {
        let payload = HttpEndpointPayload {
            address: "192.0.2.1:80".parse().unwrap(),
            path: "/ready".to_owned(),
            expected_status: 200,
            expected_body_ref: None,
        };

        assert!(validate_internal_http_target(&payload).is_err());
    }

    #[test]
    fn http_verifier_accepts_only_internal_matching_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let contract = ContractDescriptor::new(
            HTTP_ENDPOINT_VERIFIER_ID,
            serde_json::json!({
                "address": address,
                "path": "/ready",
                "expected_status": 204
            }),
        )
        .unwrap();
        let mut registry = ContractVerifierRegistry::default();
        registry.register(Arc::new(HttpEndpointVerifier)).unwrap();
        let objects = MemoryObjectStore::default();
        let workspace = tempfile::tempdir().unwrap();
        let context = ContractContext {
            objects: &objects,
            workspace: workspace.path(),
        };

        let accepted = accept_candidate(
            Box::new(Candidate(computation())),
            &[contract],
            &registry,
            &context,
        )
        .unwrap();

        assert_eq!(
            accepted.contract_results()[0].verifier_id,
            HTTP_ENDPOINT_VERIFIER_ID
        );
        server.join().unwrap();
    }
}
