use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use ato_computation::ContentRef;
use ato_objects::{ObjectResolver, ReferenceRegistry, read_exact_object};
pub use ato_runtime_object_graph::{
    ExportedPort, ObjectGraphIndexV1, RequiredBinding, VisibilityPolicy,
};
use ato_runtime_object_graph::{validate_runtime_object_graph, vm_capture_refs};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderName, HeaderValue, ORIGIN};
use serde::{Deserialize, Serialize};

const MAX_UPLOAD_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct PrepareRequest<'a> {
    idempotency_key: &'a str,
    index_digest: &'a str,
    index: &'a ObjectGraphIndexV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadInstruction {
    content_ref: String,
    size_bytes: u64,
    upload_url: String,
    upload_direct: bool,
    upload_headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphResponse {
    graph_id: String,
    root_computation_ref: String,
    bundle_index_digest: String,
    visibility_policy: VisibilityPolicy,
    status: String,
    object_count: usize,
    #[serde(default)]
    declared_object_count: Option<usize>,
    logical_bytes: u64,
    bundle_id: Option<String>,
    rejection_code: Option<String>,
    expires_at: String,
    validated_at: Option<String>,
    #[serde(default)]
    uploads: Vec<UploadInstruction>,
    #[serde(default)]
    objects_uploaded: Option<usize>,
    #[serde(default)]
    objects_stored_new: Option<usize>,
    #[serde(default)]
    objects_reused: Option<usize>,
    #[serde(default)]
    unique_stored_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusEnvelope {
    object_graph: GraphResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectUploadReceipt {
    pub version: u32,
    pub graph_id: String,
    pub root_computation_ref: String,
    pub bundle_index_digest: String,
    pub bundle_id: String,
    /// Exact number of objects declared by the canonical graph index.
    pub declared_object_count: usize,
    pub object_count: usize,
    pub logical_bytes: u64,
    /// Actual PUT calls made by this client session.
    pub client_put_count: usize,
    /// Compatibility field; same meaning as `client_put_count` in client receipts.
    pub objects_uploaded: usize,
    pub uploaded_bytes: u64,
    pub objects_stored_new: usize,
    pub objects_reused: usize,
    pub unique_stored_bytes: u64,
    pub object_digests: Vec<String>,
    pub validation_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm_materialization_descriptor_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_frontier_ref: Option<String>,
}

pub fn vm_capture_receipt_refs(
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
) -> Result<(Option<String>, Option<String>)> {
    Ok(
        vm_capture_refs(index, objects)?.map_or((None, None), |(descriptor, frontier)| {
            (Some(descriptor.to_string()), Some(frontier.to_string()))
        }),
    )
}

#[derive(Debug, Clone, Copy)]
pub struct UploadConfig {
    pub concurrency: usize,
    pub retry_attempts: usize,
    pub validation_poll_attempts: usize,
    pub validation_poll_interval: Duration,
}

#[derive(Debug)]
struct ApiError {
    message: String,
    retryable: bool,
}

impl ApiError {
    fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }

    fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

trait ObjectTransportApi: Sync {
    fn prepare(&self, request: &PrepareRequest<'_>) -> Result<GraphResponse, ApiError>;
    fn upload(&self, instruction: &UploadInstruction, bytes: Vec<u8>) -> Result<(), ApiError>;
    fn finalize(&self, graph_id: &str) -> Result<GraphResponse, ApiError>;
    fn status(&self, graph_id: &str) -> Result<GraphResponse, ApiError>;
}

pub struct HttpObjectTransportApi {
    client: Client,
    base_url: String,
    token: String,
    origin: HeaderValue,
    prepared_graph_id: Mutex<Option<String>>,
    staging_proxy_upload: bool,
}

impl HttpObjectTransportApi {
    pub fn new(base_url: &str, token: String) -> Result<Self> {
        let parsed = reqwest::Url::parse(base_url).context("invalid object transport API URL")?;
        if parsed.scheme() != "https" && parsed.host_str() != Some("localhost") {
            bail!("object transport API must use HTTPS (except localhost)");
        }
        if parsed.query().is_some() || parsed.fragment().is_some() {
            bail!("object transport API URL cannot contain a query or fragment");
        }
        if parsed.path() != "/" && !parsed.path().is_empty() {
            bail!("object transport API URL cannot contain a path");
        }
        if token.trim().is_empty() {
            bail!("object transport API token cannot be empty");
        }
        let origin = HeaderValue::from_str(&parsed.origin().ascii_serialization())
            .context("invalid object transport API origin")?;
        let base_url = base_url.trim_end_matches('/').to_owned();
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(900))
                .build()
                .context("failed to construct object transport HTTP client")?,
            base_url,
            token,
            origin,
            prepared_graph_id: Mutex::new(None),
            staging_proxy_upload: false,
        })
    }

    /// Route object PUTs through the authenticated staging API fallback when
    /// direct R2 presigned PUTs are unavailable from the staging runner.
    pub fn with_staging_proxy_upload(mut self) -> Result<Self> {
        ensure!(
            self.base_url == "https://staging.api.ato.run",
            "proxy upload override is restricted to the staging API"
        );
        self.staging_proxy_upload = true;
        Ok(self)
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .bearer_auth(&self.token)
            .header(ORIGIN, self.origin.clone())
    }

    fn graph_url(&self, suffix: &str) -> String {
        format!("{}/v1/capsule-object-graphs{}", self.base_url, suffix)
    }

    fn response<T: for<'de> Deserialize<'de>>(
        response: reqwest::blocking::Response,
    ) -> Result<T, ApiError> {
        let status = response.status();
        if status.is_success() {
            return response
                .json()
                .map_err(|error| ApiError::terminal(format!("invalid API response: {error}")));
        }
        let retryable =
            status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
        let body = response.text().unwrap_or_default();
        let body = body.chars().take(512).collect::<String>();
        let message = format!("object transport API returned {status}: {body}");
        Err(if retryable {
            ApiError::retryable(message)
        } else {
            ApiError::terminal(message)
        })
    }

    fn send(&self, request: RequestBuilder) -> Result<reqwest::blocking::Response, ApiError> {
        request.send().map_err(|error| {
            ApiError::retryable(if error.is_timeout() {
                "object transport request timed out"
            } else {
                "object transport request failed"
            })
        })
    }

    /// Remove an owned non-ready graph and return the number of tenant CAS
    /// objects deleted by the server's graph-aware garbage collector.
    pub fn delete_rejected_graph(&self, graph_id: &str) -> Result<u64> {
        #[derive(Deserialize)]
        struct DeleteResponse {
            objects_deleted: u64,
        }

        ensure!(
            !graph_id.is_empty()
                && graph_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'),
            "invalid object graph id"
        );
        let response = self
            .send(self.authenticated(self.client.delete(self.graph_url(&format!("/{graph_id}")))))
            .map_err(anyhow::Error::from)?;
        let deleted: DeleteResponse = Self::response(response).map_err(anyhow::Error::from)?;
        Ok(deleted.objects_deleted)
    }
}

impl ObjectTransportApi for HttpObjectTransportApi {
    fn prepare(&self, request: &PrepareRequest<'_>) -> Result<GraphResponse, ApiError> {
        let response = self
            .send(self.authenticated(self.client.post(self.graph_url("/prepare")).json(request)))?;
        let graph: GraphResponse = Self::response(response)?;
        *self
            .prepared_graph_id
            .lock()
            .map_err(|_| ApiError::terminal("object transport graph lock is poisoned"))? =
            Some(graph.graph_id.clone());
        Ok(graph)
    }

    fn upload(&self, instruction: &UploadInstruction, bytes: Vec<u8>) -> Result<(), ApiError> {
        let proxy_upload = instruction.upload_direct && self.staging_proxy_upload;
        let upload_url = if proxy_upload {
            let graph_id = self
                .prepared_graph_id
                .lock()
                .map_err(|_| ApiError::terminal("object transport graph lock is poisoned"))?
                .clone()
                .ok_or_else(|| ApiError::terminal("object transport graph was not prepared"))?;
            self.graph_url(&format!("/{graph_id}/objects/{}", instruction.content_ref))
        } else {
            instruction.upload_url.clone()
        };
        let mut request = self.client.put(upload_url);
        for (name, value) in &instruction.upload_headers {
            let lower = name.to_ascii_lowercase();
            if lower != "content-type" && !lower.starts_with("x-amz-meta-") {
                return Err(ApiError::terminal(format!(
                    "server requested forbidden upload header `{name}`"
                )));
            }
            let name = HeaderName::from_bytes(lower.as_bytes())
                .map_err(|_| ApiError::terminal("server returned an invalid upload header"))?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                ApiError::terminal("server returned an invalid upload header value")
            })?;
            request = request.header(name, value);
        }
        if !instruction.upload_direct || proxy_upload {
            request = self.authenticated(request);
        }
        let response = self.send(request.body(bytes))?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let retryable =
                status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
            let body = response.text().unwrap_or_default();
            let r2_code = body
                .split_once("<Code>")
                .and_then(|(_, suffix)| suffix.split_once("</Code>"))
                .map(|(code, _)| code)
                .filter(|code| {
                    !code.is_empty()
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                })
                .unwrap_or("unknown");
            let message = format!("object PUT returned {status} (r2_code={r2_code})");
            Err(if retryable {
                ApiError::retryable(message)
            } else {
                ApiError::terminal(message)
            })
        }
    }

    fn finalize(&self, graph_id: &str) -> Result<GraphResponse, ApiError> {
        let response = self.send(
            self.authenticated(
                self.client
                    .post(self.graph_url(&format!("/{graph_id}/finalize"))),
            ),
        )?;
        Self::response(response)
    }

    fn status(&self, graph_id: &str) -> Result<GraphResponse, ApiError> {
        let response = self
            .send(self.authenticated(self.client.get(self.graph_url(&format!("/{graph_id}")))))?;
        Self::response::<StatusEnvelope>(response).map(|envelope| envelope.object_graph)
    }
}

fn retry<T>(attempts: usize, mut operation: impl FnMut() -> Result<T, ApiError>) -> Result<T> {
    let attempts = attempts.max(1);
    for attempt in 1..=attempts {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if error.retryable && attempt < attempts => {
                std::thread::sleep(Duration::from_millis(100 * attempt as u64));
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("retry loop has at least one attempt")
}

fn validate_prepare(
    response: &GraphResponse,
    index: &ObjectGraphIndexV1,
    index_digest: &str,
) -> Result<()> {
    if response.root_computation_ref != index.root_computation_ref
        || response.bundle_index_digest != index_digest
        || response.object_count != index.objects.len()
        || response
            .declared_object_count
            .is_some_and(|count| count != index.objects.len())
        || response.logical_bytes != index.logical_bytes()?
    {
        bail!("prepare response does not match the submitted object graph");
    }
    if response.status == "ready" {
        if !response.uploads.is_empty() || response.bundle_id.is_none() {
            bail!("ready idempotency response has an invalid upload shape");
        }
        return Ok(());
    }
    if response.status != "uploading" {
        bail!(
            "object graph prepare returned terminal state `{}` ({:?})",
            response.status,
            response.rejection_code
        );
    }
    let expected = index
        .objects
        .iter()
        .map(|object| (object.content_ref.as_str(), object.size_bytes))
        .collect::<BTreeMap<_, _>>();
    let mut actual = BTreeMap::new();
    for instruction in &response.uploads {
        if actual
            .insert(instruction.content_ref.as_str(), instruction.size_bytes)
            .is_some()
        {
            bail!("prepare returned duplicate upload instruction");
        }
    }
    if expected != actual {
        bail!("prepare upload instructions do not equal the declared closure");
    }
    Ok(())
}

fn upload_object_graph(
    api: &dyn ObjectTransportApi,
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
    idempotency_key: &str,
    config: UploadConfig,
) -> Result<ObjectUploadReceipt> {
    if config.concurrency == 0 || config.concurrency > 32 {
        bail!("upload concurrency must be between 1 and 32");
    }
    let index_digest = index.digest()?;
    let prepared = retry(config.retry_attempts, || {
        api.prepare(&PrepareRequest {
            idempotency_key,
            index_digest: &index_digest,
            index,
        })
    })?;
    validate_prepare(&prepared, index, &index_digest)?;

    let uploaded_objects = AtomicUsize::new(0);
    let uploaded_bytes = AtomicU64::new(0);
    if prepared.status == "uploading" {
        let instructions = Arc::new(prepared.uploads.clone());
        let next = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let failure = Mutex::new(None::<String>);
        std::thread::scope(|scope| {
            for _ in 0..config.concurrency.min(instructions.len()) {
                let instructions = Arc::clone(&instructions);
                let next = &next;
                let cancelled = &cancelled;
                let failure = &failure;
                let uploaded_objects = &uploaded_objects;
                let uploaded_bytes = &uploaded_bytes;
                scope.spawn(move || {
                    loop {
                        if cancelled.load(Ordering::Acquire) {
                            return;
                        }
                        let index = next.fetch_add(1, Ordering::AcqRel);
                        let Some(instruction) = instructions.get(index) else {
                            return;
                        };
                        let result = (|| -> Result<()> {
                            let reference = ContentRef::parse(&instruction.content_ref)?;
                            let bytes = read_exact_object(
                                objects,
                                &reference,
                                instruction.size_bytes,
                                MAX_UPLOAD_OBJECT_BYTES,
                            )?;
                            retry(config.retry_attempts, || {
                                api.upload(instruction, bytes.clone())
                            })?;
                            uploaded_objects.fetch_add(1, Ordering::AcqRel);
                            uploaded_bytes.fetch_add(instruction.size_bytes, Ordering::AcqRel);
                            Ok(())
                        })();
                        if let Err(error) = result {
                            cancelled.store(true, Ordering::Release);
                            *failure.lock().expect("upload failure lock poisoned") =
                                Some(format!("{}: {error:#}", instruction.content_ref));
                            return;
                        }
                    }
                });
            }
        });
        if let Some(error) = failure.into_inner().expect("upload failure lock poisoned") {
            bail!("object upload failed for {error}");
        }
    }

    let graph_id = prepared.graph_id.clone();
    let mut finalized = if prepared.status == "ready" {
        prepared
    } else {
        retry(config.retry_attempts, || api.finalize(&graph_id))?
    };
    for attempt in 0..config.validation_poll_attempts.max(1) {
        match finalized.status.as_str() {
            "ready" => break,
            "rejected" => bail!(
                "object graph {} validation rejected: {:?}",
                finalized.graph_id,
                finalized.rejection_code
            ),
            "validating" if attempt + 1 < config.validation_poll_attempts.max(1) => {
                std::thread::sleep(config.validation_poll_interval);
                finalized = retry(config.retry_attempts, || api.status(&graph_id))?;
            }
            "validating" => bail!("object graph validation did not finish before the poll limit"),
            state => bail!("object graph entered unexpected state `{state}`"),
        }
    }
    let bundle_id = finalized
        .bundle_id
        .context("ready object graph response omitted bundle_id")?;
    Ok(ObjectUploadReceipt {
        version: 1,
        graph_id: finalized.graph_id,
        root_computation_ref: finalized.root_computation_ref,
        bundle_index_digest: index_digest,
        bundle_id,
        declared_object_count: index.objects.len(),
        object_count: index.objects.len(),
        logical_bytes: index.logical_bytes()?,
        client_put_count: uploaded_objects.load(Ordering::Acquire),
        objects_uploaded: uploaded_objects.load(Ordering::Acquire),
        uploaded_bytes: uploaded_bytes.load(Ordering::Acquire),
        objects_stored_new: finalized
            .objects_stored_new
            .context("ready response omitted objects_stored_new accounting")?,
        objects_reused: finalized.objects_reused.unwrap_or_else(|| {
            index
                .objects
                .len()
                .saturating_sub(finalized.objects_stored_new.unwrap_or(0))
        }),
        unique_stored_bytes: finalized
            .unique_stored_bytes
            .context("ready response omitted unique_stored_bytes accounting")?,
        object_digests: index
            .objects
            .iter()
            .map(|object| object.content_ref.clone())
            .collect(),
        validation_status: finalized.status,
        vm_materialization_descriptor_ref: None,
        record_frontier_ref: None,
    })
}

pub fn upload_http_object_graph(
    api: &HttpObjectTransportApi,
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
    references: &ReferenceRegistry,
    idempotency_key: &str,
    config: UploadConfig,
) -> Result<ObjectUploadReceipt> {
    validate_runtime_object_graph(index, objects, references)?;
    upload_object_graph(api, index, objects, idempotency_key, config)
}

/// Staging-only negative acceptance hook. It deliberately bypasses the local
/// semantic validator so the independently deployed Validator Agent receives
/// a malformed private graph. Production hosts and public graphs are refused.
pub fn upload_staging_negative_test_object_graph(
    api: &HttpObjectTransportApi,
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
    idempotency_key: &str,
    config: UploadConfig,
) -> Result<ObjectUploadReceipt> {
    ensure!(
        api.base_url == "https://staging.api.ato.run",
        "negative validator test is restricted to the staging API"
    );
    ensure!(
        index.visibility_policy == VisibilityPolicy::Private,
        "negative validator test graph must be private"
    );
    ensure!(
        idempotency_key.starts_with("staging-negative-validator-"),
        "negative validator test idempotency key is invalid"
    );
    upload_object_graph(api, index, objects, idempotency_key, config)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ato_materializer_vm_snapshot::VM_SNAPSHOT_MATERIALIZER_ID;
    use ato_objects::{
        GraphMaterialization, GraphObjectDescriptor, GraphObjectKind, MemoryObjectStore,
        ObjectStore,
    };

    use super::*;

    struct FakeApi {
        prepared: GraphResponse,
        active: AtomicUsize,
        maximum_active: AtomicUsize,
        attempts: Mutex<BTreeMap<String, usize>>,
        polls: AtomicUsize,
    }

    impl FakeApi {
        fn new(index: &ObjectGraphIndexV1) -> Self {
            let digest = index.digest().unwrap();
            Self {
                prepared: GraphResponse {
                    graph_id: "cog_01M0TEST000000000000000000".to_owned(),
                    root_computation_ref: index.root_computation_ref.clone(),
                    bundle_index_digest: digest,
                    visibility_policy: VisibilityPolicy::Private,
                    status: "uploading".to_owned(),
                    object_count: index.objects.len(),
                    declared_object_count: Some(index.objects.len()),
                    logical_bytes: index.logical_bytes().unwrap(),
                    bundle_id: None,
                    rejection_code: None,
                    expires_at: "later".to_owned(),
                    validated_at: None,
                    uploads: index
                        .objects
                        .iter()
                        .map(|object| UploadInstruction {
                            content_ref: object.content_ref.clone(),
                            size_bytes: object.size_bytes,
                            upload_url: format!("https://upload.test/{}", object.content_ref),
                            upload_direct: true,
                            upload_headers: BTreeMap::new(),
                        })
                        .collect(),
                    objects_uploaded: None,
                    objects_stored_new: None,
                    objects_reused: None,
                    unique_stored_bytes: None,
                },
                active: AtomicUsize::new(0),
                maximum_active: AtomicUsize::new(0),
                attempts: Mutex::new(BTreeMap::new()),
                polls: AtomicUsize::new(0),
            }
        }
    }

    impl ObjectTransportApi for FakeApi {
        fn prepare(&self, _request: &PrepareRequest<'_>) -> Result<GraphResponse, ApiError> {
            Ok(self.prepared.clone())
        }

        fn upload(&self, instruction: &UploadInstruction, _bytes: Vec<u8>) -> Result<(), ApiError> {
            let active = self.active.fetch_add(1, Ordering::AcqRel) + 1;
            self.maximum_active.fetch_max(active, Ordering::AcqRel);
            std::thread::sleep(Duration::from_millis(5));
            self.active.fetch_sub(1, Ordering::AcqRel);
            let mut attempts = self.attempts.lock().unwrap();
            let count = attempts.entry(instruction.content_ref.clone()).or_default();
            *count += 1;
            if instruction == &self.prepared.uploads[0] && *count == 1 {
                return Err(ApiError::retryable("transient PUT"));
            }
            Ok(())
        }

        fn finalize(&self, _graph_id: &str) -> Result<GraphResponse, ApiError> {
            let mut response = self.prepared.clone();
            response.status = "validating".to_owned();
            response.uploads.clear();
            Ok(response)
        }

        fn status(&self, _graph_id: &str) -> Result<GraphResponse, ApiError> {
            self.polls.fetch_add(1, Ordering::AcqRel);
            let mut response = self.prepared.clone();
            response.status = "ready".to_owned();
            response.uploads.clear();
            response.bundle_id = Some("bnd_01M0TEST000000000000000000".to_owned());
            response.objects_stored_new = Some(response.object_count);
            response.unique_stored_bytes = Some(response.logical_bytes);
            Ok(response)
        }
    }

    fn fixture() -> (MemoryObjectStore, ObjectGraphIndexV1) {
        let objects = MemoryObjectStore::default();
        let first = objects.put(b"root").unwrap();
        let second = objects.put(b"artifact").unwrap();
        (
            objects,
            ObjectGraphIndexV1 {
                version: 1,
                root_computation_ref: first.to_string(),
                objects: vec![
                    GraphObjectDescriptor {
                        content_ref: first.to_string(),
                        size_bytes: 4,
                        kind: GraphObjectKind::Computation,
                        references: vec![second.to_string()],
                    },
                    GraphObjectDescriptor {
                        content_ref: second.to_string(),
                        size_bytes: 8,
                        kind: GraphObjectKind::Payload,
                        references: Vec::new(),
                    },
                ],
                materializations: Vec::new(),
                exported_ports: Vec::new(),
                required_bindings: Vec::new(),
                visibility_policy: VisibilityPolicy::Private,
            },
        )
    }

    #[test]
    fn uploads_with_bounded_concurrency_retry_and_validation_polling() {
        let (objects, index) = fixture();
        let api = FakeApi::new(&index);
        let receipt = upload_object_graph(
            &api,
            &index,
            &objects,
            "idempotency-key-for-test",
            UploadConfig {
                concurrency: 2,
                retry_attempts: 2,
                validation_poll_attempts: 2,
                validation_poll_interval: Duration::ZERO,
            },
        )
        .unwrap();

        assert_eq!(api.maximum_active.load(Ordering::Acquire), 2);
        assert_eq!(receipt.objects_uploaded, 2);
        assert_eq!(receipt.uploaded_bytes, 12);
        assert_eq!(receipt.objects_stored_new, 2);
        assert_eq!(receipt.unique_stored_bytes, 12);
        assert_eq!(api.polls.load(Ordering::Acquire), 1);
        assert_eq!(
            api.attempts
                .lock()
                .unwrap()
                .get(&index.objects[0].content_ref),
            Some(&2)
        );
    }

    #[test]
    fn rejects_an_upload_instruction_outside_the_declared_closure() {
        let (objects, index) = fixture();
        let mut api = FakeApi::new(&index);
        api.prepared.uploads.pop();
        let error = upload_object_graph(
            &api,
            &index,
            &objects,
            "idempotency-key-for-test",
            UploadConfig {
                concurrency: 1,
                retry_attempts: 1,
                validation_poll_attempts: 1,
                validation_poll_interval: Duration::ZERO,
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("declared closure"));
        assert!(api.attempts.lock().unwrap().is_empty());
    }

    #[test]
    fn proxy_upload_override_is_staging_only() {
        let production = HttpObjectTransportApi::new(
            "https://api.ato.run",
            "ato_dev_test-only-placeholder".to_owned(),
        )
        .unwrap();
        assert!(production.with_staging_proxy_upload().is_err());

        let staging = HttpObjectTransportApi::new(
            "https://staging.api.ato.run",
            "ato_dev_test-only-placeholder".to_owned(),
        )
        .unwrap();
        assert!(staging.with_staging_proxy_upload().is_ok());
    }

    #[test]
    fn derives_vm_descriptor_and_record_frontier_refs_without_changing_root() {
        let objects = MemoryObjectStore::default();
        let root = objects.put(b"known-computation").unwrap();
        let frontier = objects.put(b"sealed-frontier").unwrap();
        let descriptor_bytes = serde_jcs::to_vec(&serde_json::json!({
            "version": 1,
            "target_computation_ref": root.to_string(),
            "record_frontier_ref": frontier.to_string(),
            "backend": "firecracker",
            "snapshot_format": "fc-vmstate-v1",
            "architecture": "x86_64",
            "guest_os": "linux",
            "host_backend_contract": {
                "backend_id": "firecracker", "host_os": "linux", "required_features": []
            },
            "cpu_contract": { "vcpu_count": 1, "required_features": [] },
            "firecracker_version": "1.7.0",
            "device_contract": { "required_features": [] },
            "network_contract": { "required_features": [], "tap_device": null },
            "vsock_contract": { "required_features": [], "uds_path": null },
            "memory_contract": { "guest_memory_mib": 128, "minimum_host_memory_mib": 256 },
            "artifacts": [],
            "state_contract_refs": [],
            "contracts": [],
            "capture_provenance": {
                "captured_at": "2026-08-23T00:00:00Z",
                "backend_implementation_id": "test-firecracker",
                "source_realization_id": "realization-test",
                "capture_barrier_complete": true,
                "realization_quiesced": true,
                "placement_hint": null
            }
        }))
        .unwrap();
        let descriptor = objects.put(&descriptor_bytes).unwrap();
        let index = ObjectGraphIndexV1 {
            version: 1,
            root_computation_ref: root.to_string(),
            objects: vec![
                GraphObjectDescriptor {
                    content_ref: root.to_string(),
                    size_bytes: b"known-computation".len() as u64,
                    kind: GraphObjectKind::Computation,
                    references: vec![descriptor.to_string()],
                },
                GraphObjectDescriptor {
                    content_ref: descriptor.to_string(),
                    size_bytes: descriptor_bytes.len() as u64,
                    kind: GraphObjectKind::Materialization,
                    references: Vec::new(),
                },
            ],
            materializations: vec![GraphMaterialization {
                id: VM_SNAPSHOT_MATERIALIZER_ID.to_owned(),
                descriptor_ref: descriptor.to_string(),
                restore_capability: ato_objects::GraphRestoreCapability::Supported,
            }],
            exported_ports: Vec::new(),
            required_bindings: Vec::new(),
            visibility_policy: VisibilityPolicy::Private,
        };

        let (descriptor_ref, frontier_ref) = vm_capture_receipt_refs(&index, &objects).unwrap();
        assert_eq!(descriptor_ref, Some(descriptor.to_string()));
        assert_eq!(frontier_ref, Some(frontier.to_string()));
        assert_eq!(index.root_computation_ref, root.to_string());
    }
}
