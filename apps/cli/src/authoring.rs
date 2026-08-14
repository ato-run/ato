use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_adapter_workspace::{
    WorkspaceMutation, WorkspaceSnapshot, capture_workspace, encode_mutation,
};
use ato_computation::{
    Boundary, ComputationObject, ComputationRef, ContentRef, PortDef, PortId, ProtocolId,
    ResolvedComputation, RoleId, SemanticsId, computation_ref, encode_computation_object,
};
use ato_objects::{
    BundleError, ComputationReferences, Direction, LocalCapsuleRepository, ObjectLink,
    ObjectResolver, ObjectStore, RecordEnvelope, read_exact_object, resolve_computation,
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
    pub binding: Vec<BindingConfig>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortConfig {
    pub id: String,
    pub protocol: String,
    pub role: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthoringState {
    version: u32,
    pub config: AuthoringConfig,
    pub workspace_snapshot: String,
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
    Ok(config)
}

pub(crate) fn initial_computation(
    repository: &LocalCapsuleRepository,
    config: AuthoringConfig,
) -> Result<ComputationRef> {
    let snapshot = capture_workspace(repository.project(), repository.objects())?;
    seal_state(repository.objects(), config, snapshot)
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
    let state: AuthoringState = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&state)? != bytes || state.version != AUTHORING_STATE_VERSION {
        bail!("authoring state is non-canonical or unsupported");
    }
    Ok(state)
}

pub(crate) fn evolve_workspace(
    repository: &LocalCapsuleRepository,
    branch: &str,
    start: &ComputationRef,
) -> Result<ComputationRef> {
    let mut state = load_state(start, repository.objects())?;
    let before = load_snapshot(&state.workspace_snapshot, repository.objects())?;
    let final_ref = capture_workspace(repository.project(), repository.objects())?;
    let after = load_snapshot(final_ref.as_str(), repository.objects())?;
    if before == after {
        return Ok(start.clone());
    }

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
        let next = seal_state(repository.objects(), state.config.clone(), snapshot)?;
        let payload = repository.objects().put(&encode_mutation(&mutation)?)?;
        let record = repository.append_record(RecordEnvelope {
            seq: 0,
            stream: branch.to_owned(),
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
        previous_record = Some(record.seq);
        head = next;
    }
    Ok(head)
}

pub(crate) fn seal_state(
    objects: &dyn ObjectStore,
    config: AuthoringConfig,
    workspace_snapshot: ContentRef,
) -> Result<ComputationRef> {
    let boundary = boundary(&config)?;
    let state = AuthoringState {
        version: AUTHORING_STATE_VERSION,
        config,
        workspace_snapshot: workspace_snapshot.to_string(),
    };
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

fn boundary(config: &AuthoringConfig) -> Result<Boundary> {
    config
        .port
        .iter()
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

#[derive(Default)]
pub(crate) struct AuthoringReferences {
    id: Option<SemanticsId>,
}

impl AuthoringReferences {
    pub(crate) fn new() -> Self {
        Self {
            id: Some(SemanticsId::parse(AUTHORING_SEMANTICS_ID).expect("valid static id")),
        }
    }
}

impl ComputationReferences for AuthoringReferences {
    fn semantics(&self) -> &SemanticsId {
        self.id.as_ref().expect("initialized authoring references")
    }

    fn outgoing(
        &self,
        computation: &ResolvedComputation,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let state = load_state(computation.reference(), objects).map_err(|error| {
            BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
        })?;
        let snapshot_ref = ContentRef::parse(&state.workspace_snapshot).map_err(|error| {
            BundleError::InvalidReference {
                value: state.workspace_snapshot.clone(),
                reason: error.to_string(),
            }
        })?;
        let snapshot = load_snapshot(&state.workspace_snapshot, objects).map_err(|error| {
            BundleError::Object(ato_objects::ObjectError::Storage(error.to_string()))
        })?;
        let mut links = vec![ObjectLink::Content(snapshot_ref)];
        for content in snapshot.files.into_values() {
            links.push(ObjectLink::Content(ContentRef::parse(&content).map_err(
                |error| BundleError::InvalidReference {
                    value: content,
                    reason: error.to_string(),
                },
            )?));
        }
        Ok(links)
    }
}
