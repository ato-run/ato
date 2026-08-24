//! Generic operation dispatcher. Player resolves one provisionable Actuator
//! route per Record and never simulates domain state across Records.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use ato_adapter_api::{
    Actuator, ActuatorProviderRegistry, ActuatorRoute, AdapterContext, AdapterError,
    OperationRequirement, PortBinding,
};
use ato_objects::{RecordEnvelopeV2, RecordIdV2};
use thiserror::Error;

/// Cached summary derived exclusively from the Record closure.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequiredOperationSet(BTreeSet<OperationRequirement>);

impl RequiredOperationSet {
    pub fn derive(records: &[RecordEnvelopeV2]) -> Self {
        Self(records.iter().map(OperationRequirement::from).collect())
    }

    pub fn iter(&self) -> impl Iterator<Item = &OperationRequirement> {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRecordRoute {
    pub record_id: RecordIdV2,
    pub route: ActuatorRoute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerPreflight {
    pub required_operations: RequiredOperationSet,
    pub routes: Vec<PlannedRecordRoute>,
}

pub struct Player<'a> {
    providers: &'a ActuatorProviderRegistry,
    bindings: &'a [PortBinding],
    context: AdapterContext<'a>,
}

impl<'a> Player<'a> {
    pub fn new(
        providers: &'a ActuatorProviderRegistry,
        bindings: &'a [PortBinding],
        context: AdapterContext<'a>,
    ) -> Self {
        Self {
            providers,
            bindings,
            context,
        }
    }

    /// Resolves exactly one deterministic provisionable route and validates
    /// each payload. It does not provision routes or predict Record effects.
    pub fn preflight(&self, records: &[RecordEnvelopeV2]) -> Result<PlayerPreflight, PlayerError> {
        let mut seen = BTreeSet::new();
        let mut routes = Vec::with_capacity(records.len());
        for record in records {
            if !seen.insert(record.id.clone()) {
                return Err(PlayerError::DuplicateRecord(record.id.clone()));
            }
            let route = self.resolve_route(record)?;
            self.providers
                .get(&route.provider_id)?
                .validate_payload(record, &self.context)?;
            routes.push(PlannedRecordRoute {
                record_id: record.id.clone(),
                route,
            });
        }
        Ok(PlayerPreflight {
            required_operations: RequiredOperationSet::derive(records),
            routes,
        })
    }

    /// Applies Records in writer order. Route resolution is repeated at the
    /// point of each apply, so Core does not infer effects from earlier
    /// `ato.adapter@1` or domain-specific operations.
    pub fn play(&self, records: &[RecordEnvelopeV2]) -> Result<(), PlayerError> {
        self.preflight(records)?;
        let mut actuators: BTreeMap<RouteKey, Box<dyn Actuator>> = BTreeMap::new();
        let mut expected_order = None;
        for record in records {
            if let Some(previous) = expected_order
                && record.writer_order <= previous
            {
                return Err(PlayerError::WriterOrder {
                    previous,
                    actual: record.writer_order,
                });
            }
            expected_order = Some(record.writer_order);
            let route = self.resolve_route(record)?;
            let key = RouteKey::from(&route);
            if !actuators.contains_key(&key) {
                let actuator = self
                    .providers
                    .get(&route.provider_id)?
                    .provision(&route, &self.context)?;
                if actuator.route().provider_id != route.provider_id
                    || actuator.route().route_id != route.route_id
                    || actuator.route().port_id != route.port_id
                {
                    return Err(PlayerError::ProvisionedRouteMismatch {
                        expected: route.route_id,
                        actual: actuator.route().route_id.clone(),
                    });
                }
                actuators.insert(key.clone(), actuator);
            }
            actuators
                .get_mut(&key)
                .expect("Actuator was inserted for the selected route")
                .apply(record, &self.context)?;
        }
        Ok(())
    }

    fn resolve_route(&self, record: &RecordEnvelopeV2) -> Result<ActuatorRoute, PlayerError> {
        let matching_bindings: Vec<_> = self
            .bindings
            .iter()
            .filter(|binding| binding.port_id == record.port_id)
            .collect();
        let binding = match matching_bindings.as_slice() {
            [] => None,
            [binding] => Some(*binding),
            bindings => {
                return Err(PlayerError::AmbiguousBinding {
                    port: record.port_id.to_string(),
                    count: bindings.len(),
                });
            }
        };
        let requirement = OperationRequirement::from(record);
        let mut routes = Vec::new();
        for provider in self.providers.iter() {
            routes.extend(provider.routes(
                &requirement,
                &record.port_id,
                binding,
                &self.context,
            )?);
        }
        match routes.len() {
            0 => Err(PlayerError::NoRoute {
                record: record.id.clone(),
                protocol: record.protocol_id.to_string(),
                operation: record.operation_id.to_string(),
            }),
            1 => Ok(routes.pop().expect("one route")),
            count => Err(PlayerError::AmbiguousRoute {
                record: record.id.clone(),
                count,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteKey {
    provider_id: String,
    route_id: String,
    port_id: String,
}

impl From<&ActuatorRoute> for RouteKey {
    fn from(route: &ActuatorRoute) -> Self {
        Self {
            provider_id: route.provider_id.clone(),
            route_id: route.route_id.clone(),
            port_id: route.port_id.to_string(),
        }
    }
}

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error("Record {0} appears more than once in the replay closure")]
    DuplicateRecord(RecordIdV2),
    #[error("Port `{port}` has {count} Actuator bindings")]
    AmbiguousBinding { port: String, count: usize },
    #[error("Record {record} has no provisionable Actuator route for {protocol}/{operation}")]
    NoRoute {
        record: RecordIdV2,
        protocol: String,
        operation: String,
    },
    #[error("Record {record} has {count} provisionable Actuator routes")]
    AmbiguousRoute { record: RecordIdV2, count: usize },
    #[error("Record writer order must increase (previous {previous}, actual {actual})")]
    WriterOrder { previous: u64, actual: u64 },
    #[error("Actuator provisioned route `{actual}` instead of `{expected}`")]
    ProvisionedRouteMismatch { expected: String, actual: String },
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ato_adapter_api::{ActuatorProvider, SupportedOperation};
    use ato_computation::{ContentRef, OperationId, PortId, ProtocolId};
    use ato_objects::{MemoryObjectStore, ObjectStore, RecordBodyV2};

    use super::*;

    struct FakeProvider {
        id: String,
        operations: Vec<SupportedOperation>,
        applied: Arc<Mutex<Vec<String>>>,
        apply_error: Option<String>,
    }

    impl ActuatorProvider for FakeProvider {
        fn id(&self) -> &str {
            &self.id
        }

        fn supported_operations(&self) -> &[SupportedOperation] {
            &self.operations
        }

        fn validate_payload(
            &self,
            record: &RecordEnvelopeV2,
            context: &AdapterContext<'_>,
        ) -> Result<(), AdapterError> {
            context.objects.metadata(&record.payload_ref)?;
            Ok(())
        }

        fn provision(
            &self,
            route: &ActuatorRoute,
            _context: &AdapterContext<'_>,
        ) -> Result<Box<dyn Actuator>, AdapterError> {
            Ok(Box::new(FakeActuator {
                route: route.clone(),
                applied: Arc::clone(&self.applied),
                apply_error: self.apply_error.clone(),
            }))
        }
    }

    struct FakeActuator {
        route: ActuatorRoute,
        applied: Arc<Mutex<Vec<String>>>,
        apply_error: Option<String>,
    }

    impl Actuator for FakeActuator {
        fn route(&self) -> &ActuatorRoute {
            &self.route
        }

        fn apply(
            &mut self,
            record: &RecordEnvelopeV2,
            _context: &AdapterContext<'_>,
        ) -> Result<(), AdapterError> {
            self.applied
                .lock()
                .unwrap()
                .push(record.operation_id.to_string());
            if let Some(error) = &self.apply_error {
                return Err(AdapterError::Operation(error.clone()));
            }
            Ok(())
        }
    }

    fn supported(protocol: &str, operation: &str) -> SupportedOperation {
        SupportedOperation {
            protocol_id: ProtocolId::parse(protocol).unwrap(),
            operation_id: OperationId::parse(operation).unwrap(),
            payload_version: 1,
            required_features: BTreeSet::new(),
        }
    }

    fn record(
        payload_ref: &ContentRef,
        protocol: &str,
        operation: &str,
        port: &str,
        writer_order: u64,
        recorded_by: Option<&str>,
    ) -> RecordEnvelopeV2 {
        RecordEnvelopeV2::seal(RecordBodyV2 {
            protocol_id: ProtocolId::parse(protocol).unwrap(),
            operation_id: OperationId::parse(operation).unwrap(),
            port_id: PortId::parse(port).unwrap(),
            payload_ref: payload_ref.clone(),
            payload_version: 1,
            required_features: BTreeSet::new(),
            recorded_by: recorded_by.map(str::to_owned),
            stream: "main".to_owned(),
            local_seq: writer_order,
            writer_order,
            caused_by: Vec::new(),
            observed_at: "2030-01-01T00:00:00Z".to_owned(),
        })
        .unwrap()
    }

    #[test]
    fn port_binding_selects_one_provider_independent_of_recording_implementation() {
        let objects = MemoryObjectStore::default();
        let payload = objects.put(b"{}").unwrap();
        let chrome_applied = Arc::new(Mutex::new(Vec::new()));
        let firefox_applied = Arc::new(Mutex::new(Vec::new()));
        let mut providers = ActuatorProviderRegistry::default();
        providers
            .register(Arc::new(FakeProvider {
                id: "browser.chrome@1".to_owned(),
                operations: vec![supported("ato.browser@1", "click")],
                applied: Arc::clone(&chrome_applied),
                apply_error: None,
            }))
            .unwrap();
        providers
            .register(Arc::new(FakeProvider {
                id: "browser.firefox@1".to_owned(),
                operations: vec![supported("ato.browser@1", "click")],
                applied: Arc::clone(&firefox_applied),
                apply_error: None,
            }))
            .unwrap();
        let bindings = [PortBinding {
            port_id: PortId::parse("ui.main").unwrap(),
            provider_id: "browser.firefox@1".to_owned(),
            route_id: "firefox.default".to_owned(),
        }];
        let record = record(
            &payload,
            "ato.browser@1",
            "click",
            "ui.main",
            1,
            Some("browser.chrome@1"),
        );
        let player = Player::new(
            &providers,
            &bindings,
            AdapterContext {
                workspace: std::path::Path::new("."),
                objects: &objects,
            },
        );

        let preflight = player.preflight(std::slice::from_ref(&record)).unwrap();
        player.play(&[record]).unwrap();

        assert_eq!(preflight.routes[0].route.provider_id, "browser.firefox@1");
        assert!(chrome_applied.lock().unwrap().is_empty());
        assert_eq!(*firefox_applied.lock().unwrap(), vec!["click"]);
    }

    #[test]
    fn preflight_fails_when_multiple_routes_have_no_planner_binding() {
        let objects = MemoryObjectStore::default();
        let payload = objects.put(b"{}").unwrap();
        let mut providers = ActuatorProviderRegistry::default();
        for id in ["browser.chrome@1", "browser.firefox@1"] {
            providers
                .register(Arc::new(FakeProvider {
                    id: id.to_owned(),
                    operations: vec![supported("ato.browser@1", "click")],
                    applied: Arc::new(Mutex::new(Vec::new())),
                    apply_error: None,
                }))
                .unwrap();
        }
        let record = record(&payload, "ato.browser@1", "click", "ui.main", 1, None);
        let player = Player::new(
            &providers,
            &[],
            AdapterContext {
                workspace: std::path::Path::new("."),
                objects: &objects,
            },
        );

        assert!(matches!(
            player.preflight(&[record]),
            Err(PlayerError::AmbiguousRoute { count: 2, .. })
        ));
    }

    #[test]
    fn player_propagates_actuator_error_without_simulating_prior_record_semantics() {
        let objects = MemoryObjectStore::default();
        let payload = objects.put(b"{}").unwrap();
        let mut providers = ActuatorProviderRegistry::default();
        providers
            .register(Arc::new(FakeProvider {
                id: "adapter.control@1".to_owned(),
                operations: vec![
                    supported("ato.adapter@1", "add"),
                    supported("ato.adapter@1", "remove"),
                ],
                applied: Arc::new(Mutex::new(Vec::new())),
                apply_error: None,
            }))
            .unwrap();
        let browser_applied = Arc::new(Mutex::new(Vec::new()));
        providers
            .register(Arc::new(FakeProvider {
                id: "browser.runtime@1".to_owned(),
                operations: vec![supported("ato.browser@1", "click")],
                applied: Arc::clone(&browser_applied),
                apply_error: Some("browser actuator is not active".to_owned()),
            }))
            .unwrap();
        let records = vec![
            record(&payload, "ato.adapter@1", "add", "control.main", 1, None),
            record(&payload, "ato.adapter@1", "remove", "control.main", 2, None),
            record(&payload, "ato.browser@1", "click", "ui.main", 3, None),
        ];
        let player = Player::new(
            &providers,
            &[],
            AdapterContext {
                workspace: std::path::Path::new("."),
                objects: &objects,
            },
        );

        let error = player.play(&records).unwrap_err();

        assert!(error.to_string().contains("browser actuator is not active"));
        assert_eq!(*browser_applied.lock().unwrap(), vec!["click"]);
    }
}
