use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_adapter_api::{AdapterInstance, AdapterObservation, WorkspaceCapturePolicy};
use ato_adapter_binding::{BINDING_ADAPTER_ID, BindingAdapterConfig};
use ato_adapter_http::{HTTP_ADAPTER_ID, HttpAdapterConfig};
use ato_adapter_process::{PROCESS_ADAPTER_ID, ProcessSpec};
use ato_adapter_pty::{PTY_ADAPTER_ID, PtyAdapterConfig};
use ato_adapter_workspace::{
    WorkspaceMutation, WorkspaceSnapshot, capture_workspace_with_policy, encode_mutation,
};
use ato_compose::{
    COMPOSE_SEMANTICS_ID, CompositeResidual, Connection, Endpoint, NodeId,
    decode_composite_residual, encode_composite_residual,
};
use ato_computation::{
    Boundary, ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId, RoleId,
    SemanticsId, computation_ref, encode_computation_object,
};
use ato_objects::{
    Direction, LocalCapsuleRepository, ObjectResolver, ObjectStore, RecordEnvelope, RecordId,
    read_exact_object, resolve_computation,
};
use serde::{Deserialize, Serialize};

pub(crate) const AUTHORING_SEMANTICS_ID: &str = "ato.authoring@1";
const AUTHORING_STATE_VERSION: u32 = 1;
const MAX_AUTHORING_STATE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKSPACE_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthoringConfig {
    pub schema: u32,
    #[serde(default)]
    pub process: Vec<ProcessConfig>,
    #[serde(default)]
    pub adapter: Vec<AdapterConfig>,
    #[serde(default)]
    pub port: Vec<PortConfig>,
    #[serde(default)]
    pub connection: Vec<ConnectionConfig>,
    #[serde(default)]
    pub binding: Vec<BindingConfig>,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub encap: EncapConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessConfig {
    pub id: String,
    pub command: Vec<String>,
    #[serde(default = "default_cwd")]
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdapterConfig {
    #[serde(rename = "use")]
    pub use_adapter: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub port: Option<String>,
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub ready_path: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortConfig {
    pub id: String,
    pub node: String,
    pub protocol: String,
    pub role: String,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub internal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConnectionConfig {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BindingConfig {
    pub id: String,
    pub environment: String,
    #[serde(default = "default_binding_protocol")]
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct EncapConfig {
    #[serde(default)]
    pub materializers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkspaceConfig {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthoringState {
    version: u32,
    pub config: AuthoringConfig,
    pub workspace_snapshot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    semantic_frontier: Option<String>,
}

#[derive(Serialize)]
struct SemanticTransition<'a> {
    version: u32,
    prior_frontier: Option<&'a str>,
    adapter_id: &'a str,
    protocol_id: String,
    port_id: String,
    direction: Direction,
    payload_ref: String,
}

fn default_cwd() -> PathBuf {
    PathBuf::from(".")
}

fn default_binding_protocol() -> String {
    "ato.binding@1".to_owned()
}

pub(crate) fn load_config(project: &Path) -> Result<AuthoringConfig> {
    let path = project.join("capsule.toml");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read authoring config {}", path.display()))?;
    let config: AuthoringConfig = toml::from_str(&text)
        .with_context(|| format!("invalid authoring config {}", path.display()))?;
    if config.schema != 1 {
        bail!(
            "unsupported capsule.toml schema {}; expected 1",
            config.schema
        );
    }
    for process in &config.process {
        if process.id.is_empty() || process.command.is_empty() {
            bail!("every process requires an id and non-empty command argv");
        }
    }
    let process_ids: BTreeSet<_> = config
        .process
        .iter()
        .map(|process| process.id.as_str())
        .collect();
    let ports: BTreeSet<_> = config.port.iter().map(|port| port.id.as_str()).collect();
    if ports.len() != config.port.len() {
        bail!("port ids must be unique");
    }
    for port in &config.port {
        if !process_ids.contains(port.node.as_str()) {
            bail!(
                "port `{}` names unknown owner node `{}`",
                port.id,
                port.node
            );
        }
        if port.environment.as_deref().is_some_and(str::is_empty) {
            bail!("port `{}` has an empty environment projection", port.id);
        }
        if port.internal
            && port.role == "client"
            && !config
                .connection
                .iter()
                .any(|connection| connection.from == port.id || connection.to == port.id)
        {
            bail!("internal client port `{}` is unwired", port.id);
        }
    }
    for connection in &config.connection {
        if connection.from == connection.to
            || !ports.contains(connection.from.as_str())
            || !ports.contains(connection.to.as_str())
        {
            bail!(
                "connection endpoints must name two distinct declared ports: {} -> {}",
                connection.from,
                connection.to
            );
        }
        let from = config
            .port
            .iter()
            .find(|port| port.id == connection.from)
            .expect("validated port");
        let to = config
            .port
            .iter()
            .find(|port| port.id == connection.to)
            .expect("validated port");
        if !from.internal || !to.internal || from.protocol != to.protocol {
            bail!("connections require internal ports with the same protocol");
        }
    }
    workspace_policy(&config)?;
    Ok(config)
}

pub(crate) fn workspace_policy(config: &AuthoringConfig) -> Result<WorkspaceCapturePolicy> {
    WorkspaceCapturePolicy::new(
        config.workspace.include.clone(),
        config.workspace.exclude.clone(),
    )
    .map_err(Into::into)
}

pub(crate) fn initial_computation(
    repository: &LocalCapsuleRepository,
    config: AuthoringConfig,
) -> Result<ComputationRef> {
    let policy = workspace_policy(&config)?;
    let snapshot =
        capture_workspace_with_policy(repository.project(), repository.objects(), &policy)?;
    seal_authored_root(repository.objects(), config, snapshot)
}

fn seal_authored_root(
    objects: &dyn ObjectStore,
    config: AuthoringConfig,
    snapshot: ContentRef,
) -> Result<ComputationRef> {
    if config.process.len() < 2 && config.connection.is_empty() {
        return seal_state(objects, config, snapshot);
    }
    let owner = |port: &str| -> Result<String> {
        config
            .port
            .iter()
            .find(|candidate| candidate.id == port)
            .map(|candidate| candidate.node.clone())
            .with_context(|| format!("unknown port `{port}`"))
    };
    let mut nodes = std::collections::BTreeMap::new();
    for process in &config.process {
        let ports: Vec<_> = config
            .port
            .iter()
            .filter(|port| port.node == process.id)
            .cloned()
            .collect();
        let port_ids: BTreeSet<_> = ports.iter().map(|port| port.id.as_str()).collect();
        let adapters = config
            .adapter
            .iter()
            .filter(|adapter| {
                adapter.target.as_deref() == Some(process.id.as_str())
                    || adapter
                        .port
                        .as_deref()
                        .is_some_and(|port| port_ids.contains(port))
                    || adapter.target.as_deref() == Some("workspace")
            })
            .cloned()
            .collect();
        let child = AuthoringConfig {
            schema: config.schema,
            process: vec![process.clone()],
            adapter: adapters,
            port: ports,
            connection: Vec::new(),
            binding: config.binding.clone(),
            workspace: config.workspace.clone(),
            encap: config.encap.clone(),
        };
        nodes.insert(
            NodeId::parse(&process.id)?,
            seal_state(objects, child, snapshot.clone())?,
        );
    }
    let endpoint = |port: &str| -> Result<Endpoint> {
        Ok(Endpoint {
            node: NodeId::parse(owner(port)?)?,
            port: PortId::parse(port)?,
        })
    };
    let connections = config
        .connection
        .iter()
        .map(|connection| {
            Connection::new(endpoint(&connection.from)?, endpoint(&connection.to)?)
                .map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    let exports = config
        .port
        .iter()
        .filter(|port| !port.internal)
        .map(|port| Ok((PortId::parse(&port.id)?, endpoint(&port.id)?)))
        .collect::<Result<_>>()?;
    seal_composite(objects, nodes, connections, exports, boundary(&config)?)
}

pub(crate) fn adapter_instances(
    config: &AuthoringConfig,
    bindings: &BTreeMap<String, String>,
    isolated_processes: bool,
    emit_initial_events: bool,
) -> Result<Vec<AdapterInstance>> {
    let base_environment: BTreeMap<_, _> = config
        .binding
        .iter()
        .filter_map(|binding| {
            bindings
                .get(&binding.id)
                .map(|value| (binding.environment.clone(), value.clone()))
        })
        .collect();
    let mut instances = Vec::new();
    let pty_targets: BTreeSet<_> = config
        .adapter
        .iter()
        .filter(|adapter| adapter.use_adapter == PTY_ADAPTER_ID)
        .filter_map(|adapter| adapter.target.as_deref())
        .collect();
    for (index, adapter) in config.adapter.iter().enumerate() {
        let value = match adapter.use_adapter.as_str() {
            PROCESS_ADAPTER_ID => {
                let target = adapter
                    .target
                    .as_deref()
                    .context("ato.process@1 adapter requires target")?;
                if pty_targets.contains(target) {
                    continue;
                }
                let process = config
                    .process
                    .iter()
                    .find(|process| process.id == target)
                    .with_context(|| format!("unknown process adapter target `{target}`"))?;
                let environment = process_environment(config, target, &base_environment)?;
                serde_json::to_value(ProcessSpec {
                    id: process.id.clone(),
                    command: process.command.clone(),
                    cwd: process.cwd.clone(),
                    environment: environment.clone(),
                    isolated_group: isolated_processes,
                })?
            }
            HTTP_ADAPTER_ID => serde_json::to_value(HttpAdapterConfig {
                listen: adapter
                    .listen
                    .as_deref()
                    .context("ato.http@1 adapter requires listen")?
                    .parse()?,
                upstream: adapter
                    .upstream
                    .as_deref()
                    .context("ato.http@1 adapter requires upstream")?
                    .parse()?,
                port_id: adapter
                    .port
                    .clone()
                    .context("ato.http@1 adapter requires port")?,
                ready_path: adapter.ready_path.clone(),
            })?,
            PTY_ADAPTER_ID => {
                let target = adapter
                    .target
                    .as_deref()
                    .context("ato.pty@1 adapter requires target")?;
                let process = config
                    .process
                    .iter()
                    .find(|process| process.id == target)
                    .with_context(|| format!("unknown PTY adapter target `{target}`"))?;
                let environment = process_environment(config, target, &base_environment)?;
                serde_json::to_value(PtyAdapterConfig {
                    command: process.command.clone(),
                    cwd: process.cwd.clone(),
                    environment: environment.clone(),
                    initial_input: emit_initial_events.then(|| adapter.input.clone()).flatten(),
                })?
            }
            _ => lower_generic_adapter_config(adapter)?,
        };
        instances.push(AdapterInstance {
            instance_id: format!("configured.{index}"),
            adapter_id: adapter.use_adapter.clone(),
            config: value,
        });
    }
    for binding in &config.binding {
        if bindings.contains_key(&binding.id) {
            instances.push(AdapterInstance {
                instance_id: format!("binding.{}", binding.id),
                adapter_id: BINDING_ADAPTER_ID.to_owned(),
                config: serde_json::to_value(BindingAdapterConfig {
                    binding_id: binding.id.clone(),
                    protocol: binding.protocol.clone(),
                    provider_ref: format!("runtime-binding:{}", binding.id),
                    port_id: format!("binding.{}", binding.id),
                })?,
            });
        }
    }
    Ok(instances)
}

fn lower_generic_adapter_config(adapter: &AdapterConfig) -> Result<serde_json::Value> {
    let mut value = match adapter.config.clone() {
        serde_json::Value::Null => serde_json::Value::Object(Default::default()),
        serde_json::Value::Object(value) => serde_json::Value::Object(value),
        _ => bail!("Adapter config must be a table"),
    };
    if let Some(port) = &adapter.port {
        let object = value.as_object_mut().expect("generic config is an object");
        match object.get("port_id") {
            Some(existing) if existing != port => {
                bail!("Adapter port and config.port_id must match")
            }
            Some(_) => {}
            None => {
                object.insert(
                    "port_id".to_owned(),
                    serde_json::Value::String(port.clone()),
                );
            }
        }
    }
    Ok(value)
}

fn process_environment(
    config: &AuthoringConfig,
    target: &str,
    base: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut environment = base.clone();
    for port in config.port.iter().filter(|port| port.node == target) {
        if let (Some(name), Some(address)) = (&port.environment, &port.address) {
            environment.insert(name.clone(), address.clone());
        }
    }
    for connection in &config.connection {
        let from = config
            .port
            .iter()
            .find(|port| port.id == connection.from)
            .context("validated connection source")?;
        let to = config
            .port
            .iter()
            .find(|port| port.id == connection.to)
            .context("validated connection target")?;
        project_peer_address(&mut environment, target, from, to)?;
        project_peer_address(&mut environment, target, to, from)?;
    }
    Ok(environment)
}

fn project_peer_address(
    environment: &mut BTreeMap<String, String>,
    target: &str,
    local: &PortConfig,
    peer: &PortConfig,
) -> Result<()> {
    if local.node != target || local.role != "client" {
        return Ok(());
    }
    let Some(name) = &local.environment else {
        return Ok(());
    };
    let address = peer.address.as_ref().with_context(|| {
        format!(
            "connected peer port `{}` requires a physical address for `{}`",
            peer.id, local.id
        )
    })?;
    environment.insert(name.clone(), address.clone());
    Ok(())
}

pub(crate) fn load_state(
    reference: &ComputationRef,
    objects: &dyn ObjectResolver,
) -> Result<AuthoringState> {
    let computation = resolve_computation(objects, reference)?;
    if computation.object().semantics != SemanticsId::parse(AUTHORING_SEMANTICS_ID)? {
        bail!(
            "computation {} is not an authored process computation",
            reference
        );
    }
    let residual = &computation.object().residual;
    let metadata = objects.metadata(residual)?;
    let bytes = read_exact_object(objects, residual, metadata.size, MAX_AUTHORING_STATE_BYTES)?;
    let canonical_value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&canonical_value)? != bytes {
        bail!("authoring state is non-canonical or unsupported");
    }
    let state = match serde_json::from_value::<AuthoringState>(canonical_value.clone()) {
        Ok(state) => state,
        Err(current_error) => {
            decode_legacy_authoring_state(canonical_value).with_context(|| {
                format!("authoring state is unsupported by the current schema: {current_error}")
            })?
        }
    };
    if state.version != AUTHORING_STATE_VERSION {
        bail!("authoring state is non-canonical or unsupported");
    }
    Ok(state)
}

/// Reads the schema-1 authoring residual emitted by the pre-Record-v2 CLI.
///
/// That writer serialized an adapter-level `config: null` member.  The member
/// never carried protocol semantics for these objects, but it remains part of
/// their already-sealed ComputationRef preimage.  We therefore verify the
/// original canonical bytes first, accept only a literal null, and remove it
/// solely in the runtime projection.  The ComputationRef is never resealed or
/// derived from this compatibility step.
fn decode_legacy_authoring_state(mut value: serde_json::Value) -> Result<AuthoringState> {
    let adapters = value
        .get_mut("config")
        .and_then(|config| config.get_mut("adapter"))
        .and_then(serde_json::Value::as_array_mut)
        .context("legacy authoring state has no adapter array")?;
    let mut removed = false;
    for adapter in adapters {
        let object = adapter
            .as_object_mut()
            .context("legacy authoring adapter is not an object")?;
        if let Some(config) = object.remove("config") {
            if !config.is_null() {
                bail!("legacy authoring adapter config must be null");
            }
            removed = true;
        }
    }
    if !removed {
        bail!("legacy authoring adapter config marker is absent");
    }
    let processes = value
        .get_mut("config")
        .and_then(|config| config.get_mut("process"))
        .and_then(serde_json::Value::as_array_mut)
        .context("legacy authoring state has no process array")?;
    for process in processes {
        let object = process
            .as_object_mut()
            .context("legacy authoring process is not an object")?;
        if let Some(capture) = object.remove("capture")
            && capture != serde_json::Value::String("unsupported".to_owned())
        {
            bail!("legacy authoring process capture mode is unsupported");
        }
    }
    Ok(serde_json::from_value(value)?)
}

pub(crate) fn load_runtime_state(
    reference: &ComputationRef,
    objects: &dyn ObjectResolver,
) -> Result<AuthoringState> {
    let computation = resolve_computation(objects, reference)?;
    if computation.object().semantics == SemanticsId::parse(AUTHORING_SEMANTICS_ID)? {
        return load_state(reference, objects);
    }
    if computation.object().semantics != SemanticsId::parse(COMPOSE_SEMANTICS_ID)? {
        bail!("computation {reference} has no authoring runtime projection");
    }
    let composite = load_composite(reference, objects)?;
    let mut states = composite
        .nodes
        .values()
        .map(|child| load_runtime_state(child, objects));
    let mut merged = states.next().context("composite has no child nodes")??;
    for state in states {
        let state = state?;
        if state.workspace_snapshot != merged.workspace_snapshot {
            bail!("composite children do not share one workspace snapshot");
        }
        merged.config.process.extend(state.config.process);
        merged.config.adapter.extend(state.config.adapter);
        for port in state.config.port {
            if !merged
                .config
                .port
                .iter()
                .any(|existing| existing.id == port.id)
            {
                merged.config.port.push(port);
            }
        }
        for binding in state.config.binding {
            if !merged
                .config
                .binding
                .iter()
                .any(|existing| existing.id == binding.id)
            {
                merged.config.binding.push(binding);
            }
        }
    }
    merged.config.connection = composite
        .connections
        .iter()
        .map(|connection| ConnectionConfig {
            from: connection.first().port.to_string(),
            to: connection.second().port.to_string(),
        })
        .collect();
    Ok(merged)
}

fn load_composite(
    reference: &ComputationRef,
    objects: &dyn ObjectResolver,
) -> Result<CompositeResidual> {
    let computation = resolve_computation(objects, reference)?;
    let residual = &computation.object().residual;
    let metadata = objects.metadata(residual)?;
    let bytes = read_exact_object(objects, residual, metadata.size, 16 * 1024 * 1024)?;
    Ok(decode_composite_residual(&bytes)?)
}

pub(crate) fn evolve_workspace(
    repository: &LocalCapsuleRepository,
    branch: &str,
    start: &ComputationRef,
) -> Result<ComputationRef> {
    let runtime = load_runtime_state(start, repository.objects())?;
    let before = load_snapshot(&runtime.workspace_snapshot, repository.objects())?;
    let policy = workspace_policy(&runtime.config)?;
    let final_ref =
        capture_workspace_with_policy(repository.project(), repository.objects(), &policy)?;
    let after = load_snapshot(final_ref.as_str(), repository.objects())?;
    if before == after {
        return Ok(start.clone());
    }
    if resolve_computation(repository.objects(), start)?
        .object()
        .semantics
        == SemanticsId::parse(COMPOSE_SEMANTICS_ID)?
    {
        let paths: BTreeSet<_> = before
            .files
            .keys()
            .chain(after.files.keys())
            .cloned()
            .collect();
        let mut files = before.files;
        let mut head = start.clone();
        let mut previous = repository
            .records_for_stream(branch, None)?
            .last()
            .map(|record| record.id.clone());
        for path in paths {
            let mutation = match (files.get(&path), after.files.get(&path)) {
                (left, right) if left == right => continue,
                (_, Some(content)) => {
                    files.insert(path.clone(), content.clone());
                    WorkspaceMutation::Put {
                        path,
                        content: content.clone(),
                    }
                }
                (Some(_), None) => {
                    files.remove(&path);
                    WorkspaceMutation::Delete { path }
                }
                (None, None) => continue,
            };
            let snapshot = repository
                .objects()
                .put(&serde_jcs::to_vec(&WorkspaceSnapshot {
                    files: files.clone(),
                })?)?;
            let next = evolve_composite_snapshot(repository.objects(), &head, &snapshot)?;
            let payload = repository.objects().put(&encode_mutation(&mutation)?)?;
            let record = repository.append_record(RecordEnvelope {
                id: RecordId::new(branch, 0),
                adapter_id: "ato.workspace@1".to_owned(),
                protocol_id: ProtocolId::parse("ato.workspace@1")?,
                port_id: PortId::parse("workspace.main")?,
                direction: Direction::Inbound,
                payload_ref: payload,
                head_before: head,
                head_after: next.clone(),
                caused_by: previous.into_iter().collect(),
                observed_at: observed_at(),
            })?;
            previous = Some(record.id);
            head = next;
        }
        return Ok(head);
    }
    let mut state = load_state(start, repository.objects())?;

    let paths: BTreeSet<_> = before
        .files
        .keys()
        .chain(after.files.keys())
        .cloned()
        .collect();
    let mut current_files = before.files;
    let mut head = start.clone();
    let mut previous_record = None;
    for path in paths {
        let mutation = match (current_files.get(&path), after.files.get(&path)) {
            (left, right) if left == right => continue,
            (_, Some(content)) => {
                current_files.insert(path.clone(), content.clone());
                WorkspaceMutation::Put {
                    path,
                    content: content.clone(),
                }
            }
            (Some(_), None) => {
                current_files.remove(&path);
                WorkspaceMutation::Delete { path }
            }
            (None, None) => continue,
        };
        let snapshot = repository
            .objects()
            .put(&serde_jcs::to_vec(&WorkspaceSnapshot {
                files: current_files.clone(),
            })?)?;
        state.workspace_snapshot = snapshot.to_string();
        let next = seal_authoring_state(repository.objects(), state.clone())?;
        let payload = repository.objects().put(&encode_mutation(&mutation)?)?;
        let record = repository.append_record(RecordEnvelope {
            id: RecordId::new(branch, 0),
            adapter_id: "ato.workspace@1".to_owned(),
            protocol_id: ProtocolId::parse("ato.workspace@1")?,
            port_id: PortId::parse("workspace.main")?,
            direction: Direction::Inbound,
            payload_ref: payload,
            head_before: head.clone(),
            head_after: next.clone(),
            caused_by: previous_record.into_iter().collect(),
            observed_at: observed_at(),
        })?;
        previous_record = Some(record.id);
        head = next;
    }
    Ok(head)
}

fn evolve_composite_snapshot(
    objects: &dyn ObjectStore,
    reference: &ComputationRef,
    snapshot: &ContentRef,
) -> Result<ComputationRef> {
    let resolved = resolve_computation(objects, reference)?;
    let mut composite = load_composite(reference, objects)?;
    for child in composite.nodes.values_mut() {
        let semantics = resolve_computation(objects, child)?
            .object()
            .semantics
            .clone();
        *child = if semantics == SemanticsId::parse(AUTHORING_SEMANTICS_ID)? {
            let mut state = load_state(child, objects)?;
            state.workspace_snapshot = snapshot.to_string();
            seal_authoring_state(objects, state)?
        } else {
            evolve_composite_snapshot(objects, child, snapshot)?
        };
    }
    seal_composite(
        objects,
        composite.nodes,
        composite.connections,
        composite.exports,
        resolved.object().boundary.clone(),
    )
}

pub(crate) fn seal_state(
    objects: &dyn ObjectStore,
    config: AuthoringConfig,
    workspace_snapshot: ContentRef,
) -> Result<ComputationRef> {
    let state = AuthoringState {
        version: AUTHORING_STATE_VERSION,
        config,
        workspace_snapshot: workspace_snapshot.to_string(),
        semantic_frontier: None,
    };
    seal_authoring_state(objects, state)
}

fn seal_authoring_state(
    objects: &dyn ObjectStore,
    state: AuthoringState,
) -> Result<ComputationRef> {
    let boundary = boundary(&state.config)?;
    let residual = objects.put(&serde_jcs::to_vec(&state)?)?;
    let computation = ComputationObject {
        semantics: SemanticsId::parse(AUTHORING_SEMANTICS_ID)?,
        boundary,
        residual,
    };
    let reference = computation_ref(&computation)?;
    objects.insert(
        reference.content_ref(),
        &encode_computation_object(&computation)?,
    )?;
    Ok(reference)
}

/// Commits an Adapter-declared semantic transition without making its Record,
/// sequence, timestamp, or other evidence part of computation identity.
pub(crate) fn evolve_observation(
    objects: &dyn ObjectStore,
    start: &ComputationRef,
    observation: &AdapterObservation,
    payload_ref: &ContentRef,
) -> Result<ComputationRef> {
    let resolved = resolve_computation(objects, start)?;
    if resolved.object().semantics == SemanticsId::parse(AUTHORING_SEMANTICS_ID)? {
        let mut state = load_state(start, objects)?;
        let transition = SemanticTransition {
            version: 1,
            prior_frontier: state.semantic_frontier.as_deref(),
            adapter_id: &observation.adapter_id,
            protocol_id: observation.protocol_id.to_string(),
            port_id: observation.port_id.to_string(),
            direction: observation.direction,
            payload_ref: payload_ref.to_string(),
        };
        state.semantic_frontier = Some(format!(
            "blake3:{}",
            blake3::hash(&serde_jcs::to_vec(&transition)?).to_hex()
        ));
        return seal_authoring_state(objects, state);
    }
    if resolved.object().semantics != SemanticsId::parse(COMPOSE_SEMANTICS_ID)? {
        bail!("computation {start} cannot commit an Adapter observation");
    }

    let mut composite = load_composite(start, objects)?;
    let mut matching_nodes = Vec::new();
    for (node, child) in &composite.nodes {
        if computation_has_port(child, &observation.port_id, objects)? {
            matching_nodes.push(node.clone());
        }
    }
    let node = match matching_nodes.as_slice() {
        [node] => node,
        [] if composite.nodes.len() == 1 => composite.nodes.keys().next().expect("one node"),
        [] => bail!(
            "no composite child owns semantic observation port `{}`",
            observation.port_id
        ),
        _ => bail!(
            "multiple composite children own semantic observation port `{}`",
            observation.port_id
        ),
    }
    .clone();
    let child = composite.nodes.get(&node).expect("selected child").clone();
    composite.nodes.insert(
        node,
        evolve_observation(objects, &child, observation, payload_ref)?,
    );
    seal_composite(
        objects,
        composite.nodes,
        composite.connections,
        composite.exports,
        resolved.object().boundary.clone(),
    )
}

fn computation_has_port(
    reference: &ComputationRef,
    port_id: &PortId,
    objects: &dyn ObjectResolver,
) -> Result<bool> {
    let computation = resolve_computation(objects, reference)?;
    if computation.object().semantics == SemanticsId::parse(AUTHORING_SEMANTICS_ID)? {
        return Ok(load_state(reference, objects)?
            .config
            .port
            .iter()
            .any(|port| port.id == port_id.as_str()));
    }
    if computation.object().semantics == SemanticsId::parse(COMPOSE_SEMANTICS_ID)? {
        for child in load_composite(reference, objects)?.nodes.values() {
            if computation_has_port(child, port_id, objects)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn seal_composite(
    objects: &dyn ObjectStore,
    nodes: std::collections::BTreeMap<NodeId, ComputationRef>,
    connections: Vec<Connection>,
    exports: std::collections::BTreeMap<PortId, Endpoint>,
    boundary: Boundary,
) -> Result<ComputationRef> {
    let residual = objects.put(&encode_composite_residual(&CompositeResidual {
        nodes,
        connections,
        exports,
    })?)?;
    let computation = ComputationObject {
        semantics: SemanticsId::parse(COMPOSE_SEMANTICS_ID)?,
        boundary,
        residual,
    };
    let reference = computation_ref(&computation)?;
    objects.insert(
        reference.content_ref(),
        &encode_computation_object(&computation)?,
    )?;
    Ok(reference)
}

fn boundary(config: &AuthoringConfig) -> Result<Boundary> {
    config
        .port
        .iter()
        .filter(|port| !port.internal)
        .map(|port| {
            Ok((
                PortId::parse(&port.id)?,
                PortDef {
                    protocol: ProtocolId::parse(&port.protocol)?,
                    role: RoleId::parse(&port.role)?,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_adapter_config_lowers_port_without_an_adapter_id_branch() {
        let project = tempfile::tempdir().expect("temporary project should open");
        std::fs::write(
            project.path().join("capsule.toml"),
            r#"schema = 1

[[process]]
id = "app"
command = ["app"]

[[port]]
id = "app.browser"
node = "app"
protocol = "ato.browser@1"
role = "server"

[[adapter]]
use = "ato.browser@1"
port = "app.browser"

[adapter.config]
expected_origin = "http://127.0.0.1:3000"
"#,
        )
        .expect("authoring config should write");
        let config = load_config(project.path()).expect("authoring config should load");
        let instances = adapter_instances(&config, &BTreeMap::new(), false, false)
            .expect("generic Adapter should lower");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].adapter_id, "ato.browser@1");
        assert_eq!(instances[0].config["port_id"], "app.browser");
        assert_eq!(
            instances[0].config["expected_origin"],
            "http://127.0.0.1:3000"
        );
    }
}

pub(crate) fn load_snapshot(
    reference: &str,
    objects: &dyn ObjectResolver,
) -> Result<WorkspaceSnapshot> {
    let reference = ContentRef::parse(reference)?;
    let metadata = objects.metadata(&reference)?;
    let bytes = read_exact_object(
        objects,
        &reference,
        metadata.size,
        MAX_WORKSPACE_SNAPSHOT_BYTES,
    )?;
    let snapshot: WorkspaceSnapshot = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&snapshot)? != bytes {
        bail!("workspace snapshot is not canonical JCS");
    }
    Ok(snapshot)
}

fn observed_at() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_state(adapter_config: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "config": {
                "adapter": [{
                    "config": adapter_config,
                    "input": null,
                    "listen": null,
                    "port": null,
                    "ready_path": null,
                    "target": "app",
                    "upstream": null,
                    "use": "ato.process@1"
                }],
                "binding": [],
                "connection": [],
                "encap": { "materializers": ["ato.replay@1"] },
                "port": [],
                "process": [{ "command": ["true"], "cwd": ".", "id": "app" }],
                "schema": 1,
                "workspace": { "exclude": [], "include": [] }
            },
            "version": 1,
            "workspace_snapshot": format!("blake3:{}", "a".repeat(64))
        })
    }

    #[test]
    fn legacy_null_adapter_config_is_a_projection_only_compatibility_field() {
        let state = decode_legacy_authoring_state(legacy_state(serde_json::Value::Null)).unwrap();
        assert_eq!(state.config.adapter.len(), 1);
        assert_eq!(state.config.adapter[0].use_adapter, "ato.process@1");
    }

    #[test]
    fn legacy_non_null_adapter_config_is_not_silently_discarded() {
        let error = decode_legacy_authoring_state(legacy_state(serde_json::json!({"secret": 1})))
            .unwrap_err();
        assert!(error.to_string().contains("must be null"));
    }
}
