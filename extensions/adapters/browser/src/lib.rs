//! Browser input is an ordinary Protocol boundary; replay remains generic.

#![forbid(unsafe_code)]

mod coalescer;
mod protocol;
mod transport;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, LiveOperation, Stylus, SupportedOperation,
};
use ato_objects::{RecordCandidate, RecordEnvelope, read_exact_object};
use ato_record_writer::RecordSchemaRegistry;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use url::Url;

pub use protocol::{
    BrowserEvent, BrowserProtocolError, KeyboardKind, Modifiers, PointerKind, PointerType,
    decode_event, encode_event,
};

pub const BROWSER_ADAPTER_ID: &str = "ato.browser@1";
pub const BROWSER_PROTOCOL_ID: &str = "ato.browser@1";
pub const BROWSER_KEYBOARD_OPERATION: &str = "keyboard";
pub const BROWSER_POINTER_OPERATION: &str = "pointer";
pub const BROWSER_CLICK_OPERATION: &str = "click";
pub const BROWSER_SCROLL_OPERATION: &str = "scroll";

const MAX_BROWSER_EVENT_BYTES: u64 = 64 * 1024;
const ACK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserAdapterConfig {
    pub port_id: String,
    pub expected_origin: String,
    #[serde(default)]
    pub allowed_non_text_codes: BTreeSet<String>,
}

#[derive(Default)]
pub struct BrowserAdapter;

/// Converts physical Browser input into portable operation Records.
///
/// The wrapped Stylus is the non-persisting submission boundary. Presentation
/// output (screenshots, DOM, console, and media) never enters this component.
pub struct BrowserStylus {
    inner: Arc<dyn Stylus>,
    port_id: ato_computation::PortId,
    stream: String,
    recorded_by: String,
    allowed_non_text_codes: BTreeSet<String>,
    local_seq: AtomicU64,
}

impl BrowserStylus {
    pub fn new(
        inner: Arc<dyn Stylus>,
        port_id: ato_computation::PortId,
        stream: impl Into<String>,
        recorded_by: impl Into<String>,
        allowed_non_text_codes: BTreeSet<String>,
    ) -> Result<Self, AdapterError> {
        let stream = stream.into();
        if stream.is_empty() {
            return Err(AdapterError::InvalidConfig(
                "Browser Record stream must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            inner,
            port_id,
            stream,
            recorded_by: recorded_by.into(),
            allowed_non_text_codes,
            local_seq: AtomicU64::new(0),
        })
    }

    pub fn record(&self, event: &BrowserEvent) -> Result<(), AdapterError> {
        let payload = protocol::encode_event_with_policy(event, &self.allowed_non_text_codes)
            .map_err(|error| AdapterError::InvalidPayload(error.to_string()))?;
        let local_seq = self.local_seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner.record(RecordCandidate {
            protocol_id: ato_computation::ProtocolId::parse(BROWSER_PROTOCOL_ID)
                .expect("valid static Browser Protocol ID"),
            operation_id: ato_computation::OperationId::parse(operation_for_event(event))
                .expect("valid static Browser operation ID"),
            port_id: self.port_id.clone(),
            payload,
            payload_version: 1,
            required_features: BTreeSet::new(),
            recorded_by: Some(self.recorded_by.clone()),
            stream: self.stream.clone(),
            local_seq,
            caused_by: Vec::new(),
            observed_at: observed_now(),
        })
    }
}

pub fn operation_for_event(event: &BrowserEvent) -> &'static str {
    match event {
        BrowserEvent::Keyboard { .. } => BROWSER_KEYBOARD_OPERATION,
        BrowserEvent::Pointer { .. } => BROWSER_POINTER_OPERATION,
        BrowserEvent::Click { .. } => BROWSER_CLICK_OPERATION,
        BrowserEvent::Scroll { .. } => BROWSER_SCROLL_OPERATION,
    }
}

/// Registers the extension-owned payload validators used by both the Record
/// Writer and hosted Runner wiring. This keeps operation validation out of CLI.
pub fn register_record_schemas(registry: &mut RecordSchemaRegistry) -> Result<(), AdapterError> {
    for operation_id in [
        BROWSER_KEYBOARD_OPERATION,
        BROWSER_POINTER_OPERATION,
        BROWSER_CLICK_OPERATION,
        BROWSER_SCROLL_OPERATION,
    ] {
        registry
            .register(
                SupportedOperation::new(BROWSER_PROTOCOL_ID, operation_id, 1, BTreeSet::new())
                    .expect("valid static Browser operation"),
                move |bytes| {
                    let event = decode_event(bytes).map_err(|error| error.to_string())?;
                    (operation_for_event(&event) == operation_id)
                        .then_some(())
                        .ok_or_else(|| {
                            format!("Browser payload kind does not match `{operation_id}`")
                        })
                },
            )
            .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
    }
    Ok(())
}

impl AdapterFactory for BrowserAdapter {
    fn id(&self) -> &str {
        BROWSER_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        browser_capabilities()
    }

    fn supported_operations(&self) -> Vec<SupportedOperation> {
        [
            BROWSER_KEYBOARD_OPERATION,
            BROWSER_POINTER_OPERATION,
            BROWSER_CLICK_OPERATION,
            BROWSER_SCROLL_OPERATION,
        ]
        .into_iter()
        .map(|operation| {
            SupportedOperation::new(BROWSER_PROTOCOL_ID, operation, 1, BTreeSet::new())
                .expect("valid static Browser operation")
        })
        .collect()
    }

    fn preflight(
        &self,
        instance: &AdapterInstance,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        parse_config(instance).map(|_| ())
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        let config = parse_config(instance)?;
        let channel_credential = random_credential();
        let browser_session = random_credential();
        let port_id = ato_computation::PortId::parse(&config.port_id)
            .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
        let stylus = Arc::new(BrowserStylus::new(
            Arc::clone(&context.stylus),
            port_id.clone(),
            format!("browser.{}", instance.instance_id),
            BROWSER_ADAPTER_ID,
            config.allowed_non_text_codes.clone(),
        )?);
        let transport = transport::start_transport(
            context.runtime.workspace,
            &instance.instance_id,
            transport::TransportConfig {
                expected_origin: config.expected_origin.clone(),
                port_id,
                allowed_non_text_codes: config.allowed_non_text_codes.clone(),
                channel_credential,
                browser_session,
            },
            stylus,
            context.observations.clone(),
        )?;
        Ok(Box::new(BrowserSession {
            instance_id: instance.instance_id.clone(),
            config,
            transport,
            next_request_id: 1,
            quiesced: false,
        }))
    }
}

struct BrowserSession {
    instance_id: String,
    config: BrowserAdapterConfig,
    transport: transport::TransportHandle,
    next_request_id: u64,
    quiesced: bool,
}

impl BrowserSession {
    fn request_id(&mut self) -> String {
        let request_id = self.next_request_id.to_string();
        self.next_request_id = self.next_request_id.saturating_add(1);
        request_id
    }

    fn transport_failure(&self) -> Result<(), AdapterError> {
        if let Some(error) = self
            .transport
            .failure
            .lock()
            .map_err(|_| {
                AdapterError::Operation("Browser transport failure state poisoned".to_owned())
            })?
            .as_ref()
        {
            return Err(AdapterError::Operation(error.clone()));
        }
        Ok(())
    }

    fn apply_event(&mut self, event: BrowserEvent) -> Result<(), AdapterError> {
        self.transport_failure()?;
        if self.quiesced {
            return Err(AdapterError::Operation(
                "Browser Adapter is already quiesced".to_owned(),
            ));
        }
        let request_id = self.request_id();
        let (sender, receiver) = mpsc::channel();
        self.transport
            .commands
            .send(transport::TransportCommand::Apply {
                request_id,
                event,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, ACK_TIMEOUT, "apply")
    }
}

impl AttachedAdapter for BrowserSession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        BROWSER_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        browser_capabilities()
    }

    fn accepts(&self, record: &RecordEnvelope) -> bool {
        record.adapter_id == BROWSER_ADAPTER_ID
            && record.protocol_id.as_str() == BROWSER_PROTOCOL_ID
            && record.port_id.as_str() == self.config.port_id
    }

    fn apply(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let event = read_event(record, context, &self.config)?;
        self.apply_event(event)
    }

    fn apply_operation(&mut self, operation: &LiveOperation) -> Result<(), AdapterError> {
        if operation.protocol_id.as_str() != BROWSER_PROTOCOL_ID
            || operation.port_id.as_str() != self.config.port_id
        {
            return Err(AdapterError::Operation(
                "Browser live operation has wrong protocol or port".to_owned(),
            ));
        }
        let event = protocol::decode_event_with_policy(
            &operation.payload,
            &self.config.allowed_non_text_codes,
        )
        .map_err(|error| AdapterError::Operation(error.to_string()))?;
        if operation.operation_id.as_str() != operation_for_event(&event) {
            return Err(AdapterError::Operation(
                "Browser live operation kind does not match payload".to_owned(),
            ));
        }
        self.apply_event(event)
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        read_event(record, context, &self.config).map(|_| ())
    }

    fn quiesce(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.transport_failure()?;
        if self.quiesced {
            return Ok(());
        }
        let request_id = self.request_id();
        let (sender, receiver) = mpsc::channel();
        self.transport
            .commands
            .send(transport::TransportCommand::Quiesce {
                request_id,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, ACK_TIMEOUT, "quiesce")?;
        self.quiesced = true;
        Ok(())
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        let _ = self
            .transport
            .commands
            .send(transport::TransportCommand::Shutdown);
        if let Some(join) = self.transport.join.take() {
            join.join().map_err(|_| {
                AdapterError::Operation("Browser transport thread panicked".to_owned())
            })?;
        }
        match std::fs::remove_file(&self.transport.discovery_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.transport_failure()
    }
}

fn browser_capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        observe: true,
        apply: true,
        verify: true,
        quiesce: true,
    }
}

fn parse_config(instance: &AdapterInstance) -> Result<BrowserAdapterConfig, AdapterError> {
    if instance.adapter_id != BROWSER_ADAPTER_ID {
        return Err(AdapterError::InvalidConfig(format!(
            "Browser factory cannot attach `{}`",
            instance.adapter_id
        )));
    }
    let config: BrowserAdapterConfig = serde_json::from_value(instance.config.clone())?;
    ato_computation::PortId::parse(&config.port_id)
        .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
    validate_origin(&config.expected_origin)?;
    for code in &config.allowed_non_text_codes {
        if code.is_empty()
            || code.starts_with("Key")
            || code.starts_with("Digit")
            || code
                .strip_prefix("Numpad")
                .is_some_and(|suffix| suffix.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(AdapterError::InvalidConfig(format!(
                "configured Browser keyboard code `{code}` is text-like"
            )));
        }
    }
    Ok(config)
}

fn validate_origin(origin: &str) -> Result<(), AdapterError> {
    let url = Url::parse(origin)
        .map_err(|error| AdapterError::InvalidConfig(format!("invalid Browser origin: {error}")))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.origin().ascii_serialization() != origin
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AdapterError::InvalidConfig(
            "Browser expected_origin must be an exact HTTP(S) origin".to_owned(),
        ));
    }
    Ok(())
}

fn random_credential() -> String {
    let mut bytes = [0_u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn observed_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string())
}

fn read_event(
    record: &RecordEnvelope,
    context: &AdapterContext<'_>,
    config: &BrowserAdapterConfig,
) -> Result<BrowserEvent, AdapterError> {
    if record.adapter_id != BROWSER_ADAPTER_ID {
        return Err(AdapterError::Operation(
            "Browser record has wrong adapter".to_owned(),
        ));
    }
    if record.protocol_id.as_str() != BROWSER_PROTOCOL_ID {
        return Err(AdapterError::Operation(
            "Browser record has wrong protocol".to_owned(),
        ));
    }
    if record.port_id.as_str() != config.port_id {
        return Err(AdapterError::Operation(
            "Browser record has wrong port".to_owned(),
        ));
    }
    if record.direction != ato_objects::Direction::Inbound {
        return Err(AdapterError::Operation(
            "Browser record has wrong direction".to_owned(),
        ));
    }
    let metadata = context.objects.metadata(&record.payload_ref)?;
    let bytes = read_exact_object(
        context.objects,
        &record.payload_ref,
        metadata.size,
        MAX_BROWSER_EVENT_BYTES,
    )?;
    protocol::decode_event_with_policy(&bytes, &config.allowed_non_text_codes)
        .map_err(|error| AdapterError::Operation(error.to_string()))
}

pub fn runtime_discovery_path(workspace: &Path, instance_id: &str) -> std::path::PathBuf {
    workspace
        .join(".capsule/runs")
        .join(format!("browser-{instance_id}.json"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};

    use ato_adapter_api::{AdapterAttachContext, AdapterRegistry, ObservationSink, Stylus};
    use ato_computation::{ComputationRef, ProtocolId};
    use ato_objects::{Direction, FsObjectStore, ObjectStore, RecordId};
    use ato_record_writer::{
        RecordPipeline, RecordSchemaRegistry, RecordWriterConfig, records_for_frontier,
    };
    use tungstenite::client::IntoClientRequest;
    use tungstenite::{Message, connect};

    use super::*;

    struct ChannelSink(mpsc::Sender<ato_adapter_api::AdapterObservation>);

    struct ChannelStylus(mpsc::Sender<RecordCandidate>);

    impl Stylus for ChannelStylus {
        fn record(&self, candidate: RecordCandidate) -> Result<(), AdapterError> {
            self.0
                .send(candidate)
                .map_err(|error| AdapterError::Operation(error.to_string()))
        }
    }

    struct RejectingStylus;

    impl Stylus for RejectingStylus {
        fn record(&self, _candidate: RecordCandidate) -> Result<(), AdapterError> {
            Err(AdapterError::Operation("Record queue is full".to_owned()))
        }
    }

    impl ObservationSink for ChannelSink {
        fn emit(
            &self,
            observation: ato_adapter_api::AdapterObservation,
        ) -> Result<(), AdapterError> {
            self.0
                .send(observation)
                .map_err(|error| AdapterError::Operation(error.to_string()))
        }
    }

    #[test]
    fn factory_reports_all_required_capabilities() {
        let capabilities = AdapterFactory::capabilities(&BrowserAdapter);
        assert!(capabilities.observe);
        assert!(capabilities.apply);
        assert!(capabilities.verify);
        assert!(capabilities.quiesce);
        let operations = AdapterFactory::supported_operations(&BrowserAdapter);
        assert_eq!(operations.len(), 4);
        assert_eq!(
            operations
                .iter()
                .map(|operation| operation.operation_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                BROWSER_CLICK_OPERATION,
                BROWSER_KEYBOARD_OPERATION,
                BROWSER_POINTER_OPERATION,
                BROWSER_SCROLL_OPERATION,
            ])
        );
    }

    #[test]
    fn browser_stylus_emits_operation_candidates_with_monotonic_local_order() {
        let (sender, receiver) = mpsc::channel();
        let stylus = BrowserStylus::new(
            Arc::new(ChannelStylus(sender)),
            ato_computation::PortId::parse("ui.main").unwrap(),
            "browser.run-1",
            "example.chrome-adapter@1",
            BTreeSet::new(),
        )
        .unwrap();
        let events = [
            BrowserEvent::Keyboard {
                kind: KeyboardKind::KeyDown,
                code: "ArrowLeft".to_owned(),
                modifiers: Modifiers::default(),
            },
            BrowserEvent::Click {
                x_normalized: 0.25,
                y_normalized: 0.75,
                button: 0,
            },
            BrowserEvent::Scroll { x: 0.0, y: 120.0 },
        ];
        for event in &events {
            stylus.record(event).unwrap();
        }
        for (index, expected_operation) in [
            BROWSER_KEYBOARD_OPERATION,
            BROWSER_CLICK_OPERATION,
            BROWSER_SCROLL_OPERATION,
        ]
        .into_iter()
        .enumerate()
        {
            let candidate = receiver.recv().unwrap();
            assert_eq!(candidate.protocol_id.as_str(), BROWSER_PROTOCOL_ID);
            assert_eq!(candidate.operation_id.as_str(), expected_operation);
            assert_eq!(candidate.port_id.as_str(), "ui.main");
            assert_eq!(candidate.local_seq, index as u64 + 1);
            assert_eq!(candidate.stream, "browser.run-1");
            assert_eq!(
                candidate.recorded_by.as_deref(),
                Some("example.chrome-adapter@1")
            );
            assert_eq!(decode_event(&candidate.payload).unwrap(), events[index]);
        }
    }

    #[test]
    fn browser_stylus_propagates_drop_forbidden_queue_failure() {
        let stylus = BrowserStylus::new(
            Arc::new(RejectingStylus),
            ato_computation::PortId::parse("ui.main").unwrap(),
            "browser.run-1",
            BROWSER_ADAPTER_ID,
            BTreeSet::new(),
        )
        .unwrap();
        let error = stylus
            .record(&BrowserEvent::Keyboard {
                kind: KeyboardKind::KeyDown,
                code: "ArrowUp".to_owned(),
                modifiers: Modifiers::default(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("queue is full"));
    }

    #[test]
    fn browser_stylus_flows_through_async_writer_to_a_sealed_frontier() {
        let directory = tempfile::tempdir().unwrap();
        let objects = Arc::new(FsObjectStore::open(directory.path().join("objects")).unwrap());
        let mut schemas = RecordSchemaRegistry::default();
        for operation_id in [
            BROWSER_KEYBOARD_OPERATION,
            BROWSER_POINTER_OPERATION,
            BROWSER_CLICK_OPERATION,
            BROWSER_SCROLL_OPERATION,
        ] {
            schemas
                .register(
                    SupportedOperation::new(BROWSER_PROTOCOL_ID, operation_id, 1, BTreeSet::new())
                        .unwrap(),
                    move |bytes| {
                        let event = decode_event(bytes).map_err(|error| error.to_string())?;
                        (operation_for_event(&event) == operation_id)
                            .then_some(())
                            .ok_or_else(|| "Browser payload operation mismatch".to_owned())
                    },
                )
                .unwrap();
        }
        let records_root = directory.path().join("records");
        let pipeline = RecordPipeline::start(
            RecordWriterConfig::at(&records_root, "run-1"),
            objects.clone(),
            schemas,
        )
        .unwrap();
        let stylus = BrowserStylus::new(
            pipeline.stylus.clone(),
            ato_computation::PortId::parse("ui.main").unwrap(),
            "browser.run-1",
            BROWSER_ADAPTER_ID,
            BTreeSet::new(),
        )
        .unwrap();
        stylus
            .record(&BrowserEvent::Keyboard {
                kind: KeyboardKind::KeyDown,
                code: "ArrowRight".to_owned(),
                modifiers: Modifiers::default(),
            })
            .unwrap();
        stylus
            .record(&BrowserEvent::Click {
                x_normalized: 0.4,
                y_normalized: 0.6,
                button: 0,
            })
            .unwrap();

        let paused = pipeline.barrier.pause_and_seal().unwrap();
        assert_eq!(paused.frontier.last_writer_order, 2);
        assert_eq!(
            paused.frontier.observed_through.get("browser.run-1"),
            Some(&2)
        );
        let records =
            records_for_frontier(&records_root, &paused.frontier, objects.as_ref()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records
                .iter()
                .map(|record| record.operation_id.as_str())
                .collect::<Vec<_>>(),
            vec![BROWSER_KEYBOARD_OPERATION, BROWSER_CLICK_OPERATION]
        );
    }

    #[test]
    fn configuration_rejects_non_origin_and_text_like_override() {
        for (origin, codes) in [
            ("https://example.test/path", BTreeSet::new()),
            ("https://example.test", BTreeSet::from(["KeyA".to_owned()])),
        ] {
            let instance = AdapterInstance {
                instance_id: "browser".to_owned(),
                adapter_id: BROWSER_ADAPTER_ID.to_owned(),
                config: serde_json::to_value(BrowserAdapterConfig {
                    port_id: "app.browser".to_owned(),
                    expected_origin: origin.to_owned(),
                    allowed_non_text_codes: codes,
                })
                .expect("test config should serialize"),
            };
            assert!(parse_config(&instance).is_err());
        }
    }

    #[test]
    fn registry_observes_applies_with_ack_and_quiesces_the_final_frontier() {
        let directory = tempfile::tempdir().expect("temporary repository should open");
        let objects_path = directory.path().join(".capsule/objects");
        let objects = FsObjectStore::open(&objects_path).expect("object store should open");
        let (observations_tx, observations_rx) = mpsc::channel();
        let (records_tx, records_rx) = mpsc::channel();
        let mut registry = AdapterRegistry::default();
        registry
            .register(Arc::new(BrowserAdapter))
            .expect("Browser factory should register");
        let instance = AdapterInstance {
            instance_id: "browser.test".to_owned(),
            adapter_id: BROWSER_ADAPTER_ID.to_owned(),
            config: serde_json::to_value(BrowserAdapterConfig {
                port_id: "app.browser".to_owned(),
                expected_origin: "http://127.0.0.1:3000".to_owned(),
                allowed_non_text_codes: BTreeSet::new(),
            })
            .expect("test config should serialize"),
        };
        let mut sessions = registry
            .attach_all(
                &[instance],
                &AdapterAttachContext {
                    runtime: AdapterContext {
                        workspace: directory.path(),
                        objects: &objects,
                    },
                    stylus: Arc::new(ChannelStylus(records_tx)),
                    observations: Arc::new(ChannelSink(observations_tx)),
                },
            )
            .expect("Browser Adapter should attach");
        let session = sessions.pop().expect("one session should attach");
        let bootstrap: transport::BrowserRuntimeBootstrap = serde_json::from_slice(
            &std::fs::read(runtime_discovery_path(directory.path(), "browser.test"))
                .expect("runtime discovery should exist"),
        )
        .expect("runtime discovery should decode");

        let (apply_seen_tx, apply_seen_rx) = mpsc::channel();
        let (release_apply_tx, release_apply_rx) = mpsc::channel();
        let bridge = std::thread::spawn(move || {
            let mut request = bootstrap
                .control_url
                .as_str()
                .into_client_request()
                .expect("control URL should be valid");
            request.headers_mut().insert(
                "origin",
                bootstrap
                    .expected_origin
                    .parse()
                    .expect("test origin should be a header"),
            );
            let (mut socket, _) = connect(request).expect("test Bridge should connect");
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "hello",
                        "protocol": bootstrap.protocol,
                        "channel_credential": bootstrap.channel_credential,
                        "browser_session": bootstrap.browser_session,
                        "top_level_origin": bootstrap.expected_origin,
                    })
                    .to_string()
                    .into(),
                ))
                .expect("hello should send");
            let hello = socket.read().expect("hello ack should arrive");
            assert!(
                hello
                    .to_text()
                    .expect("ack should be text")
                    .contains("hello_ack")
            );
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "event",
                        "event": {
                            "type": "keyboard",
                            "kind": "key_down",
                            "code": "ArrowRight",
                            "modifiers": {"alt": false, "control": false, "meta": false, "shift": false}
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .expect("observation should send");
            let apply: serde_json::Value = serde_json::from_str(
                socket
                    .read()
                    .expect("apply should arrive")
                    .to_text()
                    .expect("apply should be text"),
            )
            .expect("apply should decode");
            apply_seen_tx.send(()).expect("test should observe apply");
            release_apply_rx.recv().expect("test should release apply");
            socket
                .send(Message::Text(
                    serde_json::json!({"type": "ack", "request_id": apply["request_id"]})
                        .to_string()
                        .into(),
                ))
                .expect("apply ack should send");
            let quiesce: serde_json::Value = serde_json::from_str(
                socket
                    .read()
                    .expect("quiesce should arrive")
                    .to_text()
                    .expect("quiesce should be text"),
            )
            .expect("quiesce should decode");
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "event",
                        "event": {"type": "click", "x_normalized": 0.5, "y_normalized": 0.5, "button": 0}
                    })
                    .to_string()
                    .into(),
                ))
                .expect("frontier event should send");
            socket
                .send(Message::Text(
                    serde_json::json!({"type": "quiesced", "request_id": quiesce["request_id"]})
                        .to_string()
                        .into(),
                ))
                .expect("quiesce ack should send");
        });

        let observed = observations_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Browser observation should arrive");
        let candidate = records_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Browser Record candidate should arrive");
        assert_eq!(candidate.operation_id.as_str(), BROWSER_KEYBOARD_OPERATION);
        assert_eq!(candidate.local_seq, 1);
        assert_eq!(candidate.stream, "browser.browser.test");
        assert_eq!(candidate.payload, observed.payload);
        assert_eq!(observed.adapter_id, BROWSER_ADAPTER_ID);
        assert_eq!(observed.protocol_id.as_str(), BROWSER_PROTOCOL_ID);
        assert_eq!(
            observed.effect,
            ato_adapter_api::ObservationEffect::Evolution
        );
        let payload_ref = objects
            .put(&observed.payload)
            .expect("observation payload should persist");
        let head = ComputationRef::parse(format!("blake3:{}", "ab".repeat(32)))
            .expect("test head should parse");
        let record = RecordEnvelope {
            id: RecordId::new("main", 1),
            adapter_id: BROWSER_ADAPTER_ID.to_owned(),
            protocol_id: ProtocolId::parse(BROWSER_PROTOCOL_ID).expect("protocol should parse"),
            port_id: ato_computation::PortId::parse("app.browser").expect("port should parse"),
            direction: Direction::Inbound,
            payload_ref,
            head_before: head.clone(),
            head_after: head,
            caused_by: Vec::new(),
            observed_at: "0".to_owned(),
        };
        let project = directory.path().to_path_buf();
        let (apply_result_tx, apply_result_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let objects = FsObjectStore::open(project.join(".capsule/objects"))
                .expect("object store should reopen");
            let mut session = session;
            let result = session.apply(
                &record,
                &AdapterContext {
                    workspace: &project,
                    objects: &objects,
                },
            );
            apply_result_tx
                .send((session, result))
                .expect("apply result should return");
        });
        apply_seen_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Bridge should see apply");
        assert!(
            apply_result_rx.try_recv().is_err(),
            "apply returned before Bridge ACK"
        );
        release_apply_tx.send(()).expect("apply should be released");
        let (mut session, result) = apply_result_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("apply should finish after ACK");
        result.expect("apply should succeed");
        session
            .quiesce(&AdapterContext {
                workspace: directory.path(),
                objects: &objects,
            })
            .expect("quiesce should succeed");
        let frontier = observations_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("quiesce should persist the final event before returning");
        let frontier_candidate = records_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("quiesce should submit the final Record before returning");
        assert_eq!(
            frontier_candidate.operation_id.as_str(),
            BROWSER_CLICK_OPERATION
        );
        assert_eq!(frontier_candidate.local_seq, 2);
        assert_eq!(frontier_candidate.payload, frontier.payload);
        assert!(matches!(
            decode_event(&frontier.payload),
            Ok(BrowserEvent::Click { .. })
        ));
        session
            .detach(&AdapterContext {
                workspace: directory.path(),
                objects: &objects,
            })
            .expect("detach should succeed");
        bridge.join().expect("test Bridge should finish");
        assert!(!runtime_discovery_path(directory.path(), "browser.test").exists());
    }
}
