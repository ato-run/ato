use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_computation::ContentRef;
use ato_objects::{
    GraphMaterialization, GraphObjectDescriptor, ObjectGraphClosure, ObjectResolver,
    read_exact_object,
};
use clap::ValueEnum;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderName, HeaderValue, ORIGIN};
use serde::{Deserialize, Serialize};

const MAX_UPLOAD_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VisibilityPolicy {
    Private,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportedPort {
    pub port_id: String,
    pub protocol: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequiredBinding {
    pub id: String,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectGraphIndexV1 {
    pub version: u32,
    pub root_computation_ref: String,
    pub objects: Vec<GraphObjectDescriptor>,
    pub materializations: Vec<GraphMaterialization>,
    pub exported_ports: Vec<ExportedPort>,
    pub required_bindings: Vec<RequiredBinding>,
    pub visibility_policy: VisibilityPolicy,
}

impl ObjectGraphIndexV1 {
    pub(crate) fn new(
        closure: ObjectGraphClosure,
        exported_ports: Vec<ExportedPort>,
        required_bindings: Vec<RequiredBinding>,
        visibility_policy: VisibilityPolicy,
    ) -> Self {
        Self {
            version: 1,
            root_computation_ref: closure.root_computation_ref,
            objects: closure.objects,
            materializations: closure.materializations,
            exported_ports,
            required_bindings,
            visibility_policy,
        }
    }

    pub(crate) fn digest(&self) -> Result<String> {
        Ok(format!(
            "blake3:{}",
            blake3::hash(&serde_jcs::to_vec(self)?).to_hex()
        ))
    }

    fn logical_bytes(&self) -> Result<u64> {
        self.objects.iter().try_fold(0_u64, |total, object| {
            total
                .checked_add(object.size_bytes)
                .context("object graph logical byte count overflow")
        })
    }
}

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
    unique_stored_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusEnvelope {
    object_graph: GraphResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObjectUploadReceipt {
    pub version: u32,
    pub graph_id: String,
    pub root_computation_ref: String,
    pub bundle_index_digest: String,
    pub bundle_id: String,
    pub object_count: usize,
    pub logical_bytes: u64,
    pub objects_uploaded: usize,
    pub uploaded_bytes: u64,
    pub objects_stored_new: usize,
    pub unique_stored_bytes: u64,
    pub object_digests: Vec<String>,
    pub validation_status: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct UploadConfig {
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

pub(crate) struct HttpObjectTransportApi {
    client: Client,
    base_url: String,
    token: String,
    origin: HeaderValue,
}

impl HttpObjectTransportApi {
    pub(crate) fn new(base_url: &str, token: String) -> Result<Self> {
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
                .timeout(Duration::from_secs(180))
                .build()
                .context("failed to construct object transport HTTP client")?,
            base_url,
            token,
            origin,
        })
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
            ApiError::retryable(format!("object transport request failed: {error}"))
        })
    }
}

impl ObjectTransportApi for HttpObjectTransportApi {
    fn prepare(&self, request: &PrepareRequest<'_>) -> Result<GraphResponse, ApiError> {
        let response = self
            .send(self.authenticated(self.client.post(self.graph_url("/prepare")).json(request)))?;
        Self::response(response)
    }

    fn upload(&self, instruction: &UploadInstruction, bytes: Vec<u8>) -> Result<(), ApiError> {
        let mut request = self.client.put(&instruction.upload_url);
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
        if !instruction.upload_direct {
            request = self.authenticated(request);
        }
        let response = self.send(request.body(bytes))?;
        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let retryable =
                status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error();
            let message = format!("object PUT returned {status}");
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
                "object graph validation rejected: {:?}",
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
        object_count: index.objects.len(),
        logical_bytes: index.logical_bytes()?,
        objects_uploaded: uploaded_objects.load(Ordering::Acquire),
        uploaded_bytes: uploaded_bytes.load(Ordering::Acquire),
        objects_stored_new: finalized
            .objects_stored_new
            .context("ready response omitted objects_stored_new accounting")?,
        unique_stored_bytes: finalized
            .unique_stored_bytes
            .context("ready response omitted unique_stored_bytes accounting")?,
        object_digests: index
            .objects
            .iter()
            .map(|object| object.content_ref.clone())
            .collect(),
        validation_status: finalized.status,
    })
}

pub(crate) fn upload_http_object_graph(
    api: &HttpObjectTransportApi,
    index: &ObjectGraphIndexV1,
    objects: &dyn ObjectResolver,
    idempotency_key: &str,
    config: UploadConfig,
) -> Result<ObjectUploadReceipt> {
    upload_object_graph(api, index, objects, idempotency_key, config)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ato_objects::{GraphObjectKind, MemoryObjectStore, ObjectStore};

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
}
