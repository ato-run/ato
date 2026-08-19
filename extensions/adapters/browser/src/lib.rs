//! Browser input is an ordinary Protocol boundary; replay remains generic.

#![forbid(unsafe_code)]

mod coalescer;
mod presentation;
mod protocol;
mod transport;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, PresentationAsset, PresentationCapture,
};
use ato_objects::{RecordEnvelope, read_exact_object};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use url::Url;

pub use protocol::{
    BrowserEvent, BrowserProtocolError, KeyboardKind, Modifiers, PointerKind, PointerType,
    decode_event, encode_event,
};
pub use transport::BrowserRuntimeBootstrap;

pub const BROWSER_ADAPTER_ID: &str = "ato.browser@1";
pub const BROWSER_PROTOCOL_ID: &str = "ato.browser@1";

const MAX_BROWSER_EVENT_BYTES: u64 = 64 * 1024;
pub const BROWSER_LIFECYCLE_TIMEOUT_SECONDS: u64 = 2;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(BROWSER_LIFECYCLE_TIMEOUT_SECONDS);
const LIFECYCLE_CALL_TIMEOUT: Duration = Duration::from_secs(3);
const BRIDGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const APPLY_CALL_TIMEOUT: Duration = Duration::from_secs(33);

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

impl AdapterFactory for BrowserAdapter {
    fn id(&self) -> &str {
        BROWSER_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        browser_capabilities()
    }

    fn validate_replay(&self, records: &[RecordEnvelope]) -> Result<(), AdapterError> {
        let contains_browser_evolution = records.iter().any(|record| {
            record.adapter_id == BROWSER_ADAPTER_ID
                && record.direction == ato_objects::Direction::Inbound
                && record.head_before != record.head_after
        });
        let contains_http_evolution = records.iter().any(|record| {
            record.adapter_id == "ato.http@1"
                && record.direction == ato_objects::Direction::Inbound
                && record.head_before != record.head_after
        });
        if contains_browser_evolution && contains_http_evolution {
            return Err(AdapterError::Operation(
                "Browser-driven network effects cannot currently be replayed through both Browser and HTTP adapters".to_owned(),
            ));
        }
        Ok(())
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
        let runtime_dir = browser_runtime_dir(context.runtime.workspace)?;
        let transport = transport::start_transport(
            &runtime_dir,
            &instance.instance_id,
            transport::TransportConfig {
                expected_origin: config.expected_origin.clone(),
                port_id: ato_computation::PortId::parse(&config.port_id)
                    .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?,
                allowed_non_text_codes: config.allowed_non_text_codes.clone(),
                channel_credential,
                browser_session,
            },
            context.observations.clone(),
        )?;
        Ok(Box::new(BrowserSession {
            instance_id: instance.instance_id.clone(),
            config,
            transport,
            next_request_id: 1,
            quiesced: false,
            capture_paused: false,
        }))
    }
}

impl PresentationCapture for BrowserSession {
    fn capture_final(
        &mut self,
        _context: &AdapterContext<'_>,
    ) -> Result<Vec<PresentationAsset>, AdapterError> {
        let runtime_dir = self.transport.discovery_path.parent().ok_or_else(|| {
            AdapterError::Operation("Browser runtime discovery has no parent".to_owned())
        })?;
        presentation::capture_final(runtime_dir, &self.config.expected_origin)
            .map(|asset| asset.into_iter().collect())
    }
}

struct BrowserSession {
    instance_id: String,
    config: BrowserAdapterConfig,
    transport: transport::TransportHandle,
    next_request_id: u64,
    quiesced: bool,
    capture_paused: bool,
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

    fn presentation_capture(&mut self) -> Option<&mut dyn PresentationCapture> {
        Some(self)
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
        self.transport_failure()?;
        if self.quiesced {
            return Err(AdapterError::Operation(
                "Browser Adapter is already quiesced".to_owned(),
            ));
        }
        let event = read_event(record, context, &self.config)?;
        let request_id = self.request_id();
        let (sender, receiver) = mpsc::channel();
        self.transport
            .commands
            .send(transport::TransportCommand::Apply {
                request_id,
                event,
                deadline: std::time::Instant::now() + BRIDGE_CONNECT_TIMEOUT,
                ack_timeout: REQUEST_TIMEOUT,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, APPLY_CALL_TIMEOUT, "apply")
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
                deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
                ack_timeout: REQUEST_TIMEOUT,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, LIFECYCLE_CALL_TIMEOUT, "quiesce")?;
        self.quiesced = true;
        Ok(())
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.transport.shutdown()
    }

    fn pause_for_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.transport_failure()?;
        if self.quiesced {
            return Err(AdapterError::Operation(
                "Browser Adapter is already quiesced".to_owned(),
            ));
        }
        if self.capture_paused {
            return Ok(());
        }
        let request_id = self.request_id();
        let (sender, receiver) = mpsc::channel();
        self.transport
            .commands
            .send(transport::TransportCommand::Pause {
                request_id,
                deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
                ack_timeout: REQUEST_TIMEOUT,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, LIFECYCLE_CALL_TIMEOUT, "capture pause")?;
        self.capture_paused = true;
        Ok(())
    }

    fn resume_after_capture(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.transport_failure()?;
        if !self.capture_paused {
            return Ok(());
        }
        let request_id = self.request_id();
        let (sender, receiver) = mpsc::channel();
        self.transport
            .commands
            .send(transport::TransportCommand::Resume {
                request_id,
                deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
                ack_timeout: REQUEST_TIMEOUT,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, LIFECYCLE_CALL_TIMEOUT, "capture resume")?;
        self.capture_paused = false;
        Ok(())
    }

    fn activate(&mut self) -> Result<(), AdapterError> {
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
            .send(transport::TransportCommand::Activate {
                request_id,
                deadline: std::time::Instant::now() + REQUEST_TIMEOUT,
                ack_timeout: REQUEST_TIMEOUT,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, LIFECYCLE_CALL_TIMEOUT, "activate")
    }
}

fn browser_capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        observe: true,
        apply: true,
        verify: true,
        quiesce: true,
        capture_consistency: ato_adapter_api::CaptureConsistency::AdapterMediated,
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

fn browser_runtime_dir(workspace: &Path) -> Result<std::path::PathBuf, AdapterError> {
    let Some(configured) = std::env::var_os("ATO_BROWSER_RUNTIME_DIR") else {
        return Ok(workspace.join(".capsule/runs"));
    };
    let path = std::path::PathBuf::from(configured);
    if !path.is_absolute() {
        return Err(AdapterError::InvalidConfig(
            "ATO_BROWSER_RUNTIME_DIR must be an absolute host-private path".to_owned(),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};

    use ato_adapter_api::{AdapterAttachContext, AdapterRegistry, ObservationSink};
    use ato_computation::{ComputationRef, ProtocolId};
    use ato_objects::{Direction, FsObjectStore, ObjectStore, RecordId};
    use tungstenite::client::IntoClientRequest;
    use tungstenite::{Message, connect};

    use super::*;

    struct ChannelSink(mpsc::Sender<ato_adapter_api::AdapterObservation>);

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
    fn replay_policy_rejects_browser_and_http_evolution_together() {
        let directory = tempfile::tempdir().expect("temporary object store should open");
        let objects = FsObjectStore::open(directory.path().join("objects"))
            .expect("object store should open");
        let payload_ref = objects.put(b"payload").expect("payload should persist");
        let before = ComputationRef::parse(format!("blake3:{}", "ab".repeat(32)))
            .expect("test head should parse");
        let after = ComputationRef::parse(format!("blake3:{}", "cd".repeat(32)))
            .expect("test head should parse");
        let record =
            |sequence, adapter_id: &str, protocol_id: &str, port_id: &str| RecordEnvelope {
                id: RecordId::new("main", sequence),
                adapter_id: adapter_id.to_owned(),
                protocol_id: ProtocolId::parse(protocol_id).expect("protocol should parse"),
                port_id: ato_computation::PortId::parse(port_id).expect("port should parse"),
                direction: Direction::Inbound,
                payload_ref: payload_ref.clone(),
                head_before: before.clone(),
                head_after: after.clone(),
                caused_by: Vec::new(),
                observed_at: "0".to_owned(),
            };
        let browser = record(1, BROWSER_ADAPTER_ID, BROWSER_PROTOCOL_ID, "app.browser");
        let http = record(2, "ato.http@1", "ato.http@1", "app.http");

        AdapterFactory::validate_replay(&BrowserAdapter, std::slice::from_ref(&browser))
            .expect("Browser-only replay should remain valid");
        let error = AdapterFactory::validate_replay(&BrowserAdapter, &[browser, http])
            .expect_err("Browser and HTTP Evolution must fail closed");
        assert!(error.to_string().contains("cannot currently be replayed"));
    }

    #[test]
    fn verification_rejects_wrong_adapter_protocol_port_and_direction() {
        let directory = tempfile::tempdir().expect("temporary object store should open");
        let objects = FsObjectStore::open(directory.path().join("objects"))
            .expect("object store should open");
        let payload = encode_event(&BrowserEvent::Keyboard {
            kind: KeyboardKind::KeyDown,
            code: "ArrowRight".to_owned(),
            modifiers: Modifiers::default(),
        })
        .expect("test event should encode");
        let payload_ref = objects.put(&payload).expect("payload should persist");
        let head = ComputationRef::parse(format!("blake3:{}", "ab".repeat(32)))
            .expect("test head should parse");
        let config = BrowserAdapterConfig {
            port_id: "app.browser".to_owned(),
            expected_origin: "http://127.0.0.1:3000".to_owned(),
            allowed_non_text_codes: BTreeSet::new(),
        };
        let base = RecordEnvelope {
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
        let context = AdapterContext {
            workspace: directory.path(),
            objects: &objects,
        };
        let mut wrong_adapter = base.clone();
        wrong_adapter.adapter_id = "example.wrong@1".to_owned();
        let mut wrong_protocol = base.clone();
        wrong_protocol.protocol_id =
            ProtocolId::parse("example.wrong@1").expect("protocol should parse");
        let mut wrong_port = base.clone();
        wrong_port.port_id =
            ato_computation::PortId::parse("wrong.browser").expect("port should parse");
        let mut wrong_direction = base;
        wrong_direction.direction = Direction::Outbound;
        for record in [wrong_adapter, wrong_protocol, wrong_port, wrong_direction] {
            assert!(read_event(&record, &context, &config).is_err());
        }
    }

    #[test]
    fn registry_observes_applies_with_ack_and_quiesces_the_final_frontier() {
        let directory = tempfile::tempdir().expect("temporary repository should open");
        let objects_path = directory.path().join(".capsule/objects");
        let objects = FsObjectStore::open(&objects_path).expect("object store should open");
        let (observations_tx, observations_rx) = mpsc::channel();
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
                    observations: Arc::new(ChannelSink(observations_tx)),
                },
            )
            .expect("Browser Adapter should attach");
        let mut session = sessions.pop().expect("one session should attach");
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
            let hello: serde_json::Value = serde_json::from_str(
                socket
                    .read()
                    .expect("hello ack should arrive")
                    .to_text()
                    .expect("ack should be text"),
            )
            .expect("hello ack should decode");
            assert_eq!(hello["type"], "hello_ack");
            if hello["lifecycle"] == "restoring" {
                let activate: serde_json::Value = serde_json::from_str(
                    socket
                        .read()
                        .expect("activate should arrive")
                        .to_text()
                        .expect("activate should be text"),
                )
                .expect("activate should decode");
                socket
                    .send(Message::Text(
                        serde_json::json!({
                            "type": "activated",
                            "request_id": activate["request_id"]
                        })
                        .to_string()
                        .into(),
                    ))
                    .expect("activate ack should send");
            }
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
            let quiesce = loop {
                let message: serde_json::Value = serde_json::from_str(
                    socket
                        .read()
                        .expect("quiesce should arrive")
                        .to_text()
                        .expect("quiesce should be text"),
                )
                .expect("quiesce should decode");
                if message["type"] == "quiesce" {
                    break message;
                }
            };
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "event",
                        "event": {
                            "type": "pointer",
                            "kind": "pointer_move",
                            "pointer_id": 1,
                            "pointer_type": "mouse",
                            "x_normalized": 0.5,
                            "y_normalized": 0.5,
                            "button": -1,
                            "buttons": 0
                        }
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

        session.activate().expect("Browser Adapter should activate");
        let observed = observations_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Browser observation should arrive");
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
        assert!(matches!(
            decode_event(&frontier.payload),
            Ok(BrowserEvent::Pointer {
                kind: PointerKind::PointerMove,
                ..
            })
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
