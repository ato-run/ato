//! A Coordinator's service-replacement directive for a live OCI service group.
//!
//! The wire contract (zero-downtime-updates.md section 2a) fixes what the
//! Runner must check before it touches anything, and what it must report:
//!
//! - the control poll may carry an optional
//!   { replacement_id, launch_spec, services } directive;
//! - the Runner applies one replacement_id once per Run and keeps its result
//!   until the report reaches the Coordinator;
//! - a refusal changes nothing and is reported with a stable code.
//!
//! The container mechanics (stop one service, start the next definition on
//! the same network/alias/host port, roll the old definition back) live in the
//! Runtime (ato_runtime_attempt::launch::oci). What stays here is the wire
//! directive, the pre-flight validation, and the per-Run bookkeeping.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, PoisonError, RwLock};

use ato_adapter_oci::{OciOwner, OciServiceGroup, StopBudget};
use ato_ipc::runtime_launch_v2::{OciServiceV2, RuntimeLaunchSpec, RuntimeLaunchSpecV2};
use ato_runtime_attempt::launch::oci::{
    self as oci_launch, ServiceEgressState, ServiceReplacement,
};
use ato_runtime_attempt::launch::process_executor::ReadinessProbe;
use ato_runtime_attempt::launch::resolved::ResolvedRuntimeLaunchContext;

/// Advertised next to the other OCI features. A Coordinator sends the
/// replacement directive only to a Runner that advertises it.
pub(crate) const SERVICE_REPLACEMENT_RUNTIME_FEATURE: &str = "run=ato.service-replacement@1";

/// The body the Runner POSTs to
/// /v1/runner-leases/:id/service-replacements/:replacement_id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ServiceReplacementReport {
    /// "completed" or "failed".
    pub(crate) status: &'static str,
    /// The named services and the container each one runs now, in directive
    /// order. A refused directive touches nothing and reports an empty list.
    pub(crate) services: Vec<ReplacedService>,
    /// Present exactly when status is "failed".
    pub(crate) error: Option<ReplacementErrorReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ReplacedService {
    pub(crate) name: String,
    pub(crate) container_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ReplacementErrorReport {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl ServiceReplacementReport {
    fn completed(services: Vec<ReplacedService>) -> Self {
        Self {
            status: "completed",
            services,
            error: None,
        }
    }

    fn failed(
        code: impl Into<String>,
        message: impl Into<String>,
        services: Vec<ReplacedService>,
    ) -> Self {
        Self {
            status: "failed",
            services,
            error: Some(ReplacementErrorReport {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

/// A deterministic refusal. Nothing was stopped or started when one is
/// returned, and the code is what the Coordinator sees.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refusal {
    code: &'static str,
    message: String,
}

impl Refusal {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The parsed form of a control poll's optional directive. The payload is
/// validated when it is parsed: the Runner never interprets a malformed spec
/// as the nearest known shape.
#[derive(Debug, Clone)]
struct ServiceReplacementDirective {
    launch_spec: RuntimeLaunchSpec,
    services: Vec<String>,
}

impl ServiceReplacementDirective {
    fn parse(value: &serde_json::Value) -> Result<Self, Refusal> {
        let object = value.as_object().ok_or_else(|| {
            Refusal::new("invalid_directive", "the directive is not a JSON object")
        })?;
        if reportable_replacement_id(value).is_none() {
            return Err(Refusal::new(
                "invalid_directive",
                "the directive carries no reportable replacement_id",
            ));
        }
        let launch_spec = object.get("launch_spec").ok_or_else(|| {
            Refusal::new("invalid_directive", "the directive carries no launch_spec")
        })?;
        let raw = serde_json::to_string(launch_spec).map_err(|error| {
            Refusal::new(
                "invalid_launch_spec",
                format!("the directive launch_spec is not JSON: {error}"),
            )
        })?;
        let launch_spec = RuntimeLaunchSpec::parse(&raw).map_err(|error| {
            Refusal::new(
                "invalid_launch_spec",
                format!("the directive launch_spec is not a valid launch spec: {error}"),
            )
        })?;
        let services = object
            .get("services")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                Refusal::new(
                    "invalid_directive",
                    "the directive carries no services list",
                )
            })?
            .iter()
            .map(|name| {
                name.as_str().map(str::to_owned).ok_or_else(|| {
                    Refusal::new(
                        "invalid_directive",
                        "a directive service name is not a string",
                    )
                })
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        Ok(Self {
            launch_spec,
            services,
        })
    }
}

/// A replacement id is a path segment in the report URL: refuse anything that
/// is not the Coordinator's own id alphabet rather than escaping a URL.
fn reportable_replacement_id(value: &serde_json::Value) -> Option<String> {
    let id = value.get("replacement_id")?.as_str()?;
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    valid.then(|| id.to_owned())
}

/// The definition a service is running now, with the launch spec it started
/// under. A rollback starts it again exactly as it was.
#[derive(Debug, Clone)]
struct RunningService {
    service: OciServiceV2,
    spec: RuntimeLaunchSpecV2,
}

/// One named service, with both definitions and the spec the old one ran
/// under.
#[derive(Debug, Clone)]
struct ValidatedService {
    name: String,
    next: OciServiceV2,
    previous: OciServiceV2,
    previous_spec: RuntimeLaunchSpecV2,
}

struct ValidatedReplacement {
    spec: RuntimeLaunchSpecV2,
    services: Vec<ValidatedService>,
}

/// Check every rule from section 2a before anything is touched. The order is
/// fixed: a directive that breaks several rules reports the first one, so the
/// code a Coordinator sees does not depend on map iteration.
fn validate(
    run_id: &str,
    directive: &ServiceReplacementDirective,
    running: &BTreeMap<String, RunningService>,
    running_group: &BTreeSet<String>,
    context: &ResolvedRuntimeLaunchContext,
    fixed_tcp_services: &BTreeSet<String>,
) -> Result<ValidatedReplacement, Refusal> {
    let spec = directive.launch_spec.service_group_spec().ok_or_else(|| {
        Refusal::new(
            "unsupported_realization",
            "the directive launch_spec is not an OCI service group",
        )
    })?;
    let group = spec.service_group();
    if directive.launch_spec.context().run_id != run_id {
        return Err(Refusal::new(
            "run_mismatch",
            format!(
                "the directive launch_spec is for Run {} but this Run is {run_id}",
                directive.launch_spec.context().run_id
            ),
        ));
    }
    if directive.services.is_empty() {
        return Err(Refusal::new(
            "invalid_directive",
            "the directive names no service to replace",
        ));
    }
    let mut named = BTreeSet::new();
    for name in &directive.services {
        if !named.insert(name.as_str()) {
            return Err(Refusal::new(
                "invalid_directive",
                format!("the directive names service {name} more than once"),
            ));
        }
        if !running_group.contains(name) || !running.contains_key(name) {
            return Err(Refusal::new(
                "service_missing",
                format!("service {name} is not part of the running group"),
            ));
        }
        if !group.services.iter().any(|service| service.name == *name) {
            return Err(Refusal::new(
                "invalid_launch_spec",
                format!("service {name} is not declared in the directive launch_spec"),
            ));
        }
    }
    // Every service the directive does not name is unchanged: the same set of
    // names, and the same deserialized definition for each.
    let running_rest = running
        .keys()
        .map(String::as_str)
        .filter(|name| !named.contains(name))
        .collect::<BTreeSet<_>>();
    let next_rest = group
        .services
        .iter()
        .map(|service| service.name.as_str())
        .filter(|name| !named.contains(name))
        .collect::<BTreeSet<_>>();
    if running_rest != next_rest {
        return Err(Refusal::new(
            "service_changed",
            "the directive launch_spec changes the set of services outside the replacement",
        ));
    }
    for name in running_rest {
        let previous = &running[name].service;
        let next = group
            .services
            .iter()
            .find(|service| service.name == name)
            .expect("the unchanged service set was compared");
        if previous != next {
            return Err(Refusal::new(
                "service_changed",
                format!("service {name} is not named by the directive but its definition changed"),
            ));
        }
    }
    let mut services = Vec::with_capacity(directive.services.len());
    for name in &directive.services {
        let previous = running[name].service.clone();
        let next = group
            .services
            .iter()
            .find(|service| service.name == *name)
            .expect("named services were checked")
            .clone();
        if fixed_tcp_services.contains(name.as_str()) {
            return Err(Refusal::new(
                "fixed_tcp_service",
                format!(
                    "service {name} holds a fixed TCP allocation; a replacement would leave the allocation pointing at a stopped container"
                ),
            ));
        }
        if !previous.state_keys.is_empty() || !next.state_keys.is_empty() {
            return Err(Refusal::new(
                "stateful_service",
                format!("service {name} holds state; a service replacement cannot keep it"),
            ));
        }
        if endpoint_shape(&previous) != endpoint_shape(&next) {
            return Err(Refusal::new(
                "endpoint_mismatch",
                format!(
                    "service {name} changes its Endpoint names or guest ports; v1 cannot rebind them"
                ),
            ));
        }
        let secret_names = next
            .secret_grants
            .iter()
            .map(|grant| grant.name.as_str())
            .collect::<BTreeSet<_>>();
        context.secret_environment_for(&secret_names).map_err(|_| {
            let observed = context
                .observed_secret_names()
                .into_iter()
                .collect::<BTreeSet<_>>();
            let missing = secret_names
                .difference(&observed)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            Refusal::new(
                "secret_unavailable",
                format!("service {name} needs a secret this Run did not redeem: {missing}"),
            )
        })?;
        services.push(ValidatedService {
            name: name.clone(),
            next,
            previous,
            previous_spec: running[name].spec.clone(),
        });
    }
    Ok(ValidatedReplacement {
        spec: spec.clone(),
        services,
    })
}

fn endpoint_shape(service: &OciServiceV2) -> Vec<(&str, u16)> {
    let mut shape = service
        .endpoints
        .iter()
        .map(|endpoint| (endpoint.name.as_str(), endpoint.guest_port))
        .collect::<Vec<_>>();
    shape.sort();
    shape
}

/// The live container address behind every endpoint a gated Surface
/// observes. The listener holds this handle for the Run's lifetime, and the
/// service-replacement bookkeeping holds the same one: a replacement repoints
/// the replaced service's entries at the container it leaves running, without
/// restarting the listener or changing what is observable.
#[derive(Debug, Clone, Default)]
pub(crate) struct ObservationTargets(Arc<RwLock<BTreeMap<String, SocketAddr>>>);

impl ObservationTargets {
    pub(crate) fn new(targets: BTreeMap<String, SocketAddr>) -> Self {
        Self(Arc::new(RwLock::new(targets)))
    }

    /// The container an observation-scoped request for this endpoint must
    /// reach. A poisoned lock means a writer panicked; the last written
    /// values stay readable rather than turning a live gate into a 403.
    pub(crate) fn get(&self, endpoint: &str) -> Option<SocketAddr> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(endpoint)
            .copied()
    }

    /// Point every endpoint of `service` at `address`. Only a value changes:
    /// an endpoint the Surface did not start observing is never added, so the
    /// observable set stays exactly what the gate started with.
    fn repoint_service(&self, service: &OciServiceV2, address: IpAddr) {
        let mut targets = self.0.write().unwrap_or_else(PoisonError::into_inner);
        for endpoint in &service.endpoints {
            if let Some(target) = targets.get_mut(&endpoint.name) {
                *target = SocketAddr::new(address, endpoint.guest_port);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct AppliedOutcome {
    report: ServiceReplacementReport,
    reported: bool,
}

/// What one live service group knows about service replacement for the Run's
/// lifetime: the definition each service runs now and the outcome of every
/// replacement_id that was already applied.
pub struct ServiceReplacementRun {
    is_group: bool,
    run_id: String,
    running: BTreeMap<String, RunningService>,
    egress: BTreeMap<String, ServiceEgressState>,
    applied: BTreeMap<String, AppliedOutcome>,
    /// Services the lease's network authorization binds a fixed TCP
    /// allocation to. The allocation is forwarded to the container, so a
    /// replacement would leave it pointing at a stopped container.
    fixed_tcp_services: BTreeSet<String>,
    /// The live observation targets of the Run's gated Surface, once it
    /// starts. A replacement repoints the endpoints it left running.
    observation_targets: Option<ObservationTargets>,
}

/// What the control loop must do with one poll's directive.
pub(crate) enum ProcessedDirective {
    /// POST this body; on success the caller marks the id reported.
    Report {
        replacement_id: String,
        report: ServiceReplacementReport,
    },
    /// Nothing to do: this id's result already reached the Coordinator.
    AlreadyReported,
    /// The directive has no usable replacement id, so there is no address to
    /// report to. The caller logs it; the API contract has no other channel.
    Unreportable(String),
}

impl ServiceReplacementRun {
    /// A live OCI service group: every service of the launch spec is running
    /// under that spec until a directive replaces it.
    pub(crate) fn new(
        spec: &RuntimeLaunchSpecV2,
        egress: BTreeMap<String, ServiceEgressState>,
        fixed_tcp_services: BTreeSet<String>,
    ) -> Self {
        let running = spec
            .service_group()
            .services
            .iter()
            .map(|service| {
                (
                    service.name.clone(),
                    RunningService {
                        service: service.clone(),
                        spec: spec.clone(),
                    },
                )
            })
            .collect();
        Self {
            is_group: true,
            run_id: spec.context.run_id.clone(),
            running,
            egress,
            applied: BTreeMap::new(),
            fixed_tcp_services,
            observation_targets: None,
        }
    }

    /// A Run that is not an OCI service group. It still records a directive's
    /// (refused) outcome so a repeat after a lost report is re-sent, not
    /// re-decided.
    pub(crate) fn not_a_group() -> Self {
        Self {
            is_group: false,
            run_id: String::new(),
            running: BTreeMap::new(),
            egress: BTreeMap::new(),
            applied: BTreeMap::new(),
            fixed_tcp_services: BTreeSet::new(),
            observation_targets: None,
        }
    }

    /// The gated Surface started: it observes through this Run's live
    /// targets, and a replacement repoints the replaced service there.
    pub(crate) fn set_observation_targets(&mut self, targets: ObservationTargets) {
        self.observation_targets = Some(targets);
    }

    /// Handle one control poll's optional directive. Applying happens at most
    /// once per replacement_id; the report is handed back until it is marked
    /// reported.
    pub(crate) fn process(
        &mut self,
        value: &serde_json::Value,
        group: Option<&mut OciServiceGroup>,
        context: &ResolvedRuntimeLaunchContext,
        probe: &dyn ReadinessProbe,
        owner: &OciOwner,
        budget: StopBudget,
    ) -> ProcessedDirective {
        let Some(replacement_id) = reportable_replacement_id(value) else {
            return ProcessedDirective::Unreportable(
                "service replacement directive has no reportable replacement_id".to_owned(),
            );
        };
        if let Some(outcome) = self.applied.get(&replacement_id) {
            return if outcome.reported {
                ProcessedDirective::AlreadyReported
            } else {
                ProcessedDirective::Report {
                    replacement_id,
                    report: outcome.report.clone(),
                }
            };
        }
        let report = match self.apply(value, group, context, probe, owner, budget) {
            Ok(report) => report,
            Err(refusal) => {
                ServiceReplacementReport::failed(refusal.code, refusal.message, Vec::new())
            }
        };
        self.applied.insert(
            replacement_id.clone(),
            AppliedOutcome {
                report: report.clone(),
                reported: false,
            },
        );
        ProcessedDirective::Report {
            replacement_id,
            report,
        }
    }

    /// Record that the Coordinator accepted this outcome. A repeat directive
    /// for the id is then ignored for the rest of the Run.
    pub(crate) fn mark_reported(&mut self, replacement_id: &str) {
        if let Some(outcome) = self.applied.get_mut(replacement_id) {
            outcome.reported = true;
        }
    }

    fn apply(
        &mut self,
        value: &serde_json::Value,
        group: Option<&mut OciServiceGroup>,
        context: &ResolvedRuntimeLaunchContext,
        probe: &dyn ReadinessProbe,
        owner: &OciOwner,
        budget: StopBudget,
    ) -> Result<ServiceReplacementReport, Refusal> {
        if !self.is_group {
            return Err(Refusal::new(
                "unsupported_realization",
                "this Run is not an OCI service group",
            ));
        }
        let Some(group) = group else {
            return Err(Refusal::new(
                "unsupported_realization",
                "this Run has no live OCI service group",
            ));
        };
        let directive = ServiceReplacementDirective::parse(value)?;
        let running_group = group
            .services()
            .map(|(name, _)| name.to_owned())
            .collect::<BTreeSet<_>>();
        let validated = validate(
            &self.run_id,
            &directive,
            &self.running,
            &running_group,
            context,
            &self.fixed_tcp_services,
        )?;
        Ok(self.apply_validated(validated, group, context, probe, owner, budget))
    }

    fn apply_validated(
        &mut self,
        validated: ValidatedReplacement,
        group: &mut OciServiceGroup,
        context: &ResolvedRuntimeLaunchContext,
        probe: &dyn ReadinessProbe,
        owner: &OciOwner,
        budget: StopBudget,
    ) -> ServiceReplacementReport {
        let ValidatedReplacement { spec, services } = validated;
        let mut replaced = Vec::with_capacity(services.len());
        let mut names = Vec::with_capacity(services.len());
        let mut failure = None;
        for service in services {
            names.push(service.name.clone());
            let egress = self.egress.get(&service.name).cloned();
            let outcome = oci_launch::replace_group_service(
                group,
                &spec,
                &service.previous_spec,
                ServiceReplacement {
                    name: &service.name,
                    next: &service.next,
                    previous: &service.previous,
                    egress: egress.as_ref(),
                },
                context,
                probe,
                owner,
                budget,
            );
            match outcome {
                Ok(container_id) => {
                    let name = service.name;
                    self.running.insert(
                        name.clone(),
                        RunningService {
                            service: service.next,
                            spec: spec.clone(),
                        },
                    );
                    replaced.push(ReplacedService { name, container_id });
                }
                Err(error) => {
                    failure = Some((error.code().to_owned(), error.message().to_owned()));
                    break;
                }
            }
        }
        let report = match failure {
            None => ServiceReplacementReport::completed(replaced),
            Some((code, message)) => {
                // The report names every service the directive asked for and
                // the container it runs now: a service whose rollback failed
                // has none, and is left out rather than given a false id.
                let services = names
                    .iter()
                    .filter_map(|name| {
                        group
                            .services()
                            .find(|(service, _)| service == name)
                            .map(|(_, handle)| ReplacedService {
                                name: name.clone(),
                                container_id: handle.container_id().to_owned(),
                            })
                    })
                    .collect();
                ServiceReplacementReport::failed(code, message, services)
            }
        };
        // A successful replacement and a rollback both leave a container
        // running for every attempted service: point the gate at whichever
        // container that is before the outcome reaches the Coordinator.
        self.repoint_observation_targets(group, &names);
        report
    }

    /// Point the gated Surface at the container each attempted service runs
    /// now: the next definition after a successful replacement, the previous
    /// one after a rollback. A service whose rollback also failed has no
    /// running container, and keeps its last target rather than a false one.
    fn repoint_observation_targets(&self, group: &OciServiceGroup, names: &[String]) {
        let Some(targets) = self.observation_targets.as_ref() else {
            return;
        };
        for name in names {
            let Some((_, handle)) = group.services().find(|(service, _)| service == name) else {
                continue;
            };
            let Some(address) = handle.container_address() else {
                continue;
            };
            let Some(running) = self.running.get(name) else {
                continue;
            };
            targets.repoint_service(&running.service, address);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ato_adapter_oci::OciOwner;
    use ato_runtime_attempt::launch::process_executor::LoopbackReadinessProbe;

    const RUN_ID: &str = "run_service_replacement_fixture";

    fn digest(seed: char) -> String {
        format!("sha256:{}", seed.to_string().repeat(64))
    }

    fn group_service(
        name: &str,
        argv: &[&str],
        endpoint: serde_json::Value,
        readiness: serde_json::Value,
        limits: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "image_digest_ref": digest('1'),
            "image_reference": format!("docker.io/library/example@{}", digest('1')),
            "argv": argv,
            "working_dir": "/app",
            "workspace_mount_path": "/app",
            "resource_limits": limits,
            "public_env": [],
            "secret_grants": [],
            "state_keys": [],
            "endpoints": [endpoint],
            "readiness": readiness,
        })
    }

    fn base_spec(run_id: &str) -> serde_json::Value {
        serde_json::json!({
            "protocol": "ato.runtime-launch-spec.v2",
            "context": {
                "compute_id": "cmp_service_replacement",
                "compute_instance_id": "cinst_service_replacement",
                "compute_schema_id": "csch_service_replacement",
                "run_id": run_id,
            },
            "workspace": {
                "cwd_relative": "",
                "materialization_ref": digest('3'),
            },
            "realization": {
                "kind": "oci_service_group",
                "platform": "linux/amd64",
                "services": [
                    group_service(
                        "worker",
                        &["--port", "9000"],
                        serde_json::json!({
                            "name": "worker.http",
                            "protocol": "tcp",
                            "guest_port": 9000,
                            "exposure": "internal",
                        }),
                        serde_json::json!({
                            "kind": "tcp",
                            "endpoint_name": "worker.http",
                            "timeout_ms": 5000,
                        }),
                        serde_json::json!({
                            "memory_bytes": 268435456,
                            "cpu_limit_millis": 500,
                            "pids_limit": 64,
                        }),
                    ),
                    group_service(
                        "web",
                        &["--port", "8000"],
                        serde_json::json!({
                            "name": "app.http",
                            "protocol": "http",
                            "guest_port": 8000,
                            "exposure": "surface",
                        }),
                        serde_json::json!({
                            "kind": "http",
                            "endpoint_name": "app.http",
                            "path": "/",
                            "timeout_ms": 5000,
                        }),
                        serde_json::json!({
                            "memory_bytes": 268435456,
                            "cpu_limit_millis": 500,
                            "pids_limit": 64,
                        }),
                    ),
                ],
                "total_limits": {
                    "memory_bytes": 536870912,
                    "cpu_limit_millis": 1000,
                    "pids_limit": 128,
                },
            },
            "state_attachments": [],
            "lifecycle": {
                "graceful_shutdown_ms": 10000,
                "force_kill_after_ms": 15000,
            },
        })
    }

    fn parse(value: serde_json::Value) -> RuntimeLaunchSpec {
        RuntimeLaunchSpec::parse(&serde_json::to_string(&value).unwrap()).expect("valid spec")
    }

    fn running(spec: &RuntimeLaunchSpec) -> BTreeMap<String, RunningService> {
        let view = spec.service_group_spec().expect("a group");
        view.service_group()
            .services
            .iter()
            .map(|service| {
                (
                    service.name.clone(),
                    RunningService {
                        service: service.clone(),
                        spec: view.clone(),
                    },
                )
            })
            .collect()
    }

    fn running_group(spec: &RuntimeLaunchSpec) -> BTreeSet<String> {
        spec.service_group_spec()
            .expect("a group")
            .service_group()
            .services
            .iter()
            .map(|service| service.name.clone())
            .collect()
    }

    fn directive(spec: serde_json::Value, names: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "replacement_id": "srep_fixture",
            "launch_spec": spec,
            "services": names,
        })
    }

    fn context() -> (tempfile::TempDir, ResolvedRuntimeLaunchContext) {
        let root = tempfile::tempdir().unwrap();
        let context = ResolvedRuntimeLaunchContext::new(
            root.path().to_path_buf(),
            "",
            BTreeMap::new(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap();
        (root, context)
    }

    fn refusal_code(
        current: serde_json::Value,
        next: serde_json::Value,
        names: &[&str],
    ) -> &'static str {
        refusal_code_with_fixed_tcp(current, next, names, &BTreeSet::new())
    }

    fn refusal_code_with_fixed_tcp(
        current: serde_json::Value,
        next: serde_json::Value,
        names: &[&str],
        fixed_tcp_services: &BTreeSet<String>,
    ) -> &'static str {
        let current = parse(current);
        let directive = ServiceReplacementDirective::parse(&directive(next, names)).unwrap();
        let (_root, context) = context();
        validate(
            RUN_ID,
            &directive,
            &running(&current),
            &running_group(&current),
            &context,
            fixed_tcp_services,
        )
        .err()
        .expect("the validation must refuse")
        .code
    }

    fn worker_image(spec: &mut serde_json::Value) {
        spec["realization"]["services"][0]["image_digest_ref"] = digest('4').into();
        spec["realization"]["services"][0]["image_reference"] =
            format!("docker.io/library/example@{}", digest('4')).into();
    }

    #[test]
    fn service_replacement_validation_accepts_a_named_stateless_service() {
        let current = parse(base_spec(RUN_ID));
        let mut next = base_spec(RUN_ID);
        next["realization"]["services"][1]["argv"] =
            serde_json::json!(["--port", "8000", "--verbose"]);
        let directive =
            ServiceReplacementDirective::parse(&directive(next, &["web"])).expect("directive");
        let (_root, context) = context();
        let validated = validate(
            RUN_ID,
            &directive,
            &running(&current),
            &running_group(&current),
            &context,
            &BTreeSet::new(),
        )
        .expect("a stateless named service may be replaced");
        assert_eq!(validated.services.len(), 1);
        assert_eq!(validated.services[0].name, "web");
        assert_ne!(validated.services[0].previous, validated.services[0].next);
    }

    #[test]
    fn service_replacement_validation_refuses_an_absent_service() {
        assert_eq!(
            refusal_code(base_spec(RUN_ID), base_spec(RUN_ID), &["absent"]),
            "service_missing"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_a_change_to_an_unnamed_service() {
        let mut next = base_spec(RUN_ID);
        worker_image(&mut next);
        assert_eq!(
            refusal_code(base_spec(RUN_ID), next, &["web"]),
            "service_changed"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_a_stateful_named_service() {
        let mut current = base_spec(RUN_ID);
        current["state_attachments"] = serde_json::json!([{
            "state_key": "data",
            "revision_ref": null,
            "mount_target": "/data",
            "access": "read_write",
        }]);
        current["realization"]["services"][1]["state_keys"] = serde_json::json!(["data"]);
        assert_eq!(
            refusal_code(current, base_spec(RUN_ID), &["web"]),
            "stateful_service"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_a_new_state_holding_definition() {
        let mut next = base_spec(RUN_ID);
        next["state_attachments"] = serde_json::json!([{
            "state_key": "data",
            "revision_ref": null,
            "mount_target": "/data",
            "access": "read_write",
        }]);
        next["realization"]["services"][1]["state_keys"] = serde_json::json!(["data"]);
        assert_eq!(
            refusal_code(base_spec(RUN_ID), next, &["web"]),
            "stateful_service"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_changed_endpoints() {
        let mut next = base_spec(RUN_ID);
        next["realization"]["services"][1]["endpoints"][0]["guest_port"] = 8080.into();
        next["realization"]["services"][1]["readiness"]["endpoint_name"] =
            serde_json::json!("app.http");
        assert_eq!(
            refusal_code(base_spec(RUN_ID), next, &["web"]),
            "endpoint_mismatch"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_an_unavailable_secret() {
        let mut next = base_spec(RUN_ID);
        next["realization"]["services"][1]["secret_grants"] = serde_json::json!([{
            "name": "ATO_BINDING_WEB_SECRET",
            "grant_ref": "portable-binding:web_secret",
        }]);
        assert_eq!(
            refusal_code(base_spec(RUN_ID), next, &["web"]),
            "secret_unavailable"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_a_different_run() {
        assert_eq!(
            refusal_code(
                base_spec(RUN_ID),
                base_spec("run_service_replacement_other"),
                &["web"],
            ),
            "run_mismatch"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_an_empty_service_list() {
        assert_eq!(
            refusal_code(base_spec(RUN_ID), base_spec(RUN_ID), &[]),
            "invalid_directive"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_a_duplicate_service() {
        assert_eq!(
            refusal_code(base_spec(RUN_ID), base_spec(RUN_ID), &["web", "web"]),
            "invalid_directive"
        );
    }

    #[test]
    fn service_replacement_validation_refuses_a_non_group_spec() {
        let current = parse(base_spec(RUN_ID));
        let v1: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/portable-oci-working-dir.json"
        ))
        .unwrap();
        let directive = ServiceReplacementDirective::parse(&directive(v1, &["web"]))
            .expect("a v1 spec still parses");
        let (_root, context) = context();
        assert_eq!(
            validate(
                RUN_ID,
                &directive,
                &running(&current),
                &running_group(&current),
                &context,
                &BTreeSet::new(),
            )
            .err()
            .expect("a non-group Run cannot be updated by service")
            .code,
            "unsupported_realization"
        );
    }

    #[test]
    fn service_replacement_validation_accepts_a_secret_the_run_redeemed() {
        let mut next = base_spec(RUN_ID);
        next["realization"]["services"][1]["secret_grants"] = serde_json::json!([{
            "name": "ATO_BINDING_WEB_SECRET",
            "grant_ref": "portable-binding:web_secret",
        }]);
        let current = parse(base_spec(RUN_ID));
        let directive =
            ServiceReplacementDirective::parse(&directive(next, &["web"])).expect("directive");
        let root = tempfile::tempdir().unwrap();
        let context = ResolvedRuntimeLaunchContext::new(
            root.path().to_path_buf(),
            "",
            BTreeMap::new(),
            vec![crate::runtime_launch::resolved::ResolvedSecret::new(
                "ATO_BINDING_WEB_SECRET",
                "redeemed-value",
            )],
            vec![],
            vec![],
        )
        .unwrap();
        validate(
            RUN_ID,
            &directive,
            &running(&current),
            &running_group(&current),
            &context,
            &BTreeSet::new(),
        )
        .expect("a redeemed secret is available to the replacement");
    }

    #[test]
    fn service_replacement_validation_refuses_a_malformed_launch_spec() {
        let current = parse(base_spec(RUN_ID));
        let directive = ServiceReplacementDirective::parse(&serde_json::json!({
            "replacement_id": "srep_fixture",
            "launch_spec": {"protocol": "ato.runtime-launch-spec.v2"},
            "services": ["web"],
        }))
        .err()
        .expect("a malformed spec must be refused at parse time");
        assert_eq!(directive.code, "invalid_launch_spec");
        let _ = current;
    }

    #[test]
    fn service_replacement_validation_refuses_a_fixed_tcp_service() {
        let fixed_tcp = BTreeSet::from(["web".to_owned()]);
        assert_eq!(
            refusal_code_with_fixed_tcp(base_spec(RUN_ID), base_spec(RUN_ID), &["web"], &fixed_tcp),
            "fixed_tcp_service"
        );
    }

    #[test]
    fn service_replacement_validation_accepts_a_fixed_tcp_grant_on_an_unnamed_service() {
        let fixed_tcp = BTreeSet::from(["worker".to_owned()]);
        let current = parse(base_spec(RUN_ID));
        let directive = ServiceReplacementDirective::parse(&directive(base_spec(RUN_ID), &["web"]))
            .expect("directive");
        let (_root, context) = context();
        validate(
            RUN_ID,
            &directive,
            &running(&current),
            &running_group(&current),
            &context,
            &fixed_tcp,
        )
        .expect("a fixed TCP grant on a service the directive does not name is not a blocker");
    }

    #[test]
    fn observation_targets_follow_the_replaced_service_through_success_and_rollback() {
        let spec = parse(base_spec(RUN_ID));
        let view = spec.service_group_spec().expect("a group");
        let web = view
            .service_group()
            .services
            .iter()
            .find(|service| service.name == "web")
            .expect("the fixture web service")
            .clone();
        let worker_address: IpAddr = "10.0.0.3".parse().unwrap();
        let old_web: IpAddr = "10.0.0.2".parse().unwrap();
        let next_web: IpAddr = "10.0.0.9".parse().unwrap();
        let restarted_web: IpAddr = "10.0.0.10".parse().unwrap();
        let targets = ObservationTargets::new(BTreeMap::from([
            (
                "worker.http".to_owned(),
                SocketAddr::new(worker_address, 9000),
            ),
            ("app.http".to_owned(), SocketAddr::new(old_web, 8000)),
        ]));
        // A successful replacement runs the next definition in a new container.
        targets.repoint_service(&web, next_web);
        assert_eq!(
            targets.get("app.http"),
            Some(SocketAddr::new(next_web, 8000))
        );
        assert_eq!(
            targets.get("worker.http"),
            Some(SocketAddr::new(worker_address, 9000)),
            "the other service keeps the container it started with"
        );
        // A rollback starts the previous definition again, in yet another container.
        targets.repoint_service(&web, restarted_web);
        assert_eq!(
            targets.get("app.http"),
            Some(SocketAddr::new(restarted_web, 8000))
        );
        assert_eq!(
            targets.get("worker.http"),
            Some(SocketAddr::new(worker_address, 9000))
        );
    }

    #[test]
    fn observation_targets_never_add_an_endpoint_the_surface_did_not_start_with() {
        let spec = parse(base_spec(RUN_ID));
        let view = spec.service_group_spec().expect("a group");
        let worker = view
            .service_group()
            .services
            .iter()
            .find(|service| service.name == "worker")
            .expect("the fixture worker service")
            .clone();
        let targets = ObservationTargets::new(BTreeMap::new());
        targets.repoint_service(&worker, "10.0.0.3".parse().unwrap());
        assert_eq!(targets.get("worker.http"), None);
    }

    #[test]
    fn service_replacement_state_reports_a_repeat_until_the_report_succeeds() {
        let (_root, context) = context();
        let probe = LoopbackReadinessProbe::new(
            reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_millis(50))
                .build()
                .unwrap(),
        );
        let owner = OciOwner {
            runner_id: "runner".into(),
            slot_id: "slot".into(),
            lease_id: "lease".into(),
            run_id: RUN_ID.into(),
            incarnation: "inc".into(),
        };
        let value = directive(base_spec(RUN_ID), &["web"]);
        let mut state = ServiceReplacementRun::not_a_group();
        let first = state.process(&value, None, &context, &probe, &owner, StopBudget::DEFAULT);
        let ProcessedDirective::Report {
            replacement_id,
            report,
        } = first
        else {
            panic!("the first directive must produce a report");
        };
        assert_eq!(replacement_id, "srep_fixture");
        assert_eq!(report.status, "failed");
        assert_eq!(
            report.error.as_ref().map(|error| error.code.as_str()),
            Some("unsupported_realization")
        );
        // The report was lost: the same directive is reported again, not
        // re-applied.
        let retried = state.process(&value, None, &context, &probe, &owner, StopBudget::DEFAULT);
        assert!(matches!(
            retried,
            ProcessedDirective::Report { report, .. } if report == report
        ));
        state.mark_reported(&replacement_id);
        assert!(matches!(
            state.process(&value, None, &context, &probe, &owner, StopBudget::DEFAULT),
            ProcessedDirective::AlreadyReported
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs native Linux Docker; starts, replaces and rolls back isolated group containers"]
    fn service_replacement_docker_group_replaces_one_service_and_rolls_back() {
        use std::time::Duration;

        use ato_ipc::runtime_launch::{EndpointAllocationV1, EndpointV1};
        use ato_runtime_attempt::launch::oci::{
            ServiceReplacement, launch_service_group, replace_group_service,
        };
        use ato_runtime_attempt::launch::resolved::allocate_endpoint;

        const IMAGE: &str = "docker.io/traefik/whoami@sha256:4f90b33ddca9c4d4f06527070d6e503b16d71016edea036842be2a84e60c91cb";
        const RUN_ID: &str = "run_service_replacement_docker";

        fn docker_started_at(container_id: &str) -> String {
            let output = std::process::Command::new("docker")
                .args(["inspect", "--format", "{{.State.StartedAt}}", container_id])
                .output()
                .expect("docker inspect");
            assert!(
                output.status.success(),
                "docker inspect failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("docker inspect output")
                .trim()
                .to_owned()
        }

        fn service(
            name: &str,
            argv: &[&str],
            endpoint: serde_json::Value,
            readiness: serde_json::Value,
        ) -> serde_json::Value {
            let image_digest = IMAGE.rsplit_once('@').expect("pinned digest").1;
            serde_json::json!({
                "name": name,
                "image_digest_ref": image_digest,
                "image_reference": IMAGE,
                "argv": argv,
                "working_dir": "/app",
                "workspace_mount_path": "/app",
                "resource_limits": {
                    "memory_bytes": 134217728,
                    "cpu_limit_millis": 500,
                    "pids_limit": 64,
                },
                "public_env": [],
                "secret_grants": [],
                "state_keys": [],
                "endpoints": [endpoint],
                "readiness": readiness,
            })
        }

        fn spec_with_web_argv(web_argv: &[&str]) -> serde_json::Value {
            serde_json::json!({
                "protocol": "ato.runtime-launch-spec.v2",
                "context": {
                    "compute_id": "cmp_replacement_docker",
                    "compute_instance_id": "cinst_replacement_docker",
                    "compute_schema_id": "csch_replacement_docker",
                    "run_id": RUN_ID,
                },
                "workspace": {
                    "cwd_relative": "",
                    "materialization_ref": digest('5'),
                },
                "realization": {
                    "kind": "oci_service_group",
                    "platform": "linux/amd64",
                    "services": [
                        service(
                            "backend",
                            &["--port", "8000"],
                            serde_json::json!({
                                "name": "backend.tcp",
                                "protocol": "tcp",
                                "guest_port": 8000,
                                "exposure": "internal",
                            }),
                            serde_json::json!({
                                "kind": "tcp",
                                "endpoint_name": "backend.tcp",
                                "timeout_ms": 10000,
                            }),
                        ),
                        service(
                            "web",
                            web_argv,
                            serde_json::json!({
                                "name": "app.http",
                                "protocol": "http",
                                "guest_port": 8000,
                                "exposure": "surface",
                            }),
                            serde_json::json!({
                                "kind": "http",
                                "endpoint_name": "app.http",
                                "path": "/",
                                "timeout_ms": 10000,
                            }),
                        ),
                    ],
                    "total_limits": {
                        "memory_bytes": 268435456,
                        "cpu_limit_millis": 1000,
                        "pids_limit": 128,
                    },
                },
                "state_attachments": [],
                "lifecycle": {
                    "graceful_shutdown_ms": 1000,
                    "force_kill_after_ms": 5000,
                },
            })
        }

        fn parse_group(value: &serde_json::Value) -> RuntimeLaunchSpecV2 {
            RuntimeLaunchSpec::parse(&serde_json::to_string(value).unwrap())
                .expect("valid group spec")
                .service_group_spec()
                .expect("a group")
                .clone()
        }

        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host_port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let workspace = tempfile::tempdir().unwrap();
        let context = ResolvedRuntimeLaunchContext::new(
            workspace.path().to_path_buf(),
            "",
            BTreeMap::new(),
            vec![],
            vec![],
            vec![allocate_endpoint(
                &EndpointV1 {
                    name: "app.http".to_owned(),
                    protocol: "http".to_owned(),
                    guest_port: Some(8000),
                    allocation: EndpointAllocationV1::Automatic,
                    preferred_port: None,
                },
                host_port,
            )],
        )
        .unwrap();
        let spec = parse_group(&spec_with_web_argv(&["--port", "8000"]));
        let owner = OciOwner {
            runner_id: "replacement-fixture".to_owned(),
            slot_id: RUN_ID.to_owned(),
            lease_id: "replacement-lease".to_owned(),
            run_id: spec.context.run_id.clone(),
            incarnation: "replacement-test".to_owned(),
        };
        let probe = LoopbackReadinessProbe::new(
            reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        );
        let budget = StopBudget {
            graceful: Duration::from_secs(1),
            force: Duration::from_secs(1),
        };
        let launched = launch_service_group(&spec, &context, &probe, &owner, vec![], budget)
            .expect("launch two-service group");
        let mut group = launched.group;
        let egress = launched.egress;

        let backend_id = group
            .services()
            .find(|(name, _)| *name == "backend")
            .map(|(_, handle)| handle.container_id().to_owned())
            .expect("backend");
        let web_id = group
            .services()
            .find(|(name, _)| *name == "web")
            .map(|(_, handle)| handle.container_id().to_owned())
            .expect("web");
        let backend_started = docker_started_at(&backend_id);

        // Replace the stateless Surface service. The other service must keep
        // its container and its start time.
        let next = parse_group(&spec_with_web_argv(&["--port=8000"]));
        let next_web = next
            .service_group()
            .services
            .iter()
            .find(|service| service.name == "web")
            .expect("web in the next definition")
            .clone();
        let previous_web = spec
            .service_group()
            .services
            .iter()
            .find(|service| service.name == "web")
            .expect("web in the current definition")
            .clone();
        let replaced_id = replace_group_service(
            &mut group,
            &next,
            &spec,
            ServiceReplacement {
                name: "web",
                next: &next_web,
                previous: &previous_web,
                egress: egress.get("web"),
            },
            &context,
            &probe,
            &owner,
            budget,
        )
        .expect("replace the web service");
        assert_ne!(replaced_id, web_id, "the replacement is a new container");
        assert_eq!(
            group
                .services()
                .find(|(name, _)| *name == "web")
                .map(|(_, handle)| handle.container_id().to_owned()),
            Some(replaced_id.clone())
        );
        assert_eq!(
            group
                .services()
                .find(|(name, _)| *name == "backend")
                .map(|(_, handle)| handle.container_id().to_owned()),
            Some(backend_id.clone()),
            "an unnamed service is never restarted"
        );
        assert_eq!(
            docker_started_at(&backend_id),
            backend_started,
            "an unnamed service keeps its start time"
        );

        // A definition that cannot become ready must roll back to the one
        // that was running, still without touching the sibling.
        let failing = parse_group(&spec_with_web_argv(&["--port=8000", "--not-a-whoami-flag"]));
        let failing_web = failing
            .service_group()
            .services
            .iter()
            .find(|service| service.name == "web")
            .expect("web in the failing definition")
            .clone();
        let failure = replace_group_service(
            &mut group,
            &failing,
            &next,
            ServiceReplacement {
                name: "web",
                next: &failing_web,
                previous: &next_web,
                egress: egress.get("web"),
            },
            &context,
            &probe,
            &owner,
            budget,
        )
        .expect_err("a definition that cannot start must be refused");
        assert_eq!(failure.code(), "replacement_failed", "{failure}");
        let rolled_back = group
            .services()
            .find(|(name, _)| *name == "web")
            .expect("the previous web definition runs again");
        assert_ne!(rolled_back.1.container_id(), replaced_id);
        assert!(
            rolled_back.1.exit_code().unwrap().is_none(),
            "the rolled-back service is running"
        );
        assert_eq!(
            group
                .services()
                .find(|(name, _)| *name == "backend")
                .map(|(_, handle)| handle.container_id().to_owned()),
            Some(backend_id.clone()),
            "the failed replacement did not touch the sibling"
        );
        assert_eq!(docker_started_at(&backend_id), backend_started);

        assert!(
            group
                .stop_gracefully(budget)
                .overall()
                .expect("services stopped")
                .is_confirmed()
        );
        let resources =
            ato_adapter_oci::OwnedResourceScanner::new(&owner.runner_id, &owner.slot_id)
                .unwrap()
                .scan()
                .unwrap();
        assert!(resources.containers.is_empty() && resources.networks.is_empty());
    }
}
