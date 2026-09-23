//! Browser input is an ordinary Protocol boundary; replay remains generic.

#![forbid(unsafe_code)]

mod coalescer;
mod operation;
mod protocol;
mod transport;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, Instant};

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AttachedAdapter, LiveOperation, LiveOperationDispatcher,
    LiveOperationSettlement, Stylus, SupportedOperation,
};
use ato_objects::{RecordCandidate, RecordEnvelope, read_exact_object};
use ato_record_writer::RecordSchemaRegistry;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use url::Url;

pub use operation::{
    ActorOperationLedger, BrowserOperationError, BrowserOperationInvocationV1,
    BrowserOperationReceiptV1, BrowserSurfaceProjectionV1, BrowserSurfaceTracker,
    OperationDescriptorV1, OperationSource, RawWebMcpSnapshotV1, RawWebMcpToolV1,
    RunnerOperationResultV1, SurfaceObservationV1, SurfaceOperationDescriptorV1, WebMcpProducerApi,
    decode_operation_descriptor, encode_operation_descriptor,
};
pub use protocol::{
    BrowserEvent, BrowserProtocolError, KeyboardKind, Modifiers, PointerKind, PointerType,
    decode_event, encode_event,
};
pub use transport::BrowserRuntimeBootstrap;

pub const BROWSER_ADAPTER_ID: &str = "ato.browser@1";
pub const BROWSER_PROTOCOL_ID: &str = "ato.browser@1";
pub const BROWSER_KEYBOARD_OPERATION: &str = "keyboard";
pub const BROWSER_POINTER_OPERATION: &str = "pointer";
pub const BROWSER_CLICK_OPERATION: &str = "click";
pub const BROWSER_SCROLL_OPERATION: &str = "scroll";
pub const BROWSER_GENERIC_OPERATION: &str = "operation";

const MAX_BROWSER_EVENT_BYTES: u64 = 64 * 1024;
// Browser operations are interactive control, not background jobs. A page
// that blocks its main thread can also prevent JavaScript timers from firing,
// so the native transport owns this independent hard deadline.
// The BYOA v0 acceptance surface deliberately includes a five-second WebMCP
// handler. Keep a bounded margin above that physical duration so an ACK at
// the boundary cannot be misclassified as indeterminate. A hung untrusted
// page is still fenced well inside the controller's 30s request horizon.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Selects whether trusted DOM events are merely observed by a local CLI, or
/// whether this Adapter accepts only Runner-authorized physical operations.
/// Hosted Runs must use `ApplyOnly`: a trusted event has already changed the
/// page, so observing it cannot be an authoritative operation ingress.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserInputMode {
    #[default]
    ObserveAndApply,
    ApplyOnly,
}

/// Ephemeral authority scope for a hosted Public Activity Browser channel.
/// Actor authorization remains at the Activity Controller boundary because
/// one Browser realization is intentionally shared by multiple actors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserChannelScope {
    pub activity_id: String,
    pub run_id: String,
    pub epoch: String,
    pub expires_at_unix_seconds: i64,
}

impl BrowserInputMode {
    pub(crate) fn observes_trusted_events(self) -> bool {
        matches!(self, Self::ObserveAndApply)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserAdapterConfig {
    pub port_id: String,
    pub expected_origin: String,
    #[serde(default)]
    pub allowed_non_text_codes: BTreeSet<String>,
    #[serde(default)]
    pub input_mode: BrowserInputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_scope: Option<BrowserChannelScope>,
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
        BrowserEvent::Operation { .. } => BROWSER_GENERIC_OPERATION,
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
        BROWSER_GENERIC_OPERATION,
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
            BROWSER_GENERIC_OPERATION,
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
        let dispatcher_port_id = config.port_id.clone();
        let dispatcher_allowed_codes = config.allowed_non_text_codes.clone();
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
                port_id: port_id.clone(),
                allowed_non_text_codes: config.allowed_non_text_codes.clone(),
                input_mode: config.input_mode,
                channel_credential,
                browser_session,
                channel_scope: config.channel_scope.clone(),
            },
            stylus,
            context.observations.clone(),
        )?;
        Ok(Box::new(BrowserSession {
            instance_id: instance.instance_id.clone(),
            config,
            dispatcher: Arc::new(BrowserLiveOperationDispatcher {
                commands: transport.commands.clone(),
                failure: Arc::clone(&transport.failure),
                port_id: dispatcher_port_id,
                allowed_non_text_codes: dispatcher_allowed_codes,
                next_request_id: AtomicU64::new(0),
                lifecycle_stopped: RwLock::new(false),
            }),
            transport,
        }))
    }
}

struct BrowserSession {
    instance_id: String,
    config: BrowserAdapterConfig,
    transport: transport::TransportHandle,
    dispatcher: Arc<BrowserLiveOperationDispatcher>,
}

struct BrowserLiveOperationDispatcher {
    commands: mpsc::Sender<transport::TransportCommand>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
    port_id: String,
    allowed_non_text_codes: BTreeSet<String>,
    next_request_id: AtomicU64,
    lifecycle_stopped: RwLock<bool>,
}

impl BrowserLiveOperationDispatcher {
    fn request_id(&self) -> String {
        (self.next_request_id.fetch_add(1, Ordering::Relaxed) + 1).to_string()
    }

    fn transport_failure(&self) -> Result<(), AdapterError> {
        if let Some(error) = self
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

    fn apply_event(
        &self,
        correlation_id: Option<&str>,
        realization_generation: Option<&str>,
        event: BrowserEvent,
    ) -> Result<Option<u64>, AdapterError> {
        self.apply_event_with_timeout(correlation_id, realization_generation, event, ACK_TIMEOUT)
    }

    fn apply_event_with_timeout(
        &self,
        correlation_id: Option<&str>,
        realization_generation: Option<&str>,
        event: BrowserEvent,
        ack_timeout: Duration,
    ) -> Result<Option<u64>, AdapterError> {
        let stopped = self.lifecycle_stopped.read().map_err(|_| {
            AdapterError::Operation("Browser Adapter lifecycle state poisoned".to_owned())
        })?;
        self.transport_failure()?;
        if *stopped {
            return Err(AdapterError::Operation(
                "Browser Adapter is already quiesced".to_owned(),
            ));
        }
        let request_id = self.request_id();
        let ordered = correlation_id.is_some();
        let correlation_id = correlation_id.unwrap_or(&request_id).to_owned();
        let (sender, receiver) = mpsc::channel();
        let deadline = Instant::now() + ack_timeout;
        self.commands
            .send(transport::TransportCommand::Apply {
                request_id,
                correlation_id,
                realization_generation: realization_generation.map(str::to_owned),
                ordered,
                deadline,
                event,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        // The transport thread owns both the deadline and ACK demultiplexer.
        // Waiting without an independent timeout prevents a boundary race in
        // which this thread abandons ticket N while the transport publishes
        // ticket N+1. Transport termination drops this channel, so the wait is
        // still bounded by its fail-closed lifecycle.
        transport::wait_for_apply_result(receiver)
    }

    fn validate_operation(&self, operation: &LiveOperation) -> Result<BrowserEvent, AdapterError> {
        if operation.protocol_id.as_str() != BROWSER_PROTOCOL_ID
            || operation.port_id.as_str() != self.port_id
        {
            return Err(AdapterError::Operation(
                "Browser live operation has wrong protocol or port".to_owned(),
            ));
        }
        let event =
            protocol::decode_event_with_policy(&operation.payload, &self.allowed_non_text_codes)
                .map_err(|error| AdapterError::Operation(error.to_string()))?;
        if operation.operation_id.as_str() != operation_for_event(&event) {
            return Err(AdapterError::Operation(
                "Browser live operation kind does not match payload".to_owned(),
            ));
        }
        Ok(event)
    }
}

impl LiveOperationDispatcher for BrowserLiveOperationDispatcher {
    fn apply_operation(
        &self,
        correlation_id: &str,
        realization_generation: Option<&str>,
        operation: &LiveOperation,
    ) -> Result<LiveOperationSettlement, AdapterError> {
        if correlation_id.is_empty()
            || correlation_id.len() > 160
            || !correlation_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
        {
            return Err(AdapterError::Operation(
                "invalid Browser operation correlation id".to_owned(),
            ));
        }
        if realization_generation.is_some_and(|generation| {
            generation.is_empty()
                || generation.len() > 256
                || !generation
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        }) {
            return Err(AdapterError::Operation(
                "invalid Browser realization generation".to_owned(),
            ));
        }
        let order = self
            .apply_event(
                Some(correlation_id),
                realization_generation,
                self.validate_operation(operation)?,
            )?
            .ok_or_else(|| {
                AdapterError::Operation(
                    "Browser live apply returned no settlement order".to_owned(),
                )
            })?;
        Ok(LiveOperationSettlement { order })
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
        self.dispatcher.apply_event(None, None, event).map(|_| ())
    }

    fn apply_operation(&mut self, operation: &LiveOperation) -> Result<(), AdapterError> {
        self.dispatcher
            .apply_event(None, None, self.dispatcher.validate_operation(operation)?)
            .map(|_| ())
    }

    fn live_operation_dispatcher(&self) -> Option<Arc<dyn LiveOperationDispatcher>> {
        Some(Arc::clone(&self.dispatcher) as Arc<dyn LiveOperationDispatcher>)
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        read_event(record, context, &self.config).map(|_| ())
    }

    fn quiesce(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.dispatcher.transport_failure()?;
        let mut stopped = self.dispatcher.lifecycle_stopped.write().map_err(|_| {
            AdapterError::Operation("Browser Adapter lifecycle state poisoned".to_owned())
        })?;
        if *stopped {
            return Ok(());
        }
        *stopped = true;
        let request_id = self.dispatcher.request_id();
        let (sender, receiver) = mpsc::channel();
        self.transport
            .commands
            .send(transport::TransportCommand::Quiesce {
                request_id,
                result: sender,
            })
            .map_err(|error| AdapterError::Operation(error.to_string()))?;
        transport::wait_for_result(receiver, ACK_TIMEOUT, "quiesce")?;
        Ok(())
    }

    fn detach(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        *self.dispatcher.lifecycle_stopped.write().map_err(|_| {
            AdapterError::Operation("Browser Adapter lifecycle state poisoned".to_owned())
        })? = true;
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
        match std::fs::remove_file(&self.transport.readiness_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.dispatcher.transport_failure()
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
    if let Some(scope) = &config.channel_scope {
        for (name, value) in [
            ("activity_id", scope.activity_id.as_str()),
            ("run_id", scope.run_id.as_str()),
            ("epoch", scope.epoch.as_str()),
        ] {
            if value.is_empty()
                || value.len() > 256
                || !value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
                })
            {
                return Err(AdapterError::InvalidConfig(format!(
                    "Browser channel scope {name} is invalid"
                )));
            }
        }
        if scope.expires_at_unix_seconds <= observed_unix_seconds() {
            return Err(AdapterError::InvalidConfig(
                "Browser channel scope is expired".to_owned(),
            ));
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
    observed_unix_seconds().to_string()
}

fn observed_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| value.as_secs().try_into().unwrap_or(i64::MAX))
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
    use std::sync::{Arc, Mutex, mpsc};

    use ato_adapter_api::{AdapterAttachContext, AdapterRegistry, ObservationSink, Stylus};
    use ato_computation::{ComputationRef, OperationId, PortId, ProtocolId};
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
        assert_eq!(operations.len(), 5);
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
                BROWSER_GENERIC_OPERATION,
            ])
        );
    }

    #[test]
    fn live_dispatch_preserves_controller_correlation_outside_event_payload() {
        let (commands, receiver) = mpsc::channel();
        let dispatcher = Arc::new(BrowserLiveOperationDispatcher {
            commands,
            failure: Arc::new(Mutex::new(None)),
            port_id: "app.browser".to_owned(),
            allowed_non_text_codes: BTreeSet::new(),
            next_request_id: AtomicU64::new(0),
            lifecycle_stopped: RwLock::new(false),
        });
        let event = BrowserEvent::Keyboard {
            kind: KeyboardKind::KeyDown,
            code: "ArrowRight".to_owned(),
            modifiers: Modifiers::default(),
        };
        let operation = LiveOperation {
            protocol_id: ProtocolId::parse(BROWSER_PROTOCOL_ID).unwrap(),
            operation_id: OperationId::parse(BROWSER_KEYBOARD_OPERATION).unwrap(),
            port_id: PortId::parse("app.browser").unwrap(),
            payload: encode_event(&event).unwrap(),
        };
        let caller = Arc::clone(&dispatcher);
        let apply = std::thread::spawn(move || {
            LiveOperationDispatcher::apply_operation(
                caller.as_ref(),
                "aop_controller_42",
                Some("document_generation_7"),
                &operation,
            )
        });
        let transport::TransportCommand::Apply {
            request_id,
            correlation_id,
            realization_generation,
            ordered,
            deadline,
            event: transported,
            result,
        } = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("transport apply")
        else {
            panic!("unexpected transport command");
        };
        assert_eq!(request_id, "1");
        assert_eq!(correlation_id, "aop_controller_42");
        assert_eq!(
            realization_generation.as_deref(),
            Some("document_generation_7")
        );
        assert!(ordered);
        assert!(deadline > Instant::now());
        assert_eq!(transported, event);
        result.send(Ok(Some(1))).expect("transport ACK");
        assert_eq!(
            apply.join().expect("apply thread").expect("live apply"),
            LiveOperationSettlement { order: 1 }
        );
    }

    #[test]
    fn ordered_apply_waits_for_the_transport_owned_deadline_outcome() {
        let (commands, receiver) = mpsc::channel();
        let dispatcher = Arc::new(BrowserLiveOperationDispatcher {
            commands,
            failure: Arc::new(Mutex::new(None)),
            port_id: "app.browser".to_owned(),
            allowed_non_text_codes: BTreeSet::new(),
            next_request_id: AtomicU64::new(0),
            lifecycle_stopped: RwLock::new(false),
        });
        let caller = Arc::clone(&dispatcher);
        let apply = std::thread::spawn(move || {
            caller.apply_event_with_timeout(
                Some("aop_hung_page"),
                Some("document_hung"),
                BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
                Duration::from_millis(20),
            )
        });
        let transport::TransportCommand::Apply {
            deadline, result, ..
        } = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("transport apply")
        else {
            panic!("unexpected transport command");
        };
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .saturating_add(Duration::from_millis(5)),
        );
        result
            .send(Err(AdapterError::Operation(
                "physical_outcome_indeterminate".to_owned(),
            )))
            .expect("transport deadline result");
        let error = apply
            .join()
            .expect("apply thread")
            .expect_err("hung Browser operation must fail closed");
        assert!(error.to_string().contains("physical_outcome_indeterminate"));
        assert!(
            receiver.recv_timeout(Duration::from_millis(20)).is_err(),
            "the dispatcher must not race the transport with a second Fence command"
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
                    input_mode: BrowserInputMode::ObserveAndApply,
                    channel_scope: None,
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
                input_mode: BrowserInputMode::ObserveAndApply,
                channel_scope: None,
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
            let hello: serde_json::Value =
                serde_json::from_str(hello.to_text().expect("ack should remain readable as text"))
                    .expect("hello ack should decode");
            assert_eq!(hello["last_sequence"], 0);
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
            assert_eq!(
                apply["operation_id"], apply["request_id"],
                "legacy Record apply uses a scoped transport correlation"
            );
            apply_seen_tx.send(()).expect("test should observe apply");
            release_apply_rx.recv().expect("test should release apply");
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "ack",
                        "request_id": apply["request_id"],
                        "sequence": apply["sequence"]
                    })
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
                    serde_json::json!({
                        "type": "quiesced",
                        "request_id": quiesce["request_id"],
                        "sequence": quiesce["sequence"]
                    })
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
