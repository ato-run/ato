//! Replay delegates every record to its declared Adapter; it has no protocol switch.

#![forbid(unsafe_code)]

use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_materializer_api::{
    Compatibility, Materializer, MaterializerContext, MaterializerError, Realization,
    RestoreCapability,
};
use ato_objects::{
    BundleError, Direction, MaterializationReferences, ObjectLink, ObjectResolver, RecordEnvelope,
    RecordId, read_exact_object,
};
use serde::{Deserialize, Serialize};

pub const REPLAY_MATERIALIZER_ID: &str = "ato.replay@1";
const REPLAY_VERSION: u32 = 1;
const MAX_REPLAY_DESCRIPTOR_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayDescriptor {
    version: u32,
    target: String,
    anchor: String,
    records: Vec<RecordWire>,
    required_adapters: Vec<String>,
    required_protocols: Vec<String>,
    required_bindings: Vec<String>,
    contracts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordWire {
    id: RecordId,
    adapter_id: String,
    protocol_id: String,
    port_id: String,
    direction: Direction,
    payload_ref: String,
    head_before: String,
    head_after: String,
    caused_by: Vec<RecordId>,
    observed_at: String,
}

/// A fully decoded and causally validated `ato.replay@1` descriptor.
///
/// The vector order is the descriptor order. Timestamps are deliberately not
/// parsed or consulted here: `observed_at` is presentation metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySequence {
    anchor: ComputationRef,
    target: ComputationRef,
    records: Vec<RecordEnvelope>,
    required_adapters: Vec<String>,
    required_bindings: Vec<String>,
}

impl ReplaySequence {
    pub fn load(
        descriptor: &ContentRef,
        objects: &dyn ObjectResolver,
        adapters: &ato_adapter_api::AdapterRegistry,
    ) -> Result<Self, MaterializerError> {
        let sequence = decode_sequence(descriptor, objects)?;
        for id in &sequence.required_adapters {
            let adapter = adapters
                .get(id)
                .map_err(|_| MaterializerError::MissingApply {
                    materializer: REPLAY_MATERIALIZER_ID.to_owned(),
                    adapter: id.clone(),
                })?;
            if !adapter.capabilities().apply {
                return Err(MaterializerError::MissingApply {
                    materializer: REPLAY_MATERIALIZER_ID.to_owned(),
                    adapter: id.clone(),
                });
            }
        }
        Ok(sequence)
    }

    pub fn anchor(&self) -> &ComputationRef {
        &self.anchor
    }

    pub fn target(&self) -> &ComputationRef {
        &self.target
    }

    pub fn records(&self) -> &[RecordEnvelope] {
        &self.records
    }

    pub fn required_adapters(&self) -> &[String] {
        &self.required_adapters
    }

    pub fn required_bindings(&self) -> &[String] {
        &self.required_bindings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayProgress {
    pub cursor: usize,
    pub total: usize,
    pub record_id: RecordId,
    pub protocol_id: ProtocolId,
    pub port_id: PortId,
    pub direction: Direction,
    pub head_before: ComputationRef,
    pub head_after: ComputationRef,
    pub observed_at: String,
}

/// Runtime control state around the existing Replay Materialization.
/// It is not a Computation and does not create Record history.
pub struct ReplayStepper {
    sequence: ReplaySequence,
    cursor: usize,
    current_head: ComputationRef,
    runtime: Box<dyn ato_materializer_api::ReplayRuntime>,
}

impl ReplayStepper {
    pub fn begin(
        sequence: ReplaySequence,
        driver: &dyn ato_materializer_api::RealizationDriver,
    ) -> Result<Self, MaterializerError> {
        // `sequence` was completely validated before this first physical side
        // effect. In particular, malformed chains never reach driver.begin().
        let current_head = sequence.anchor.clone();
        let runtime = driver.begin(&current_head)?;
        Ok(Self {
            sequence,
            cursor: 0,
            current_head,
            runtime,
        })
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn total(&self) -> usize {
        self.sequence.records.len()
    }

    pub fn current_head(&self) -> &ComputationRef {
        &self.current_head
    }

    pub fn step(&mut self) -> Result<Option<ReplayProgress>, MaterializerError> {
        let Some(record) = self.sequence.records.get(self.cursor) else {
            return Ok(None);
        };
        self.runtime.apply(record)?;
        self.cursor += 1;
        self.current_head = record.head_after.clone();
        Ok(Some(ReplayProgress {
            cursor: self.cursor,
            total: self.sequence.records.len(),
            record_id: record.id.clone(),
            protocol_id: record.protocol_id.clone(),
            port_id: record.port_id.clone(),
            direction: record.direction,
            head_before: record.head_before.clone(),
            head_after: record.head_after.clone(),
            observed_at: record.observed_at.clone(),
        }))
    }

    pub fn finish(self) -> Result<Box<dyn Realization>, MaterializerError> {
        if self.cursor != self.sequence.records.len() || self.current_head != self.sequence.target {
            return Err(MaterializerError::Operation(format!(
                "replay is incomplete at {}/{} ({})",
                self.cursor,
                self.sequence.records.len(),
                self.current_head
            )));
        }
        self.runtime.finish(&self.sequence.target)
    }
}

#[derive(Default)]
pub struct ReplayMaterializer;

impl Materializer for ReplayMaterializer {
    fn id(&self) -> &str {
        REPLAY_MATERIALIZER_ID
    }

    fn restore_capability(&self) -> RestoreCapability {
        RestoreCapability::Supported
    }

    fn encode(
        &self,
        target: &ComputationRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ContentRef, MaterializerError> {
        let mut required_adapters = Vec::new();
        let mut required_protocols = Vec::new();
        for record in context.records {
            let adapter = context.adapters.get(&record.adapter_id).map_err(|_| {
                MaterializerError::MissingApply {
                    materializer: self.id().to_owned(),
                    adapter: record.adapter_id.clone(),
                }
            })?;
            if !adapter.capabilities().apply {
                return Err(MaterializerError::MissingApply {
                    materializer: self.id().to_owned(),
                    adapter: record.adapter_id.clone(),
                });
            }
            required_adapters.push(record.adapter_id.clone());
            required_protocols.push(record.protocol_id.to_string());
        }
        required_adapters.sort();
        required_adapters.dedup();
        required_protocols.sort();
        required_protocols.dedup();
        let anchor = context
            .records
            .first()
            .map_or_else(|| target.clone(), |record| record.head_before.clone());
        let descriptor = ReplayDescriptor {
            version: REPLAY_VERSION,
            target: target.to_string(),
            anchor: anchor.to_string(),
            records: context.records.iter().map(RecordWire::from).collect(),
            required_adapters,
            required_protocols,
            required_bindings: Vec::new(),
            contracts: Vec::new(),
        };
        Ok(context.objects.put(&serde_jcs::to_vec(&descriptor)?)?)
    }

    fn verify(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError> {
        Ok(
            ReplaySequence::load(descriptor, context.objects, context.adapters)?
                .target
                .clone(),
        )
    }

    fn compatibility(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Compatibility {
        match self.verify(descriptor, context) {
            Ok(_) => Compatibility::Compatible,
            Err(MaterializerError::MissingApply { .. }) => Compatibility::Incompatible,
            Err(_) => Compatibility::Unknown,
        }
    }

    fn restore(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        let sequence = ReplaySequence::load(descriptor, context.objects, context.adapters)?;
        let driver = context
            .realization
            .ok_or_else(|| MaterializerError::RealizationUnavailable(self.id().to_owned()))?;
        let mut stepper = ReplayStepper::begin(sequence, driver)?;
        while stepper.step()?.is_some() {}
        stepper.finish()
    }
}

/// Decode the verifier-approved Record chain retained by a Replay
/// Materialization. Hosted continuation capture uses this to carry the
/// immutable parent realization history into a child bundle before appending
/// the session-local delta.
pub fn records_for_descriptor(
    descriptor: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<Vec<RecordEnvelope>, MaterializerError> {
    Ok(decode_sequence(descriptor, objects)?.records)
}

fn decode_sequence(
    descriptor: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<ReplaySequence, MaterializerError> {
    let descriptor = load_descriptor(descriptor, objects)
        .map_err(|error| MaterializerError::Operation(error.to_string()))?;
    let anchor = parse_computation(&descriptor.anchor)?;
    let target = parse_computation(&descriptor.target)?;
    let records = descriptor
        .records
        .into_iter()
        .map(RecordEnvelope::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let declared_adapters = descriptor
        .required_adapters
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let declared_protocols = descriptor
        .required_protocols
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if declared_adapters.len() != descriptor.required_adapters.len()
        || declared_protocols.len() != descriptor.required_protocols.len()
        || !descriptor
            .required_adapters
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        || !descriptor
            .required_protocols
            .windows(2)
            .all(|pair| pair[0] < pair[1])
    {
        return Err(MaterializerError::Operation(
            "replay required implementation lists are not canonical".to_owned(),
        ));
    }
    let mut current = anchor.clone();
    for record in &records {
        if record.head_before != current {
            return Err(MaterializerError::Operation(format!(
                "replay causal head mismatch at {:?}: expected {}, got {}",
                record.id, current, record.head_before
            )));
        }
        if !declared_adapters.contains(&record.adapter_id)
            || !declared_protocols.contains(&record.protocol_id.to_string())
        {
            return Err(MaterializerError::Operation(format!(
                "replay record {:?} uses an undeclared implementation",
                record.id
            )));
        }
        objects.metadata(&record.payload_ref)?;
        current = record.head_after.clone();
    }
    if current != target {
        return Err(MaterializerError::Operation(format!(
            "replay derived {current}, descriptor target is {target}"
        )));
    }
    Ok(ReplaySequence {
        anchor,
        target,
        records,
        required_adapters: descriptor.required_adapters,
        required_bindings: descriptor.required_bindings,
    })
}

impl From<&RecordEnvelope> for RecordWire {
    fn from(value: &RecordEnvelope) -> Self {
        Self {
            id: value.id.clone(),
            adapter_id: value.adapter_id.clone(),
            protocol_id: value.protocol_id.to_string(),
            port_id: value.port_id.to_string(),
            direction: value.direction,
            payload_ref: value.payload_ref.to_string(),
            head_before: value.head_before.to_string(),
            head_after: value.head_after.to_string(),
            caused_by: value.caused_by.clone(),
            observed_at: value.observed_at.clone(),
        }
    }
}

impl TryFrom<RecordWire> for RecordEnvelope {
    type Error = MaterializerError;

    fn try_from(value: RecordWire) -> Result<Self, Self::Error> {
        let invalid =
            |error: Box<dyn std::fmt::Display>| MaterializerError::Operation(error.to_string());
        Ok(Self {
            id: value.id,
            adapter_id: value.adapter_id,
            protocol_id: ProtocolId::parse(value.protocol_id)
                .map_err(|error| invalid(Box::new(error)))?,
            port_id: PortId::parse(value.port_id).map_err(|error| invalid(Box::new(error)))?,
            direction: value.direction,
            payload_ref: ContentRef::parse(value.payload_ref)
                .map_err(|error| invalid(Box::new(error)))?,
            head_before: parse_computation(&value.head_before)?,
            head_after: parse_computation(&value.head_after)?,
            caused_by: value.caused_by,
            observed_at: value.observed_at,
        })
    }
}

fn load_descriptor(
    reference: &ContentRef,
    objects: &dyn ObjectResolver,
) -> Result<ReplayDescriptor, BundleError> {
    let metadata = objects.metadata(reference)?;
    let bytes = read_exact_object(
        objects,
        reference,
        metadata.size,
        MAX_REPLAY_DESCRIPTOR_BYTES,
    )?;
    let descriptor: ReplayDescriptor = serde_json::from_slice(&bytes).map_err(BundleError::Json)?;
    if serde_jcs::to_vec(&descriptor).map_err(BundleError::Json)? != bytes {
        return Err(BundleError::Json(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "replay descriptor is not canonical JCS",
            ),
        )));
    }
    if descriptor.version != REPLAY_VERSION {
        return Err(BundleError::Json(serde_json::Error::io(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported replay descriptor version",
            ),
        )));
    }
    Ok(descriptor)
}

fn parse_computation(value: &str) -> Result<ComputationRef, MaterializerError> {
    ComputationRef::parse(value).map_err(|error| MaterializerError::Operation(error.to_string()))
}

#[derive(Default)]
pub struct ReplayReferences;

impl MaterializationReferences for ReplayReferences {
    fn materializer_id(&self) -> &str {
        REPLAY_MATERIALIZER_ID
    }

    fn outgoing(
        &self,
        descriptor: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let descriptor = load_descriptor(descriptor, objects)?;
        let mut links = vec![
            ObjectLink::Computation(ComputationRef::parse(descriptor.target).map_err(|error| {
                BundleError::InvalidReference {
                    value: "target".to_owned(),
                    reason: error.to_string(),
                }
            })?),
            ObjectLink::Computation(ComputationRef::parse(descriptor.anchor).map_err(|error| {
                BundleError::InvalidReference {
                    value: "anchor".to_owned(),
                    reason: error.to_string(),
                }
            })?),
        ];
        for record in descriptor.records {
            links.push(ObjectLink::Computation(
                ComputationRef::parse(record.head_before).map_err(|error| {
                    BundleError::InvalidReference {
                        value: "record head_before".to_owned(),
                        reason: error.to_string(),
                    }
                })?,
            ));
            links.push(ObjectLink::Computation(
                ComputationRef::parse(record.head_after).map_err(|error| {
                    BundleError::InvalidReference {
                        value: "record head_after".to_owned(),
                        reason: error.to_string(),
                    }
                })?,
            ));
            links.push(ObjectLink::Content(
                ContentRef::parse(record.payload_ref).map_err(|error| {
                    BundleError::InvalidReference {
                        value: "payload_ref".to_owned(),
                        reason: error.to_string(),
                    }
                })?,
            ));
        }
        Ok(links)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use ato_adapter_api::{
        AdapterAttachContext, AdapterCapabilities, AdapterError, AdapterFactory, AdapterInstance,
        AttachedAdapter,
    };
    use ato_materializer_api::{RealizationDriver, ReplayRuntime};
    use ato_objects::{MemoryObjectStore, ObjectStore};

    use super::*;

    const ADAPTER_ID: &str = "test.replay-audited@1";

    struct TestAdapter;

    impl AdapterFactory for TestAdapter {
        fn id(&self) -> &str {
            ADAPTER_ID
        }

        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                apply: true,
                ..AdapterCapabilities::default()
            }
        }

        fn attach(
            &self,
            _: &AdapterInstance,
            _: &AdapterAttachContext<'_>,
        ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
            unreachable!("descriptor validation does not attach Adapters")
        }
    }

    #[derive(Default)]
    struct DriverState {
        begins: usize,
        applied: Vec<RecordId>,
    }

    struct TestDriver(Arc<Mutex<DriverState>>);

    impl RealizationDriver for TestDriver {
        fn begin(
            &self,
            anchor: &ComputationRef,
        ) -> Result<Box<dyn ReplayRuntime>, MaterializerError> {
            self.0.lock().unwrap().begins += 1;
            Ok(Box::new(TestRuntime {
                state: Arc::clone(&self.0),
                target: anchor.clone(),
            }))
        }
    }

    struct TestRuntime {
        state: Arc<Mutex<DriverState>>,
        target: ComputationRef,
    }

    impl ReplayRuntime for TestRuntime {
        fn apply(&mut self, record: &RecordEnvelope) -> Result<(), MaterializerError> {
            self.state.lock().unwrap().applied.push(record.id.clone());
            self.target = record.head_after.clone();
            Ok(())
        }

        fn finish(
            self: Box<Self>,
            target: &ComputationRef,
        ) -> Result<Box<dyn Realization>, MaterializerError> {
            assert_eq!(&self.target, target);
            Ok(Box::new(TestRealization(target.clone())))
        }
    }

    struct TestRealization(ComputationRef);

    impl Realization for TestRealization {
        fn target(&self) -> &ComputationRef {
            &self.0
        }
        fn activate(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
        fn wait(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
        fn quiesce(&mut self) -> Result<(), MaterializerError> {
            Ok(())
        }
    }

    fn computation(character: char) -> ComputationRef {
        ComputationRef::parse(format!("blake3:{}", character.to_string().repeat(64))).unwrap()
    }

    fn fixture(store: &MemoryObjectStore, break_chain: bool) -> (ContentRef, Vec<RecordEnvelope>) {
        let payload = store.put(b"payload").unwrap();
        let c0 = computation('0');
        let c1 = computation('1');
        let c2 = computation('2');
        let records = vec![
            RecordEnvelope {
                id: RecordId::new("main", 1),
                adapter_id: ADAPTER_ID.to_owned(),
                protocol_id: ProtocolId::parse("test.protocol@1").unwrap(),
                port_id: PortId::parse("port").unwrap(),
                direction: Direction::Inbound,
                payload_ref: payload.clone(),
                head_before: c0.clone(),
                head_after: c1.clone(),
                caused_by: Vec::new(),
                observed_at: "30".to_owned(),
            },
            RecordEnvelope {
                id: RecordId::new("main", 2),
                adapter_id: ADAPTER_ID.to_owned(),
                protocol_id: ProtocolId::parse("test.protocol@1").unwrap(),
                port_id: PortId::parse("port").unwrap(),
                direction: Direction::Outbound,
                payload_ref: payload,
                head_before: if break_chain { computation('9') } else { c1 },
                head_after: c2.clone(),
                caused_by: vec![RecordId::new("main", 1)],
                observed_at: "10".to_owned(),
            },
        ];
        let descriptor = ReplayDescriptor {
            version: REPLAY_VERSION,
            target: c2.to_string(),
            anchor: c0.to_string(),
            records: records.iter().map(RecordWire::from).collect(),
            required_adapters: vec![ADAPTER_ID.to_owned()],
            required_protocols: vec!["test.protocol@1".to_owned()],
            required_bindings: Vec::new(),
            contracts: Vec::new(),
        };
        let bytes = serde_jcs::to_vec(&descriptor).unwrap();
        (store.put(&bytes).unwrap(), records)
    }

    fn registry() -> ato_adapter_api::AdapterRegistry {
        let mut registry = ato_adapter_api::AdapterRegistry::default();
        registry.register(Arc::new(TestAdapter)).unwrap();
        registry
    }

    #[test]
    fn sequence_and_stepper_preserve_descriptor_order_not_observed_at_order() {
        let store = MemoryObjectStore::default();
        let (descriptor, records) = fixture(&store, false);
        let sequence = ReplaySequence::load(&descriptor, &store, &registry()).unwrap();
        assert_eq!(sequence.records(), records);

        let state = Arc::new(Mutex::new(DriverState::default()));
        let mut stepper = ReplayStepper::begin(sequence, &TestDriver(Arc::clone(&state))).unwrap();
        let first = stepper.step().unwrap().unwrap();
        assert_eq!(first.record_id, RecordId::new("main", 1));
        assert_eq!(stepper.cursor(), 1);
        assert_eq!(stepper.current_head(), &records[0].head_after);
        assert_eq!(
            state.lock().unwrap().applied,
            vec![RecordId::new("main", 1)]
        );
        let second = stepper.step().unwrap().unwrap();
        assert_eq!(second.record_id, RecordId::new("main", 2));
        assert_eq!(stepper.current_head(), &records[1].head_after);
        assert!(stepper.step().unwrap().is_none());
        assert_eq!(stepper.finish().unwrap().target(), &records[1].head_after);
    }

    #[test]
    fn invalid_chain_is_rejected_before_driver_begin() {
        let store = MemoryObjectStore::default();
        let (descriptor, _) = fixture(&store, true);
        let state = Arc::new(Mutex::new(DriverState::default()));
        assert!(ReplaySequence::load(&descriptor, &store, &registry()).is_err());
        assert_eq!(state.lock().unwrap().begins, 0);
    }

    #[test]
    fn full_restore_and_incremental_completion_reach_the_same_target() {
        let store = MemoryObjectStore::default();
        let (descriptor, records) = fixture(&store, false);
        let adapters = registry();
        let policy = ato_adapter_api::WorkspaceCapturePolicy::secure_default();

        let full_state = Arc::new(Mutex::new(DriverState::default()));
        let full_driver = TestDriver(Arc::clone(&full_state));
        let context = MaterializerContext {
            objects: &store,
            adapters: &adapters,
            records: &[],
            workspace: Path::new("."),
            workspace_policy: &policy,
            realization: Some(&full_driver),
        };
        let full = ReplayMaterializer.restore(&descriptor, &context).unwrap();

        let incremental_state = Arc::new(Mutex::new(DriverState::default()));
        let incremental_driver = TestDriver(Arc::clone(&incremental_state));
        let sequence = ReplaySequence::load(&descriptor, &store, &adapters).unwrap();
        let mut stepper = ReplayStepper::begin(sequence, &incremental_driver).unwrap();
        while stepper.step().unwrap().is_some() {}
        let incremental = stepper.finish().unwrap();

        assert_eq!(full.target(), incremental.target());
        assert_eq!(full.target(), &records[1].head_after);
        assert_eq!(
            full_state.lock().unwrap().applied,
            incremental_state.lock().unwrap().applied
        );
    }

    #[test]
    fn descriptor_wire_version_and_content_identity_are_unchanged_by_decode() {
        let store = MemoryObjectStore::default();
        let (descriptor, _) = fixture(&store, false);
        let metadata = store.metadata(&descriptor).unwrap();
        let before = read_exact_object(
            &store,
            &descriptor,
            metadata.size,
            MAX_REPLAY_DESCRIPTOR_BYTES,
        )
        .unwrap();
        let sequence = ReplaySequence::load(&descriptor, &store, &registry()).unwrap();
        assert_eq!(sequence.records().len(), 2);
        let after = read_exact_object(
            &store,
            &descriptor,
            metadata.size,
            MAX_REPLAY_DESCRIPTOR_BYTES,
        )
        .unwrap();
        assert_eq!(before, after);
        assert_eq!(
            serde_json::from_slice::<ReplayDescriptor>(&after)
                .unwrap()
                .version,
            1
        );
    }

    #[test]
    fn unknown_adapter_and_required_binding_are_visible_to_fail_closed_policy() {
        let store = MemoryObjectStore::default();
        let (descriptor, _) = fixture(&store, false);
        assert!(
            ReplaySequence::load(
                &descriptor,
                &store,
                &ato_adapter_api::AdapterRegistry::default()
            )
            .is_err()
        );

        let mut raw = load_descriptor(&descriptor, &store).unwrap();
        raw.required_bindings = vec!["creator-secret".to_owned()];
        let bound = store.put(&serde_jcs::to_vec(&raw).unwrap()).unwrap();
        let sequence = ReplaySequence::load(&bound, &store, &registry()).unwrap();
        assert_eq!(sequence.required_bindings(), &["creator-secret"]);
    }
}
