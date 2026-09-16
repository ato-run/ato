//! One Rust authority for the `ato.portable-application/1` bundle profile.
//!
//! The CLI and hosted validator both call this crate. TypeScript transports
//! bytes and renders reports; it does not reinterpret Contract semantics.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ato_computation::ContentRef;
use ato_formation::authoring::{
    AuthoringDraft, AuthoringProvenance, BOUND_CONTRACT_SCHEMA, BOUND_DERIVATION_SCHEMA,
    BROWSER_PROTOCOL, BindingContext, BoundContract, BoundDerivation, BoundInput, BoundPort,
    BoundRequirement, BoundStep, DerivationDraft, EffectClass, HTTP_CONTRACT_VERIFIER,
    HTTP_PROTOCOL, INSTANCE_SNAPSHOT_CONTRACT_VERIFIER, PROCESS_PROTOCOL, PortDraft, StepDraft,
    WORKSPACE_CONTRACT_VERIFIER, WORKSPACE_PROTOCOL, bind,
};
use ato_formation::capsule_toml::{CAPSULE_FILE_NAME, parse_capsule_toml};
use ato_materializer_static_web::{media_type_for, validate_relative_path};
use ato_objects::{
    PORTABLE_APPLICATION_BUNDLE_VERSION, PORTABLE_APPLICATION_PROFILE, PortableApplicationBundle,
    PortableBundleError, PortableBundleIndex, PortableBundleObjectDescriptor,
    PortableBundleObjectKind, PortableBundlePayload, PortableReferenceExtractor,
    PortableReferenceRegistry, decode_portable_application_bundle,
    encode_portable_application_bundle, validate_portable_application_closure,
};
use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub mod instance_snapshot;
pub mod local_instance;
pub mod oci_archive;
pub mod portability_export;
pub mod portability_plan;
pub mod validator_agent;

use instance_snapshot::{INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotV1, validate_snapshot};

pub const APPLICATION_SCHEMA: &str = "ato.application/1";
pub const APPLICATION_V2_SCHEMA: &str = "ato.application/2";
pub const PORTABLE_TREE_SCHEMA: &str = "ato.portable-tree/1";
pub const PORTABLE_TREE_V2_SCHEMA: &str = "ato.portable-tree/2";
pub const OCI_PROTOCOL: &str = "ato.oci@1";

pub const PYTHON_RUNTIME: &str = "python";
pub const OCI_IMAGE_RUNTIME: &str = "oci.image";
pub const OCI_PLATFORM_RUNTIME: &str = "oci.platform";
pub const OCI_MEMORY_BYTES_RUNTIME: &str = "oci.memory_bytes";
pub const OCI_CPU_MILLIS_RUNTIME: &str = "oci.cpu_limit_millis";
pub const OCI_PIDS_LIMIT_RUNTIME: &str = "oci.pids_limit";

#[derive(Debug, Error)]
pub enum PortableApplicationError {
    #[error(transparent)]
    Bundle(#[from] PortableBundleError),
    #[error("portable application I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("portable application JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("capsule.toml is invalid: {0}")]
    CapsuleToml(String),
    #[error("contract binding failed: {0}")]
    Binding(String),
    #[error("portable application profile violation: {0}")]
    Profile(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationV1 {
    pub schema: String,
    pub title: String,
    pub surfaces: Vec<ApplicationSurfaceV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSurfaceV1 {
    pub id: String,
    pub port: String,
    pub artifact_ref: String,
    pub entry: String,
    pub spa_fallback: bool,
}

/// A logical application surface. Unlike v1 it does not claim that an HTTP
/// endpoint is a file in a static artifact: a process or container publishes
/// this Port only after the Runner has established it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationV2 {
    pub schema: String,
    pub title: String,
    pub surfaces: Vec<ApplicationSurfaceV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSurfaceV2 {
    pub id: String,
    pub port: String,
    #[serde(default = "default_surface_path")]
    pub path: String,
}

fn default_surface_path() -> String {
    "/".to_owned()
}

/// The normalized application view used after schema-specific format
/// validation. Optional static fields are absent for a dynamic Surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedApplication {
    pub schema: String,
    pub title: String,
    pub surfaces: Vec<ValidatedApplicationSurface>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedApplicationSurface {
    pub id: String,
    pub port: String,
    pub path: String,
    pub artifact_ref: Option<String>,
    pub entry: Option<String>,
    pub spa_fallback: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableTreeV1 {
    pub schema: String,
    pub entries: Vec<PortableTreeEntryV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableTreeEntryV1 {
    pub path: String,
    pub content_ref: String,
    pub size: u64,
    pub media_type: String,
}

#[derive(Debug, Clone)]
pub struct ValidatedPortableApplication {
    pub contract_ref: ContentRef,
    pub application_ref: ContentRef,
    pub derivation_ref: ContentRef,
    pub instance_snapshot_ref: Option<ContentRef>,
    pub contract: BoundContract,
    pub application: ValidatedApplication,
    pub derivation: BoundDerivation,
    pub tree_ref: ContentRef,
    pub tree: PortableTreeV1,
    pub tree_schema: String,
    pub realization: PortableRealizationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortableRealizationKind {
    StaticWeb,
    LocalProcess,
    OciContainer,
}

impl PortableRealizationKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::StaticWeb => "Static Web",
            Self::LocalProcess => "Local Process",
            Self::OciContainer => "OCI Container",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PortableHttpRequirementSpec {
    pub id: String,
    pub path: String,
    pub status: u16,
    pub body_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PortableExecutionSpec {
    pub runtimes: BTreeMap<String, String>,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct PortableDynamicBundleSpec {
    pub title: String,
    pub surface_path: String,
    pub guest_port: u16,
    pub process: PortableExecutionSpec,
    pub oci: PortableExecutionSpec,
    pub requirements: Vec<PortableHttpRequirementSpec>,
}

/// Loopback-only static realization used by `ato run`.
pub struct StaticApplicationServer {
    address: SocketAddr,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl StaticApplicationServer {
    pub fn start(
        root: &Path,
        validated: &ValidatedPortableApplication,
    ) -> Result<Self, PortableApplicationError> {
        if validated.realization != PortableRealizationKind::StaticWeb {
            return Err(profile("static server requires a static-web derivation"));
        }
        verify_materialized_tree(validated, root)?;
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|source| PortableApplicationError::Io {
                path: root.to_path_buf(),
                source,
            })?;
        let address = listener
            .local_addr()
            .map_err(|source| PortableApplicationError::Io {
                path: root.to_path_buf(),
                source,
            })?;
        let routes = validated
            .tree
            .entries
            .iter()
            .map(|entry| {
                (
                    format!("/{}", entry.path),
                    (root.join(&entry.path), entry.media_type.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let surface = &validated.application.surfaces[0];
        let entry = surface
            .entry
            .as_deref()
            .ok_or_else(|| profile("static surface omitted its entry"))?;
        let entry_route = format!("/{entry}");
        let spa_fallback = surface.spa_fallback.unwrap_or(false);
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let _ = ready_tx.send(());
            while thread_running.load(Ordering::Acquire) {
                let Ok((stream, _)) = listener.accept() else {
                    continue;
                };
                if !thread_running.load(Ordering::Acquire) {
                    break;
                }
                let _ = serve_request(stream, &routes, &entry_route, spa_fallback);
            }
        });
        ready_rx
            .recv()
            .map_err(|error| profile(format!("static server did not start: {error}")))?;
        Ok(Self {
            address,
            running,
            thread: Some(thread),
        })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }
}

impl Drop for StaticApplicationServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Ok(mut stream) = TcpStream::connect_timeout(&self.address, Duration::from_secs(1)) {
            let _ = stream.write_all(b"GET /__ato_shutdown__ HTTP/1.1\r\n\r\n");
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn bundle_sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub fn validate_bytes(
    bytes: &[u8],
) -> Result<(PortableApplicationBundle, ValidatedPortableApplication), PortableApplicationError> {
    let bundle = decode_portable_application_bundle(bytes)?;
    let validated = validate_bundle(&bundle)?;
    Ok((bundle, validated))
}

pub fn validate_bytes_all(
    bytes: &[u8],
) -> Result<(PortableApplicationBundle, Vec<ValidatedPortableApplication>), PortableApplicationError>
{
    let bundle = decode_portable_application_bundle(bytes)?;
    let validated = validate_all_derivations(&bundle)?;
    Ok((bundle, validated))
}

pub fn validate_bytes_for_derivation(
    bytes: &[u8],
    derivation_ref: &str,
) -> Result<(PortableApplicationBundle, ValidatedPortableApplication), PortableApplicationError> {
    let bundle = decode_portable_application_bundle(bytes)?;
    let validated = validate_bundle_for_derivation(&bundle, derivation_ref)?;
    Ok((bundle, validated))
}

pub fn validate_bundle(
    bundle: &PortableApplicationBundle,
) -> Result<ValidatedPortableApplication, PortableApplicationError> {
    if bundle.index.derivations.len() != 1 {
        return Err(profile(
            "bundle has multiple derivations; select one explicitly",
        ));
    }
    validate_bundle_for_derivation(bundle, &bundle.index.derivations[0])
}

pub fn validate_all_derivations(
    bundle: &PortableApplicationBundle,
) -> Result<Vec<ValidatedPortableApplication>, PortableApplicationError> {
    validate_bundle_closure(bundle)?;
    bundle
        .index
        .derivations
        .iter()
        .map(|reference| validate_selected_derivation(bundle, reference))
        .collect()
}

pub fn validate_bundle_for_derivation(
    bundle: &PortableApplicationBundle,
    derivation_ref: &str,
) -> Result<ValidatedPortableApplication, PortableApplicationError> {
    validate_bundle_closure(bundle)?;
    if !bundle
        .index
        .derivations
        .iter()
        .any(|candidate| candidate == derivation_ref)
    {
        return Err(profile(format!(
            "selected derivation `{derivation_ref}` is not declared by this bundle"
        )));
    }
    validate_selected_derivation(bundle, derivation_ref)
}

fn validate_bundle_closure(
    bundle: &PortableApplicationBundle,
) -> Result<(), PortableApplicationError> {
    let registry = portable_reference_registry()?;
    validate_portable_application_closure(bundle, &registry)?;
    validate_instance_snapshot_binding(bundle)?;
    if let Some(portability) = bundle.portability.as_ref() {
        let mut oci_dependencies = BTreeMap::new();
        for value in &bundle.index.derivations {
            let reference = parse_ref(value, "derivation")?;
            let derivation: BoundDerivation =
                structured(bundle, &reference, BOUND_DERIVATION_SCHEMA)?;
            if derivation
                .steps
                .iter()
                .any(|step| step.protocol == OCI_PROTOCOL)
            {
                let image = derivation
                    .runtimes
                    .get(OCI_IMAGE_RUNTIME)
                    .ok_or_else(|| profile("OCI derivation has no image"))?;
                let platform = derivation
                    .runtimes
                    .get(OCI_PLATFORM_RUNTIME)
                    .ok_or_else(|| profile("OCI derivation has no platform"))?;
                oci_dependencies.insert(image.clone(), platform.clone());
            }
        }
        let mut supplied = BTreeSet::new();
        for archive in &portability.oci_archives {
            if oci_dependencies.get(&archive.image) != Some(&archive.platform)
                || !supplied.insert(&archive.image)
            {
                return Err(profile(
                    "OCI archive is duplicate or not declared by a Derivation",
                ));
            }
            oci_archive::verify_oci_archive(archive)
                .map_err(|error| profile(format!("OCI archive verification failed: {error:#}")))?;
        }
        if portability.profile == ato_objects::PortableDependencyProfile::Offline
            && supplied.len() != oci_dependencies.len()
        {
            return Err(profile(
                "offline OCI bundle lacks a verified embedded image manifest/layer graph",
            ));
        }
    }
    Ok(())
}

fn validate_instance_snapshot_binding(
    bundle: &PortableApplicationBundle,
) -> Result<(), PortableApplicationError> {
    let contract_ref = parse_ref(&bundle.index.root_contract_ref, "root_contract_ref")?;
    let contract: BoundContract = structured(bundle, &contract_ref, BOUND_CONTRACT_SCHEMA)?;
    let requirements = contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == INSTANCE_SNAPSHOT_CONTRACT_VERIFIER)
        .collect::<Vec<_>>();
    let Some(reference) = bundle.index.instance_snapshot_ref.as_deref() else {
        if requirements.is_empty() {
            return Ok(());
        }
        return Err(profile(
            "Contract observes an Instance snapshot but the v4 index names none",
        ));
    };
    if bundle.index.version != ato_objects::PORTABLE_APPLICATION_BUNDLE_VERSION_V4 {
        return Err(profile("Instance snapshots require portable bundle v4"));
    }
    let [requirement] = requirements.as_slice() else {
        return Err(profile(
            "snapshot-bearing Contract must contain exactly one Instance snapshot requirement",
        ));
    };
    if requirement.digest.as_deref() != Some(reference)
        || requirement.id.is_empty()
        || requirement.port.is_some()
        || requirement.method.is_some()
        || requirement.path.is_some()
        || requirement.status.is_some()
        || requirement.body_digest.is_some()
        || requirement.input.is_some()
    {
        return Err(profile(
            "Instance snapshot Contract requirement does not bind the indexed snapshot",
        ));
    }
    let snapshot_ref = parse_ref(reference, "instance_snapshot_ref")?;
    let snapshot: InstanceSnapshotV1 = structured(bundle, &snapshot_ref, INSTANCE_SNAPSHOT_SCHEMA)?;
    if snapshot.resources.is_empty() && snapshot.assets.is_empty() {
        return Err(profile("Instance snapshot is empty"));
    }
    validate_snapshot(&snapshot)?;
    for resource in &snapshot.resources {
        let reference = parse_ref(&resource.content_ref, "snapshot resource")?;
        let descriptor = bundle
            .descriptor(&reference)
            .ok_or_else(|| profile("snapshot resource is absent from the bundle"))?;
        if descriptor.kind != PortableBundleObjectKind::Blob {
            return Err(profile("snapshot resource must be an opaque blob"));
        }
        bundle.payload_bytes(&reference)?;
    }
    for asset in &snapshot.assets {
        let reference = parse_ref(&asset.content_ref, "snapshot Asset")?;
        let descriptor = bundle
            .descriptor(&reference)
            .ok_or_else(|| profile("snapshot Asset is absent from the bundle"))?;
        if descriptor.kind != PortableBundleObjectKind::Blob || descriptor.size != asset.size {
            return Err(profile("snapshot Asset size or object kind is invalid"));
        }
        bundle.payload_bytes(&reference)?;
    }
    Ok(())
}

fn validate_selected_derivation(
    bundle: &PortableApplicationBundle,
    selected_derivation_ref: &str,
) -> Result<ValidatedPortableApplication, PortableApplicationError> {
    let contract_ref = parse_ref(&bundle.index.root_contract_ref, "root_contract_ref")?;
    let application_ref = parse_ref(&bundle.index.application_ref, "application_ref")?;
    let derivation_ref = parse_ref(selected_derivation_ref, "derivation")?;
    let instance_snapshot_ref = bundle
        .index
        .instance_snapshot_ref
        .as_deref()
        .map(|reference| parse_ref(reference, "instance_snapshot_ref"))
        .transpose()?;
    let contract: BoundContract = structured(bundle, &contract_ref, BOUND_CONTRACT_SCHEMA)?;
    let application = validated_application(bundle, &application_ref)?;
    let derivation: BoundDerivation = structured(bundle, &derivation_ref, BOUND_DERIVATION_SCHEMA)?;

    if contract
        .contract_ref()
        .map_err(|error| profile(error.to_string()))?
        != contract_ref.as_str()
    {
        return Err(profile(
            "root_contract_ref does not name the canonical Contract",
        ));
    }
    if derivation
        .derivation_ref()
        .map_err(|error| profile(error.to_string()))?
        != derivation_ref.as_str()
    {
        return Err(profile("derivation reference is not its canonical digest"));
    }
    let realization = validate_initial_route(&contract, &application, &derivation)?;

    let surface = &application.surfaces[0];
    let tree_reference = surface
        .artifact_ref
        .as_deref()
        .or_else(|| {
            derivation
                .inputs
                .first()
                .map(|input| input.content_ref.as_str())
        })
        .ok_or_else(|| profile("portable derivation omitted its workspace input"))?;
    let tree_ref = parse_ref(tree_reference, "workspace tree reference")?;
    let tree_schema = bundle
        .descriptor(&tree_ref)
        .and_then(|descriptor| descriptor.schema.clone())
        .ok_or_else(|| profile("workspace input must name a structured portable tree"))?;
    if tree_schema != PORTABLE_TREE_SCHEMA && tree_schema != PORTABLE_TREE_V2_SCHEMA {
        return Err(profile(format!(
            "workspace input uses unsupported tree schema `{tree_schema}`"
        )));
    }
    let tree: PortableTreeV1 = structured(bundle, &tree_ref, &tree_schema)?;
    validate_tree(bundle, &tree, &tree_schema)?;
    if realization == PortableRealizationKind::StaticWeb {
        validate_static_http_paths(&contract, &application, &tree)?;
    }

    Ok(ValidatedPortableApplication {
        contract_ref,
        application_ref,
        derivation_ref,
        instance_snapshot_ref,
        contract,
        application,
        derivation,
        tree_ref,
        tree,
        tree_schema,
        realization,
    })
}

fn validated_application(
    bundle: &PortableApplicationBundle,
    application_ref: &ContentRef,
) -> Result<ValidatedApplication, PortableApplicationError> {
    let schema = bundle
        .descriptor(application_ref)
        .and_then(|descriptor| descriptor.schema.as_deref())
        .ok_or_else(|| profile("application_ref must name a structured Application"))?;
    match schema {
        APPLICATION_SCHEMA => {
            let application: ApplicationV1 = structured(bundle, application_ref, schema)?;
            Ok(ValidatedApplication {
                schema: application.schema,
                title: application.title,
                surfaces: application
                    .surfaces
                    .into_iter()
                    .map(|surface| ValidatedApplicationSurface {
                        id: surface.id,
                        port: surface.port,
                        path: "/".to_owned(),
                        artifact_ref: Some(surface.artifact_ref),
                        entry: Some(surface.entry),
                        spa_fallback: Some(surface.spa_fallback),
                    })
                    .collect(),
            })
        }
        APPLICATION_V2_SCHEMA => {
            let application: ApplicationV2 = structured(bundle, application_ref, schema)?;
            Ok(ValidatedApplication {
                schema: application.schema,
                title: application.title,
                surfaces: application
                    .surfaces
                    .into_iter()
                    .map(|surface| ValidatedApplicationSurface {
                        id: surface.id,
                        port: surface.port,
                        path: surface.path,
                        artifact_ref: None,
                        entry: None,
                        spa_fallback: None,
                    })
                    .collect(),
            })
        }
        other => Err(profile(format!(
            "application_ref uses unsupported schema `{other}`"
        ))),
    }
}

/// Deterministically forms a portable static application from the existing
/// `ato.capsule/1` authoring grammar. `capsule.toml` is authoring input, not a
/// runtime file, and is therefore not embedded in the artifact tree.
pub fn build_static_bundle(
    source_root: &Path,
    title: &str,
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    if title.trim().is_empty() {
        return Err(profile("application title must not be empty"));
    }
    let draft = read_authoring_draft(source_root)?;
    let (mut objects, tree_ref) = build_portable_tree(source_root)?;
    let (contract, derivation) = bind(
        &draft,
        &BindingContext {
            source_closure_ref: &tree_ref,
        },
    )
    .map_err(|error| PortableApplicationError::Binding(error.to_string()))?;

    if derivation.steps.len() != 1 || derivation.ports.len() != 1 {
        return Err(profile(
            "the initial profile requires one browser step and one port",
        ));
    }
    let step = &derivation.steps[0];
    let port = &derivation.ports[0];
    let application = ApplicationV1 {
        schema: APPLICATION_SCHEMA.to_owned(),
        title: title.to_owned(),
        surfaces: vec![ApplicationSurfaceV1 {
            id: "main".to_owned(),
            port: port.id.clone(),
            artifact_ref: tree_ref.clone(),
            entry: step.entry.clone().unwrap_or_default(),
            spa_fallback: step.spa_fallback.unwrap_or(false),
        }],
    };
    let contract_ref = add_structured(&mut objects, &contract, BOUND_CONTRACT_SCHEMA)?;
    let derivation_ref = add_structured(&mut objects, &derivation, BOUND_DERIVATION_SCHEMA)?;
    let application_ref = add_structured(&mut objects, &application, APPLICATION_SCHEMA)?;

    let bundle = finish_bundle(objects, contract_ref, application_ref, vec![derivation_ref]);
    validate_bundle(&bundle)?;
    let bytes = encode_portable_application_bundle(&bundle)?;
    Ok((bytes, bundle))
}

/// Build one canonical bundle containing two explicitly selectable routes for
/// the same Contract: browser/static delivery and a pinned Python process.
pub fn build_multi_derivation_bundle(
    source_root: &Path,
    title: &str,
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    if title.trim().is_empty() {
        return Err(profile("application title must not be empty"));
    }
    let process_draft = read_authoring_draft(source_root)?;
    let (mut objects, tree_ref) = build_portable_tree(source_root)?;
    let (contract, process_derivation) = bind(
        &process_draft,
        &BindingContext {
            source_closure_ref: &tree_ref,
        },
    )
    .map_err(|error| PortableApplicationError::Binding(error.to_string()))?;

    let input = process_draft
        .derivation
        .inputs
        .first()
        .cloned()
        .ok_or_else(|| profile("multi-derivation fixture requires one workspace input"))?;
    let process_port = process_draft
        .derivation
        .ports
        .first()
        .cloned()
        .ok_or_else(|| profile("multi-derivation fixture requires one HTTP port"))?;
    let static_step_id = "static-site".to_owned();
    let static_draft = AuthoringDraft {
        contract: process_draft.contract.clone(),
        derivation: DerivationDraft {
            inputs: vec![input.clone()],
            runtimes: Vec::new(),
            steps: vec![StepDraft {
                id: static_step_id.clone(),
                protocol: BROWSER_PROTOCOL.to_owned(),
                op: "serve".to_owned(),
                argv: Vec::new(),
                cwd: String::new(),
                env: BTreeMap::new(),
                source: Some(input.id.clone()),
                root: None,
                entry: Some("index.html".to_owned()),
                spa_fallback: Some(false),
            }],
            ports: vec![PortDraft {
                id: process_port.id.clone(),
                protocol: process_port.protocol.clone(),
                from: static_step_id,
                guest_port: None,
            }],
            state: Vec::new(),
            workspace_build: None,
            workspace_compiler: None,
            effects: EffectClass::Pure,
        },
        provenance: AuthoringProvenance::Authored,
    };
    let (static_contract, static_derivation) = bind(
        &static_draft,
        &BindingContext {
            source_closure_ref: &tree_ref,
        },
    )
    .map_err(|error| PortableApplicationError::Binding(error.to_string()))?;
    if contract != static_contract {
        return Err(profile(
            "static and process derivations did not bind to the same Contract",
        ));
    }

    let application = ApplicationV1 {
        schema: APPLICATION_SCHEMA.to_owned(),
        title: title.to_owned(),
        surfaces: vec![ApplicationSurfaceV1 {
            id: "main".to_owned(),
            port: process_port.id,
            artifact_ref: tree_ref,
            entry: "index.html".to_owned(),
            spa_fallback: false,
        }],
    };
    let contract_ref = add_structured(&mut objects, &contract, BOUND_CONTRACT_SCHEMA)?;
    let static_ref = add_structured(&mut objects, &static_derivation, BOUND_DERIVATION_SCHEMA)?;
    let process_ref = add_structured(&mut objects, &process_derivation, BOUND_DERIVATION_SCHEMA)?;
    if static_ref == process_ref {
        return Err(profile("the two derivations have the same identity"));
    }
    let application_ref = add_structured(&mut objects, &application, APPLICATION_SCHEMA)?;
    let bundle = finish_bundle(
        objects,
        contract_ref,
        application_ref,
        vec![static_ref, process_ref],
    );
    let validated = validate_all_derivations(&bundle)?;
    if validated
        .iter()
        .map(|route| route.realization)
        .collect::<BTreeSet<_>>()
        != [
            PortableRealizationKind::StaticWeb,
            PortableRealizationKind::LocalProcess,
        ]
        .into_iter()
        .collect()
    {
        return Err(profile(
            "multi-derivation bundle must contain one static and one process route",
        ));
    }
    let bytes = encode_portable_application_bundle(&bundle)?;
    Ok((bytes, bundle))
}

/// Build one application with two explicit execution routes over one logical
/// HTTP Contract. The builder knows no OSS name: Datasette is a sample of this
/// generic process/OCI profile, not a Core special case.
pub fn build_dynamic_process_oci_bundle(
    source_root: &Path,
    spec: &PortableDynamicBundleSpec,
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    if spec.title.trim().is_empty() || spec.requirements.is_empty() {
        return Err(profile(
            "dynamic application requires a title and at least one HTTP observation",
        ));
    }
    let (mut objects, tree_ref) =
        build_portable_tree_with_schema(source_root, PORTABLE_TREE_V2_SCHEMA)?;
    let input = BoundInput {
        id: "workspace".to_owned(),
        protocol: WORKSPACE_PROTOCOL.to_owned(),
        content_ref: tree_ref,
    };
    let port = BoundPort {
        id: "app.http".to_owned(),
        protocol: HTTP_PROTOCOL.to_owned(),
        from: "serve".to_owned(),
        guest_port: Some(spec.guest_port),
    };
    let derivation = |protocol: &str, execution: &PortableExecutionSpec| BoundDerivation {
        schema: BOUND_DERIVATION_SCHEMA.to_owned(),
        inputs: vec![input.clone()],
        runtimes: execution.runtimes.clone(),
        steps: vec![BoundStep {
            id: "serve".to_owned(),
            protocol: protocol.to_owned(),
            op: "serve".to_owned(),
            argv: execution.argv.clone(),
            cwd: execution.cwd.clone(),
            env: execution.env.clone(),
            source: None,
            root: None,
            entry: None,
            spa_fallback: None,
        }],
        ports: vec![port.clone()],
        state: Vec::new(),
        workspace_build: None,
        workspace_compiler: None,
        effects: EffectClass::Pure,
    };
    let process_derivation = derivation(PROCESS_PROTOCOL, &spec.process);
    let oci_derivation = derivation(OCI_PROTOCOL, &spec.oci);
    let mut requirements = spec
        .requirements
        .iter()
        .map(|requirement| BoundRequirement {
            id: requirement.id.clone(),
            verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
            port: Some(port.id.clone()),
            method: Some("GET".to_owned()),
            path: Some(requirement.path.clone()),
            status: Some(requirement.status),
            body_digest: requirement.body_digest.clone(),
            input: None,
            digest: None,
        })
        .collect::<Vec<_>>();
    requirements.sort_by(|left, right| left.id.cmp(&right.id));
    if requirements.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(profile(
            "dynamic Contract contains duplicate observation ids",
        ));
    }
    let contract = BoundContract {
        schema: BOUND_CONTRACT_SCHEMA.to_owned(),
        requirements,
    };
    let application = ApplicationV2 {
        schema: APPLICATION_V2_SCHEMA.to_owned(),
        title: spec.title.clone(),
        surfaces: vec![ApplicationSurfaceV2 {
            id: "main".to_owned(),
            port: port.id,
            path: spec.surface_path.clone(),
        }],
    };

    let contract_ref = add_structured(&mut objects, &contract, BOUND_CONTRACT_SCHEMA)?;
    let process_ref = add_structured(&mut objects, &process_derivation, BOUND_DERIVATION_SCHEMA)?;
    let oci_ref = add_structured(&mut objects, &oci_derivation, BOUND_DERIVATION_SCHEMA)?;
    if process_ref == oci_ref {
        return Err(profile("process and OCI routes have the same identity"));
    }
    let application_ref = add_structured(&mut objects, &application, APPLICATION_V2_SCHEMA)?;
    let bundle = finish_bundle(
        objects,
        contract_ref,
        application_ref,
        vec![process_ref, oci_ref],
    );
    let validated = validate_all_derivations(&bundle)?;
    let kinds = validated
        .iter()
        .map(|route| route.realization)
        .collect::<BTreeSet<_>>();
    if kinds
        != [
            PortableRealizationKind::LocalProcess,
            PortableRealizationKind::OciContainer,
        ]
        .into_iter()
        .collect()
    {
        return Err(profile(
            "dynamic bundle must contain one process and one OCI route",
        ));
    }
    let bytes = encode_portable_application_bundle(&bundle)?;
    Ok((bytes, bundle))
}

/// Materialize the validated tree into a new directory and re-read every byte
/// to prove the local workspace still has the declared identity.
pub fn materialize_tree(
    bundle: &PortableApplicationBundle,
    validated: &ValidatedPortableApplication,
    destination: &Path,
) -> Result<(), PortableApplicationError> {
    if destination.exists() {
        return Err(profile(format!(
            "materialization destination already exists: {}",
            destination.display()
        )));
    }
    fs::create_dir_all(destination).map_err(|source| PortableApplicationError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    for entry in &validated.tree.entries {
        let reference = parse_ref(&entry.content_ref, "tree entry content_ref")?;
        let bytes = bundle.payload_bytes(&reference)?;
        let output = destination.join(&entry.path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|source| PortableApplicationError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&output, &bytes).map_err(|source| PortableApplicationError::Io {
            path: output,
            source,
        })?;
    }
    verify_materialized_tree(validated, destination)
}

pub fn verify_materialized_tree(
    validated: &ValidatedPortableApplication,
    destination: &Path,
) -> Result<(), PortableApplicationError> {
    let actual_files = collect_materialized_files(destination)?;
    let declared_files = validated
        .tree
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    if actual_files != declared_files {
        return Err(profile(
            "materialized file set does not equal the portable tree",
        ));
    }
    for entry in &validated.tree.entries {
        let bytes = read(destination.join(&entry.path))?;
        if bytes.len() as u64 != entry.size || bundle_sha256(&bytes) != entry.content_ref {
            return Err(profile(format!(
                "materialized file `{}` does not match its declared identity",
                entry.path
            )));
        }
    }
    Ok(())
}

fn validate_initial_route(
    contract: &BoundContract,
    application: &ValidatedApplication,
    derivation: &BoundDerivation,
) -> Result<PortableRealizationKind, PortableApplicationError> {
    if !matches!(
        application.schema.as_str(),
        APPLICATION_SCHEMA | APPLICATION_V2_SCHEMA
    ) || application.title.trim().is_empty()
    {
        return Err(profile("invalid portable Application object"));
    }
    if application.surfaces.len() != 1
        || derivation.inputs.len() != 1
        || derivation.steps.len() != 1
        || derivation.ports.len() != 1
    {
        return Err(profile(
            "the initial profile requires one surface, input, serving step, and port",
        ));
    }
    if !derivation.state.is_empty()
        || derivation.workspace_build.is_some()
        || derivation.workspace_compiler.is_some()
        || derivation.effects != EffectClass::Pure
    {
        return Err(profile(
            "the initial profile forbids state, builds, and non-pure effects",
        ));
    }
    let input = &derivation.inputs[0];
    let step = &derivation.steps[0];
    let port = &derivation.ports[0];
    let surface = &application.surfaces[0];
    if input.protocol != WORKSPACE_PROTOCOL
        || port.protocol != HTTP_PROTOCOL
        || port.from != step.id
        || surface.port != port.id
        || surface
            .artifact_ref
            .as_ref()
            .is_some_and(|reference| reference != &input.content_ref)
    {
        return Err(profile(
            "application surface and derivation do not name one workspace-backed HTTP route",
        ));
    }
    let realization = match step.protocol.as_str() {
        BROWSER_PROTOCOL => {
            if application.schema != APPLICATION_SCHEMA {
                return Err(profile(
                    "static-web derivations require an ato.application/1 surface",
                ));
            }
            if !derivation.runtimes.is_empty()
                || step.op != "serve"
                || step.source.as_deref() != Some(input.id.as_str())
                || !step.argv.is_empty()
                || !step.cwd.is_empty()
                || !step.env.is_empty()
                || step.root.is_some()
                || port.guest_port.is_some()
                || surface.entry.as_deref() != step.entry.as_deref()
                || surface.spa_fallback != Some(step.spa_fallback.unwrap_or(false))
            {
                return Err(profile(
                    "static-web derivation does not match its application surface",
                ));
            }
            PortableRealizationKind::StaticWeb
        }
        PROCESS_PROTOCOL => {
            if application.schema == APPLICATION_V2_SCHEMA {
                validate_dynamic_surface(surface)?;
            }
            if derivation.runtimes.is_empty()
                || derivation
                    .runtimes
                    .values()
                    .any(|value| value.trim().is_empty())
                || step.op != "serve"
                || step.argv.is_empty()
                || step.argv[0].trim().is_empty()
                || !valid_workspace_relative(&step.cwd)
                || step.argv.iter().any(|value| value.contains('\0'))
                || step.env.iter().any(|(name, value)| {
                    name.is_empty() || name.contains('=') || value.contains('\0')
                })
                || step.source.is_some()
                || step.root.is_some()
                || step.entry.is_some()
                || step.spa_fallback.is_some()
                || port.guest_port.is_none()
            {
                return Err(profile(
                    "process derivation is outside the declared process HTTP profile",
                ));
            }
            PortableRealizationKind::LocalProcess
        }
        OCI_PROTOCOL => {
            if application.schema != APPLICATION_V2_SCHEMA {
                return Err(profile(
                    "OCI derivations require a logical ato.application/2 surface",
                ));
            }
            validate_dynamic_surface(surface)?;
            let image = derivation
                .runtimes
                .get(OCI_IMAGE_RUNTIME)
                .ok_or_else(|| profile("OCI derivation omitted oci.image"))?;
            let platform = derivation
                .runtimes
                .get(OCI_PLATFORM_RUNTIME)
                .ok_or_else(|| profile("OCI derivation omitted oci.platform"))?;
            validate_immutable_oci_image(image)?;
            validate_oci_platform(platform)?;
            for key in [
                OCI_MEMORY_BYTES_RUNTIME,
                OCI_CPU_MILLIS_RUNTIME,
                OCI_PIDS_LIMIT_RUNTIME,
            ] {
                let value = derivation
                    .runtimes
                    .get(key)
                    .ok_or_else(|| profile(format!("OCI derivation omitted {key}")))?;
                let parsed = value
                    .parse::<u64>()
                    .map_err(|_| profile(format!("OCI runtime value `{key}` is not an integer")))?;
                if parsed == 0 {
                    return Err(profile(format!(
                        "OCI runtime value `{key}` must be positive"
                    )));
                }
            }
            if derivation.runtimes.len() != 5
                || step.op != "serve"
                || step.argv.is_empty()
                || step.argv.iter().any(|value| value.contains('\0'))
                || !valid_workspace_relative(&step.cwd)
                || step.env.iter().any(|(name, value)| {
                    name.is_empty() || name.contains('=') || value.contains('\0')
                })
                || step.source.is_some()
                || step.root.is_some()
                || step.entry.is_some()
                || step.spa_fallback.is_some()
                || port.guest_port.is_none()
            {
                return Err(profile(
                    "OCI derivation is outside the declared container HTTP profile",
                ));
            }
            PortableRealizationKind::OciContainer
        }
        protocol => {
            return Err(profile(format!(
                "unsupported portable serving protocol `{protocol}`"
            )));
        }
    };

    let requirement_ids = contract
        .requirements
        .iter()
        .map(|requirement| requirement.id.as_str())
        .collect::<BTreeSet<_>>();
    if requirement_ids.len() != contract.requirements.len() {
        return Err(profile("contract contains duplicate requirement ids"));
    }
    for requirement in &contract.requirements {
        match requirement.verifier.as_str() {
            WORKSPACE_CONTRACT_VERIFIER => {
                if requirement.input.as_deref() != Some(input.id.as_str())
                    || requirement.digest.as_deref() != Some(input.content_ref.as_str())
                {
                    return Err(profile(
                        "workspace requirement does not bind the portable tree",
                    ));
                }
            }
            HTTP_CONTRACT_VERIFIER => {
                if requirement.port.as_deref() != Some(port.id.as_str())
                    || requirement.method.as_deref() != Some("GET")
                {
                    return Err(profile(
                        "HTTP requirement does not target the declared application surface",
                    ));
                }
                let path = requirement.path.as_deref().unwrap_or("/");
                if !path.starts_with('/')
                    || path.starts_with("//")
                    || path.contains('\0')
                    || path.contains("#")
                {
                    return Err(profile(
                        "HTTP requirement path is not an origin-relative target",
                    ));
                }
                if let Some(digest) = &requirement.body_digest {
                    parse_ref(digest, "HTTP body_digest")?;
                }
            }
            INSTANCE_SNAPSHOT_CONTRACT_VERIFIER => {
                let digest = requirement
                    .digest
                    .as_deref()
                    .ok_or_else(|| profile("Instance snapshot requirement omitted its digest"))?;
                parse_ref(digest, "Instance snapshot digest")?;
                if requirement.port.is_some()
                    || requirement.method.is_some()
                    || requirement.path.is_some()
                    || requirement.status.is_some()
                    || requirement.body_digest.is_some()
                    || requirement.input.is_some()
                {
                    return Err(profile(
                        "Instance snapshot requirement contains unrelated observation fields",
                    ));
                }
            }
            verifier => {
                return Err(profile(format!(
                    "unsupported required verifier `{verifier}`"
                )));
            }
        }
    }
    Ok(realization)
}

fn validate_dynamic_surface(
    surface: &ValidatedApplicationSurface,
) -> Result<(), PortableApplicationError> {
    if surface.artifact_ref.is_some()
        || surface.entry.is_some()
        || surface.spa_fallback.is_some()
        || !surface.path.starts_with('/')
        || surface.path.starts_with("//")
        || surface.path.contains(['\0', '?', '#'])
    {
        return Err(profile("dynamic application surface is invalid"));
    }
    Ok(())
}

fn valid_workspace_relative(value: &str) -> bool {
    if value.is_empty() || value == "." {
        return true;
    }
    if value.starts_with('/') || value.starts_with('\\') || value.contains('\0') {
        return false;
    }
    value
        .split(['/', '\\'])
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn validate_immutable_oci_image(value: &str) -> Result<(), PortableApplicationError> {
    let Some((repository, digest)) = value.rsplit_once("@sha256:") else {
        return Err(profile(
            "OCI image must be a repository reference pinned with @sha256:<digest>",
        ));
    };
    if repository.is_empty()
        || repository.contains(char::is_whitespace)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(profile("OCI image reference is not immutable"));
    }
    Ok(())
}

fn validate_oci_platform(value: &str) -> Result<(), PortableApplicationError> {
    let Some((os, architecture)) = value.split_once('/') else {
        return Err(profile("OCI platform must be declared as os/architecture"));
    };
    if os != "linux" || !matches!(architecture, "amd64" | "arm64") {
        return Err(profile(format!("unsupported OCI platform `{value}`")));
    }
    Ok(())
}

fn validate_static_http_paths(
    contract: &BoundContract,
    application: &ValidatedApplication,
    tree: &PortableTreeV1,
) -> Result<(), PortableApplicationError> {
    let surface = &application.surfaces[0];
    let entry = surface
        .entry
        .as_deref()
        .ok_or_else(|| profile("static surface omitted its entry"))?;
    for requirement in &contract.requirements {
        if requirement.verifier != HTTP_CONTRACT_VERIFIER {
            continue;
        }
        let path = requirement.path.as_deref().unwrap_or("/");
        let artifact_path = path.strip_prefix('/').unwrap_or(path);
        let artifact_path = if artifact_path.is_empty() {
            entry
        } else {
            artifact_path
        };
        if !tree.entries.iter().any(|entry| entry.path == artifact_path) {
            return Err(profile(
                "HTTP requirement does not name a file in the portable tree",
            ));
        }
        if requirement.body_digest.is_some() && artifact_path == entry {
            return Err(profile(
                "the initial hosted profile cannot preserve an entry-document body digest",
            ));
        }
    }
    Ok(())
}

fn serve_request(
    mut stream: TcpStream,
    routes: &BTreeMap<String, (PathBuf, String)>,
    entry_route: &str,
    spa_fallback: bool,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut request_line = String::new();
    BufReader::new(stream.try_clone()?)
        .take(8 * 1024)
        .read_line(&mut request_line)?;
    let mut fields = request_line.split_whitespace();
    let method = fields.next().unwrap_or_default();
    let raw_path = fields.next().unwrap_or_default();
    let request_path = raw_path.split('?').next().unwrap_or_default();
    if !matches!(method, "GET" | "HEAD") || !request_path.starts_with('/') {
        return write_response(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"Bad Request\n",
            method,
        );
    }
    let route = if request_path == "/" {
        entry_route
    } else {
        request_path
    };
    let selected = routes
        .get(route)
        .or_else(|| spa_fallback.then(|| routes.get(entry_route)).flatten());
    let Some((path, media_type)) = selected else {
        return write_response(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"Not Found\n",
            method,
        );
    };
    let body = fs::read(path)?;
    write_response(&mut stream, 200, media_type, &body, method)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    media_type: &str,
    body: &[u8],
    method: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {media_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn validate_tree(
    bundle: &PortableApplicationBundle,
    tree: &PortableTreeV1,
    expected_schema: &str,
) -> Result<(), PortableApplicationError> {
    if tree.schema != expected_schema || tree.entries.is_empty() {
        return Err(profile("portable tree must contain at least one file"));
    }
    let mut previous: Option<&str> = None;
    for entry in &tree.entries {
        validate_relative_path(&entry.path).map_err(|error| profile(error.to_string()))?;
        if previous.is_some_and(|path| path >= entry.path.as_str()) {
            return Err(profile(
                "portable tree entries must be uniquely sorted by path",
            ));
        }
        previous = Some(&entry.path);
        let expected_media_type = portable_media_type(&entry.path, expected_schema)?;
        if entry.media_type != expected_media_type {
            return Err(profile(format!("media type mismatch for `{}`", entry.path)));
        }
        let reference = parse_ref(&entry.content_ref, "tree entry content_ref")?;
        let descriptor = bundle
            .descriptor(&reference)
            .ok_or_else(|| PortableBundleError::IncompleteClosure(reference.clone()))?;
        if descriptor.kind != PortableBundleObjectKind::Blob
            || descriptor.size != entry.size
            || descriptor.schema.is_some()
        {
            return Err(profile(format!(
                "invalid blob descriptor for `{}`",
                entry.path
            )));
        }
    }
    Ok(())
}

fn portable_media_type(
    path: &str,
    tree_schema: &str,
) -> Result<&'static str, PortableApplicationError> {
    if let Some(media_type) = media_type_for(path) {
        return Ok(media_type);
    }
    if tree_schema == PORTABLE_TREE_V2_SCHEMA {
        return Ok("application/octet-stream");
    }
    Err(profile(format!("unsupported media type for `{path}`")))
}

fn structured<T: DeserializeOwned>(
    bundle: &PortableApplicationBundle,
    reference: &ContentRef,
    expected_schema: &str,
) -> Result<T, PortableApplicationError> {
    let descriptor = bundle
        .descriptor(reference)
        .ok_or_else(|| PortableBundleError::IncompleteClosure(reference.clone()))?;
    if descriptor.kind != PortableBundleObjectKind::Structured
        || descriptor.schema.as_deref() != Some(expected_schema)
    {
        return Err(profile(format!(
            "{} must name a structured {expected_schema} object",
            reference
        )));
    }
    Ok(serde_json::from_slice(&bundle.payload_bytes(reference)?)?)
}

fn portable_reference_registry() -> Result<PortableReferenceRegistry, PortableApplicationError> {
    let mut registry = PortableReferenceRegistry::default();
    registry.register(Arc::new(ContractReferences))?;
    registry.register(Arc::new(ApplicationReferences))?;
    registry.register(Arc::new(ApplicationV2References))?;
    registry.register(Arc::new(DerivationReferences))?;
    registry.register(Arc::new(TreeReferences))?;
    registry.register(Arc::new(TreeV2References))?;
    registry.register(Arc::new(InstanceSnapshotReferences))?;
    Ok(registry)
}

struct ContractReferences;

impl PortableReferenceExtractor for ContractReferences {
    fn schema(&self) -> &str {
        BOUND_CONTRACT_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        let contract = parse_for_extraction::<BoundContract>(BOUND_CONTRACT_SCHEMA, bytes)?;
        contract
            .requirements
            .into_iter()
            .filter(|requirement| requirement.verifier == INSTANCE_SNAPSHOT_CONTRACT_VERIFIER)
            .map(|requirement| {
                let digest =
                    requirement
                        .digest
                        .ok_or_else(|| PortableBundleError::ReferenceExtraction {
                            schema: BOUND_CONTRACT_SCHEMA.to_owned(),
                            reason: "Instance snapshot requirement omitted digest".to_owned(),
                        })?;
                parse_for_extraction_ref(BOUND_CONTRACT_SCHEMA, &digest)
            })
            .collect()
    }
}

struct ApplicationReferences;

impl PortableReferenceExtractor for ApplicationReferences {
    fn schema(&self) -> &str {
        APPLICATION_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        let application = parse_for_extraction::<ApplicationV1>(APPLICATION_SCHEMA, bytes)?;
        application
            .surfaces
            .into_iter()
            .map(|surface| parse_for_extraction_ref(APPLICATION_SCHEMA, &surface.artifact_ref))
            .collect()
    }
}

struct DerivationReferences;

struct ApplicationV2References;

impl PortableReferenceExtractor for ApplicationV2References {
    fn schema(&self) -> &str {
        APPLICATION_V2_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        parse_for_extraction::<ApplicationV2>(APPLICATION_V2_SCHEMA, bytes)?;
        Ok(Vec::new())
    }
}

impl PortableReferenceExtractor for DerivationReferences {
    fn schema(&self) -> &str {
        BOUND_DERIVATION_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        let derivation = parse_for_extraction::<BoundDerivation>(BOUND_DERIVATION_SCHEMA, bytes)?;
        derivation
            .inputs
            .into_iter()
            .map(|input| parse_for_extraction_ref(BOUND_DERIVATION_SCHEMA, &input.content_ref))
            .collect()
    }
}

struct TreeReferences;

impl PortableReferenceExtractor for TreeReferences {
    fn schema(&self) -> &str {
        PORTABLE_TREE_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        let tree = parse_for_extraction::<PortableTreeV1>(PORTABLE_TREE_SCHEMA, bytes)?;
        tree.entries
            .into_iter()
            .map(|entry| parse_for_extraction_ref(PORTABLE_TREE_SCHEMA, &entry.content_ref))
            .collect()
    }
}

struct TreeV2References;

struct InstanceSnapshotReferences;

impl PortableReferenceExtractor for InstanceSnapshotReferences {
    fn schema(&self) -> &str {
        INSTANCE_SNAPSHOT_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        let snapshot = parse_for_extraction::<InstanceSnapshotV1>(INSTANCE_SNAPSHOT_SCHEMA, bytes)?;
        validate_snapshot(&snapshot).map_err(|error| PortableBundleError::ReferenceExtraction {
            schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
            reason: error.to_string(),
        })
    }
}

impl PortableReferenceExtractor for TreeV2References {
    fn schema(&self) -> &str {
        PORTABLE_TREE_V2_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        let tree = parse_for_extraction::<PortableTreeV1>(PORTABLE_TREE_V2_SCHEMA, bytes)?;
        tree.entries
            .into_iter()
            .map(|entry| parse_for_extraction_ref(PORTABLE_TREE_V2_SCHEMA, &entry.content_ref))
            .collect()
    }
}

fn parse_for_extraction<T: DeserializeOwned>(
    schema: &str,
    bytes: &[u8],
) -> Result<T, PortableBundleError> {
    serde_json::from_slice(bytes).map_err(|error| PortableBundleError::ReferenceExtraction {
        schema: schema.to_owned(),
        reason: error.to_string(),
    })
}

fn parse_for_extraction_ref(schema: &str, value: &str) -> Result<ContentRef, PortableBundleError> {
    ContentRef::parse(value.to_owned()).map_err(|error| PortableBundleError::ReferenceExtraction {
        schema: schema.to_owned(),
        reason: error.to_string(),
    })
}

enum ObjectToEncode {
    Structured { schema: String, bytes: Vec<u8> },
    Blob(Vec<u8>),
}

fn read_authoring_draft(source_root: &Path) -> Result<AuthoringDraft, PortableApplicationError> {
    let capsule_toml = read(source_root.join(CAPSULE_FILE_NAME))?;
    let capsule_toml = std::str::from_utf8(&capsule_toml)
        .map_err(|error| profile(format!("capsule.toml is not UTF-8: {error}")))?;
    parse_capsule_toml(capsule_toml)
        .map_err(|error| PortableApplicationError::CapsuleToml(error.to_string()))
}

fn build_portable_tree(
    source_root: &Path,
) -> Result<(BTreeMap<String, ObjectToEncode>, String), PortableApplicationError> {
    build_portable_tree_with_schema(source_root, PORTABLE_TREE_SCHEMA)
}

fn build_portable_tree_with_schema(
    source_root: &Path,
    tree_schema: &str,
) -> Result<(BTreeMap<String, ObjectToEncode>, String), PortableApplicationError> {
    let files = collect_portable_files(source_root, tree_schema)?;
    let mut objects = BTreeMap::<String, ObjectToEncode>::new();
    let mut entries = Vec::with_capacity(files.len());
    for (relative, path, media_type) in files {
        let bytes = read(path)?;
        let reference = bundle_sha256(&bytes);
        entries.push(PortableTreeEntryV1 {
            path: relative,
            content_ref: reference.clone(),
            size: bytes.len() as u64,
            media_type,
        });
        objects
            .entry(reference)
            .or_insert(ObjectToEncode::Blob(bytes));
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let tree = PortableTreeV1 {
        schema: tree_schema.to_owned(),
        entries,
    };
    let tree_ref = add_structured(&mut objects, &tree, tree_schema)?;
    Ok((objects, tree_ref))
}

fn finish_bundle(
    objects: BTreeMap<String, ObjectToEncode>,
    root_contract_ref: String,
    application_ref: String,
    mut derivations: Vec<String>,
) -> PortableApplicationBundle {
    let mut descriptors = Vec::with_capacity(objects.len());
    let mut payloads = Vec::with_capacity(objects.len());
    for (reference, object) in objects {
        let (kind, schema, bytes) = match object {
            ObjectToEncode::Structured { schema, bytes } => {
                (PortableBundleObjectKind::Structured, Some(schema), bytes)
            }
            ObjectToEncode::Blob(bytes) => (PortableBundleObjectKind::Blob, None, bytes),
        };
        descriptors.push(PortableBundleObjectDescriptor {
            reference: reference.clone(),
            kind,
            schema,
            size: bytes.len() as u64,
        });
        payloads.push(PortableBundlePayload {
            reference,
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    derivations.sort();
    PortableApplicationBundle {
        index: PortableBundleIndex {
            version: PORTABLE_APPLICATION_BUNDLE_VERSION,
            profile: PORTABLE_APPLICATION_PROFILE.to_owned(),
            root_contract_ref,
            application_ref,
            instance_snapshot_ref: None,
            derivations,
            objects: descriptors,
        },
        payloads,
        portability: None,
        signatures: Vec::new(),
    }
}

fn add_structured<T: Serialize>(
    objects: &mut BTreeMap<String, ObjectToEncode>,
    value: &T,
    schema: &str,
) -> Result<String, PortableApplicationError> {
    let bytes = serde_jcs::to_vec(value)?;
    let reference = bundle_sha256(&bytes);
    objects.insert(
        reference.clone(),
        ObjectToEncode::Structured {
            schema: schema.to_owned(),
            bytes,
        },
    );
    Ok(reference)
}

fn collect_portable_files(
    root: &Path,
    tree_schema: &str,
) -> Result<Vec<(String, PathBuf, String)>, PortableApplicationError> {
    fn visit(
        root: &Path,
        current: &Path,
        tree_schema: &str,
        output: &mut Vec<(String, PathBuf, String)>,
    ) -> Result<(), PortableApplicationError> {
        let mut entries = fs::read_dir(current)
            .map_err(|source| PortableApplicationError::Io {
                path: current.to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| PortableApplicationError::Io {
                path: current.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| PortableApplicationError::Io {
                    path: path.clone(),
                    source,
                })?;
            if file_type.is_symlink() {
                return Err(profile(format!(
                    "source contains symlink: {}",
                    path.display()
                )));
            }
            if file_type.is_dir() {
                visit(root, &path, tree_schema, output)?;
                continue;
            }
            if !file_type.is_file() {
                return Err(profile(format!(
                    "source contains non-regular file: {}",
                    path.display()
                )));
            }
            let relative = path
                .strip_prefix(root)
                .expect("recursive path is below root")
                .to_str()
                .ok_or_else(|| profile("source path is not UTF-8"))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            if relative == CAPSULE_FILE_NAME {
                continue;
            }
            validate_relative_path(&relative).map_err(|error| profile(error.to_string()))?;
            let media_type = portable_media_type(&relative, tree_schema)?;
            output.push((relative, path, media_type.to_owned()));
        }
        Ok(())
    }

    let mut output = Vec::new();
    visit(root, root, tree_schema, &mut output)?;
    output.sort_by(|left, right| left.0.cmp(&right.0));
    if output.is_empty() {
        return Err(profile("portable application contains no static files"));
    }
    Ok(output)
}

fn collect_materialized_files(root: &Path) -> Result<BTreeSet<String>, PortableApplicationError> {
    fn visit(
        root: &Path,
        current: &Path,
        output: &mut BTreeSet<String>,
    ) -> Result<(), PortableApplicationError> {
        let entries = fs::read_dir(current).map_err(|source| PortableApplicationError::Io {
            path: current.to_path_buf(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| PortableApplicationError::Io {
                path: current.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| PortableApplicationError::Io {
                    path: path.clone(),
                    source,
                })?;
            if file_type.is_dir() {
                visit(root, &path, output)?;
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .expect("recursive path is below root")
                    .to_str()
                    .ok_or_else(|| profile("materialized path is not UTF-8"))?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                output.insert(relative);
            } else {
                return Err(profile("materialized tree contains a non-regular entry"));
            }
        }
        Ok(())
    }

    let mut output = BTreeSet::new();
    visit(root, root, &mut output)?;
    Ok(output)
}

fn read(path: PathBuf) -> Result<Vec<u8>, PortableApplicationError> {
    fs::read(&path).map_err(|source| PortableApplicationError::Io { path, source })
}

fn parse_ref(value: &str, field: &str) -> Result<ContentRef, PortableApplicationError> {
    let reference = ContentRef::parse(value.to_owned())
        .map_err(|error| profile(format!("{field}: {error}")))?;
    if reference.algorithm() != "sha256" {
        return Err(profile(format!("{field} must use SHA-256")));
    }
    Ok(reference)
}

fn profile(message: impl Into<String>) -> PortableApplicationError {
    PortableApplicationError::Profile(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_v4_transport_keeps_datasette_contract_and_derivations() {
        let (_, bundle) =
            build_dynamic_process_oci_bundle(&datasette_fixture_root(), &datasette_spec()).unwrap();
        let original = validate_all_derivations(&bundle).unwrap();
        let wheel = original[0]
            .tree
            .entries
            .iter()
            .find(|entry| entry.path.ends_with(".whl"))
            .unwrap();
        let wheel_ref = wheel.content_ref.clone();
        let (bytes, _) = crate::portability_export::repack_portable_dependencies(
            &bundle,
            ato_objects::PortableDependencyProfile::Thin,
            &BTreeMap::from([(
                wheel_ref.clone(),
                vec!["https://files.pythonhosted.org/fixed.whl".to_owned()],
            )]),
        )
        .unwrap();
        let decoded = decode_portable_application_bundle(&bytes).unwrap();
        let sparse = validate_all_derivations(&decoded).unwrap();
        assert_eq!(original[0].contract_ref, sparse[0].contract_ref);
        assert_eq!(
            original
                .iter()
                .map(|route| &route.derivation_ref)
                .collect::<Vec<_>>(),
            sparse
                .iter()
                .map(|route| &route.derivation_ref)
                .collect::<Vec<_>>()
        );

        let mut missing = decoded.clone();
        missing
            .portability
            .as_mut()
            .unwrap()
            .external_objects
            .clear();
        assert!(encode_portable_application_bundle(&missing).is_err());

        let mut false_offline = decoded;
        false_offline.portability.as_mut().unwrap().profile =
            ato_objects::PortableDependencyProfile::Offline;
        assert!(matches!(
            encode_portable_application_bundle(&false_offline),
            Err(PortableBundleError::OfflineExternalObject(_))
        ));
    }

    #[test]
    fn repacking_order_and_profile_do_not_change_semantic_refs() {
        let (_, bundle) =
            build_dynamic_process_oci_bundle(&datasette_fixture_root(), &datasette_spec()).unwrap();
        let route = validate_all_derivations(&bundle).unwrap();
        let wheel = route[0]
            .tree
            .entries
            .iter()
            .find(|entry| entry.path.ends_with(".whl"))
            .unwrap();
        let sources = BTreeMap::from([(
            wheel.content_ref.clone(),
            vec!["https://files.pythonhosted.org/fixed.whl".to_owned()],
        )]);
        let (_, thin) = crate::portability_export::repack_portable_dependencies(
            &bundle,
            ato_objects::PortableDependencyProfile::Thin,
            &sources,
        )
        .unwrap();
        let (_, cached) = crate::portability_export::repack_portable_dependencies(
            &bundle,
            ato_objects::PortableDependencyProfile::Cached,
            &sources,
        )
        .unwrap();
        assert_eq!(thin.index.root_contract_ref, cached.index.root_contract_ref);
        assert_eq!(thin.index.derivations, cached.index.derivations);

        let (_, thin_again) = crate::portability_export::repack_portable_dependencies(
            &thin,
            ato_objects::PortableDependencyProfile::Thin,
            &sources,
        )
        .unwrap();
        let (_, cached_again) = crate::portability_export::repack_portable_dependencies(
            &cached,
            ato_objects::PortableDependencyProfile::Cached,
            &sources,
        )
        .unwrap();
        assert_eq!(
            thin.index.root_contract_ref,
            thin_again.index.root_contract_ref
        );
        assert_eq!(thin.index.derivations, thin_again.index.derivations);
        assert_eq!(
            cached.index.root_contract_ref,
            cached_again.index.root_contract_ref
        );
        assert_eq!(cached.index.derivations, cached_again.index.derivations);

        let differently_ordered_sources = BTreeMap::from([(
            wheel.content_ref.clone(),
            vec![
                "https://files.pythonhosted.org/second.whl".to_owned(),
                "https://files.pythonhosted.org/first.whl".to_owned(),
            ],
        )]);
        let (_, reordered) = crate::portability_export::repack_portable_dependencies(
            &bundle,
            ato_objects::PortableDependencyProfile::Thin,
            &differently_ordered_sources,
        )
        .unwrap();
        assert_eq!(
            thin.index.root_contract_ref,
            reordered.index.root_contract_ref
        );
        assert_eq!(thin.index.derivations, reordered.index.derivations);
    }

    #[test]
    fn embedded_wheel_tamper_and_omission_fail_before_route_selection() {
        let (_, bundle) =
            build_dynamic_process_oci_bundle(&datasette_fixture_root(), &datasette_spec()).unwrap();
        let (_, cached) = crate::portability_export::repack_portable_dependencies(
            &bundle,
            ato_objects::PortableDependencyProfile::Cached,
            &BTreeMap::new(),
        )
        .unwrap();
        let route = validate_all_derivations(&cached).unwrap();
        let wheel = route[0]
            .tree
            .entries
            .iter()
            .find(|entry| entry.path.ends_with(".whl"))
            .unwrap();
        let mut tampered = cached.clone();
        let payload = tampered
            .payloads
            .iter_mut()
            .find(|payload| payload.reference == wheel.content_ref)
            .unwrap();
        payload.bytes = base64::engine::general_purpose::STANDARD.encode(b"wrong wheel");
        assert!(encode_portable_application_bundle(&tampered).is_err());

        let mut missing = cached;
        missing
            .payloads
            .retain(|payload| payload.reference != wheel.content_ref);
        assert!(encode_portable_application_bundle(&missing).is_err());
    }

    fn route_ref(bundle: &PortableApplicationBundle, kind: PortableRealizationKind) -> String {
        validate_all_derivations(bundle)
            .unwrap()
            .into_iter()
            .find(|route| route.realization == kind)
            .unwrap()
            .derivation_ref
            .to_string()
    }

    fn derivation(bundle: &PortableApplicationBundle, reference: &str) -> BoundDerivation {
        let reference = ContentRef::parse(reference).unwrap();
        serde_json::from_slice(&bundle.payload_bytes(&reference).unwrap()).unwrap()
    }

    fn add_derivation(
        bundle: &mut PortableApplicationBundle,
        derivation: &BoundDerivation,
    ) -> String {
        let bytes = serde_jcs::to_vec(derivation).unwrap();
        let reference = bundle_sha256(&bytes);
        bundle.index.objects.push(PortableBundleObjectDescriptor {
            reference: reference.clone(),
            kind: PortableBundleObjectKind::Structured,
            schema: Some(BOUND_DERIVATION_SCHEMA.to_owned()),
            size: bytes.len() as u64,
        });
        bundle
            .index
            .objects
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        bundle.payloads.push(PortableBundlePayload {
            reference: reference.clone(),
            bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
        bundle
            .payloads
            .sort_by(|left, right| left.reference.cmp(&right.reference));
        bundle.index.derivations.push(reference.clone());
        bundle.index.derivations.sort();
        reference
    }

    fn remove_derivation(bundle: &mut PortableApplicationBundle, reference: &str) {
        bundle.index.derivations.retain(|value| value != reference);
        bundle
            .index
            .objects
            .retain(|value| value.reference != reference);
        bundle.payloads.retain(|value| value.reference != reference);
    }

    fn replace_derivation(
        bundle: &mut PortableApplicationBundle,
        reference: &str,
        mutate: impl FnOnce(&mut BoundDerivation),
    ) -> String {
        let mut value = derivation(bundle, reference);
        mutate(&mut value);
        remove_derivation(bundle, reference);
        add_derivation(bundle, &value)
    }

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
    }

    fn cached_static_v4_bundle() -> PortableApplicationBundle {
        let (_, bundle) = build_static_bundle(&fixture_root(), "Ato portability proof").unwrap();
        crate::portability_export::repack_portable_dependencies(
            &bundle,
            ato_objects::PortableDependencyProfile::Cached,
            &BTreeMap::new(),
        )
        .unwrap()
        .1
    }

    fn saved_instance_snapshot() -> (
        crate::instance_snapshot::InstanceSnapshotV1,
        BTreeMap<String, Vec<u8>>,
    ) {
        let saved_data = br#"{"todos":["one","two"]}"#.to_vec();
        let asset = b"portable-photo-bytes".to_vec();
        let saved_data_ref = bundle_sha256(&saved_data);
        let asset_ref = bundle_sha256(&asset);
        (
            crate::instance_snapshot::InstanceSnapshotV1 {
                schema: crate::instance_snapshot::INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
                resources: vec![crate::instance_snapshot::InstanceSnapshotResourceV1 {
                    slot: "main".to_owned(),
                    protocol: "ato.data.json@1".to_owned(),
                    content_ref: saved_data_ref.clone(),
                }],
                assets: vec![crate::instance_snapshot::InstanceSnapshotAssetV1 {
                    alias: "asset-1".to_owned(),
                    content_ref: asset_ref.clone(),
                    filename: "photo.jpg".to_owned(),
                    content_type: "image/jpeg".to_owned(),
                    size: asset.len() as u64,
                }],
            },
            BTreeMap::from([(saved_data_ref, saved_data), (asset_ref, asset)]),
        )
    }

    #[test]
    fn saved_snapshot_changes_k_without_changing_the_derivation() {
        let source = cached_static_v4_bundle();
        let original_contract = source.index.root_contract_ref.clone();
        let original_derivations = source.index.derivations.clone();
        let (snapshot, content) = saved_instance_snapshot();

        let (bytes, saved) =
            crate::instance_snapshot::attach_instance_snapshot(&source, snapshot, &content)
                .unwrap();
        let (_, validated) = validate_bytes_all(&bytes).unwrap();

        assert_ne!(saved.index.root_contract_ref, original_contract);
        assert_eq!(saved.index.derivations, original_derivations);
        assert!(validated.iter().all(|route| {
            route
                .instance_snapshot_ref
                .as_ref()
                .map(ToString::to_string)
                == saved.index.instance_snapshot_ref
        }));
    }

    #[test]
    fn saved_snapshot_export_is_deterministic() {
        let source = cached_static_v4_bundle();
        let (snapshot, content) = saved_instance_snapshot();

        let first =
            crate::instance_snapshot::attach_instance_snapshot(&source, snapshot.clone(), &content)
                .unwrap()
                .0;
        let second =
            crate::instance_snapshot::attach_instance_snapshot(&source, snapshot, &content)
                .unwrap()
                .0;

        assert_eq!(first, second);
    }

    #[test]
    fn replacing_saved_snapshot_mints_new_k_and_prunes_old_state() {
        let source = cached_static_v4_bundle();
        let (first_snapshot, first_content) = saved_instance_snapshot();
        let old_resource_ref = first_snapshot.resources[0].content_ref.clone();
        let (_, first) = crate::instance_snapshot::attach_instance_snapshot(
            &source,
            first_snapshot,
            &first_content,
        )
        .unwrap();
        let old_snapshot_ref = first.index.instance_snapshot_ref.clone().unwrap();

        let (mut next_snapshot, mut next_content) = saved_instance_snapshot();
        next_content.remove(&old_resource_ref);
        let next_saved_data = br#"{"todos":["one","two","three"]}"#.to_vec();
        let next_saved_data_ref = bundle_sha256(&next_saved_data);
        next_snapshot.resources[0].content_ref = next_saved_data_ref.clone();
        next_content.insert(next_saved_data_ref, next_saved_data);

        let (_, next) = crate::instance_snapshot::attach_instance_snapshot(
            &first,
            next_snapshot,
            &next_content,
        )
        .unwrap();

        assert_ne!(next.index.root_contract_ref, first.index.root_contract_ref);
        assert_eq!(next.index.derivations, first.index.derivations);
        assert!(!next.index.objects.iter().any(|object| {
            object.reference == old_snapshot_ref || object.reference == old_resource_ref
        }));
    }

    #[test]
    fn saved_snapshot_rejects_missing_declared_content() {
        let source = cached_static_v4_bundle();
        let (snapshot, mut content) = saved_instance_snapshot();
        content.pop_first();

        let error = crate::instance_snapshot::attach_instance_snapshot(&source, snapshot, &content)
            .unwrap_err();

        assert!(error.to_string().contains("content map must equal"));
    }

    #[test]
    fn saved_snapshot_rejects_content_with_the_wrong_digest() {
        let source = cached_static_v4_bundle();
        let (snapshot, mut content) = saved_instance_snapshot();
        let bytes = content.values_mut().next().unwrap();
        bytes.push(b'!');

        let error = crate::instance_snapshot::attach_instance_snapshot(&source, snapshot, &content)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("do not match declared reference")
        );
    }

    #[test]
    fn saved_snapshot_requires_v4_transport() {
        let (_, source) = build_static_bundle(&fixture_root(), "Ato portability proof").unwrap();
        let (snapshot, content) = saved_instance_snapshot();

        let error = crate::instance_snapshot::attach_instance_snapshot(&source, snapshot, &content)
            .unwrap_err();

        assert!(error.to_string().contains("require portable bundle v4"));
    }

    #[test]
    fn dependency_repack_keeps_saved_snapshot_k_and_derivation() {
        let source = cached_static_v4_bundle();
        let (snapshot, content) = saved_instance_snapshot();
        let (_, saved) =
            crate::instance_snapshot::attach_instance_snapshot(&source, snapshot, &content)
                .unwrap();

        let (_, offline) = crate::portability_export::repack_portable_dependencies(
            &saved,
            ato_objects::PortableDependencyProfile::Offline,
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(
            offline.index.root_contract_ref,
            saved.index.root_contract_ref
        );
        assert_eq!(offline.index.derivations, saved.index.derivations);
        assert_eq!(
            offline.index.instance_snapshot_ref,
            saved.index.instance_snapshot_ref
        );
    }

    fn multi_fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-multi-derivation")
    }

    fn datasette_fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-datasette")
    }

    fn datasette_spec() -> PortableDynamicBundleSpec {
        PortableDynamicBundleSpec {
            title: "Datasette catalog".to_owned(),
            surface_path: "/".to_owned(),
            guest_port: 8000,
            process: PortableExecutionSpec {
                runtimes: BTreeMap::from([(PYTHON_RUNTIME.to_owned(), "3.12".to_owned())]),
                argv: vec!["python3".to_owned(), "bootstrap.py".to_owned()],
                cwd: ".".to_owned(),
                env: BTreeMap::new(),
            },
            oci: PortableExecutionSpec {
                runtimes: BTreeMap::from([
                    (
                        OCI_IMAGE_RUNTIME.to_owned(),
                        format!("docker.io/example/datasette@sha256:{}", "ab".repeat(32)),
                    ),
                    (OCI_PLATFORM_RUNTIME.to_owned(), "linux/amd64".to_owned()),
                    (OCI_MEMORY_BYTES_RUNTIME.to_owned(), "268435456".to_owned()),
                    (OCI_CPU_MILLIS_RUNTIME.to_owned(), "1000".to_owned()),
                    (OCI_PIDS_LIMIT_RUNTIME.to_owned(), "128".to_owned()),
                ]),
                argv: vec!["datasette".to_owned(), "/app/catalog.db".to_owned()],
                cwd: ".".to_owned(),
                env: BTreeMap::new(),
            },
            requirements: vec![
                PortableHttpRequirementSpec {
                    id: "entry".to_owned(),
                    path: "/".to_owned(),
                    status: 200,
                    body_digest: None,
                },
                PortableHttpRequirementSpec {
                    id: "rows".to_owned(),
                    path: "/catalog/items.json?_shape=array&_sort=id".to_owned(),
                    status: 200,
                    body_digest: Some(format!("sha256:{}", "cd".repeat(32))),
                },
            ],
        }
    }

    #[test]
    fn fixture_build_is_deterministic_and_fully_validated() {
        let (left, _) = build_static_bundle(&fixture_root(), "Ato portability proof").unwrap();
        let (right, _) = build_static_bundle(&fixture_root(), "Ato portability proof").unwrap();
        assert_eq!(left, right);

        let (_, validated) = validate_bytes(&left).unwrap();
        assert_eq!(
            validated.contract_ref.as_str(),
            validated.contract.contract_ref().unwrap()
        );
        assert_eq!(validated.application.surfaces.len(), 1);
        assert_eq!(
            validated
                .tree
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            vec!["index.html", "proof.txt"]
        );
    }

    #[test]
    fn materialization_recomputes_every_file_identity() {
        let (bytes, _) = build_static_bundle(&fixture_root(), "Ato portability proof").unwrap();
        let (bundle, validated) = validate_bytes(&bytes).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("app");
        materialize_tree(&bundle, &validated, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("proof.txt")).unwrap(),
            b"ato-k-interop-v1\n"
        );

        fs::write(destination.join("proof.txt"), b"tampered\n").unwrap();
        assert!(verify_materialized_tree(&validated, &destination).is_err());
    }

    #[test]
    fn local_server_returns_the_exact_proof_body() {
        let (bytes, _) = build_static_bundle(&fixture_root(), "Ato portability proof").unwrap();
        let (bundle, validated) = validate_bytes(&bytes).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let destination = parent.path().join("app");
        materialize_tree(&bundle, &validated, &destination).unwrap();
        let server = StaticApplicationServer::start(&destination, &validated).unwrap();
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream
            .write_all(b"GET /proof.txt HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(b"ato-k-interop-v1\n"));
    }

    #[test]
    fn one_contract_has_distinct_static_and_process_derivations() {
        let (left, bundle) =
            build_multi_derivation_bundle(&multi_fixture_root(), "Ato multi-route proof").unwrap();
        let (right, _) =
            build_multi_derivation_bundle(&multi_fixture_root(), "Ato multi-route proof").unwrap();
        assert_eq!(left, right);
        assert_eq!(bundle.index.derivations.len(), 2);
        assert_ne!(bundle.index.derivations[0], bundle.index.derivations[1]);

        let validated = validate_all_derivations(&bundle).unwrap();
        assert_eq!(validated.len(), 2);
        assert!(validated.iter().all(|route| {
            route.contract_ref.as_str() == bundle.index.root_contract_ref
                && route.contract.contract_ref().unwrap() == bundle.index.root_contract_ref
        }));
        assert_eq!(
            validated
                .iter()
                .map(|route| route.realization)
                .collect::<BTreeSet<_>>(),
            [
                PortableRealizationKind::StaticWeb,
                PortableRealizationKind::LocalProcess,
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn datasette_uses_one_contract_with_process_and_oci_routes() {
        let (_, bundle) =
            build_dynamic_process_oci_bundle(&datasette_fixture_root(), &datasette_spec()).unwrap();
        let routes = validate_all_derivations(&bundle).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(
            routes
                .iter()
                .map(|route| route.contract_ref.as_str())
                .collect::<BTreeSet<_>>(),
            [bundle.index.root_contract_ref.as_str()]
                .into_iter()
                .collect()
        );
        assert_eq!(
            routes
                .iter()
                .map(|route| route.realization)
                .collect::<BTreeSet<_>>(),
            [
                PortableRealizationKind::LocalProcess,
                PortableRealizationKind::OciContainer,
            ]
            .into_iter()
            .collect()
        );
        let paths = routes[0]
            .tree
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<BTreeSet<_>>();
        assert!(!paths.contains("catalog/items.json"));
        assert!(routes[0].contract.requirements.iter().any(|requirement| {
            requirement.path.as_deref() == Some("/catalog/items.json?_shape=array&_sort=id")
        }));
    }

    #[test]
    fn missing_or_tampered_dynamic_inputs_fail_closure_before_route_selection() {
        let (_, original) =
            build_dynamic_process_oci_bundle(&datasette_fixture_root(), &datasette_spec()).unwrap();
        let route = validate_all_derivations(&original).unwrap().remove(0);
        let wheel_path = route
            .tree
            .entries
            .iter()
            .find(|entry| entry.path.starts_with("wheels/") && entry.path.ends_with(".whl"))
            .unwrap()
            .path
            .clone();

        for path in ["catalog.db", wheel_path.as_str()] {
            let mut missing = original.clone();
            let content_ref = route
                .tree
                .entries
                .iter()
                .find(|entry| entry.path == path)
                .unwrap()
                .content_ref
                .clone();
            missing
                .index
                .objects
                .retain(|object| object.reference != content_ref);
            missing
                .payloads
                .retain(|payload| payload.reference != content_ref);
            assert!(
                validate_all_derivations(&missing).is_err(),
                "missing {path}"
            );
        }

        let mut tampered = original.clone();
        let database_ref = route
            .tree
            .entries
            .iter()
            .find(|entry| entry.path == "catalog.db")
            .unwrap()
            .content_ref
            .clone();
        tampered
            .payloads
            .iter_mut()
            .find(|payload| payload.reference == database_ref)
            .unwrap()
            .bytes = base64::engine::general_purpose::STANDARD.encode(b"tampered database");
        assert!(validate_all_derivations(&tampered).is_err());
    }

    #[test]
    fn a_rehashed_broken_dynamic_route_does_not_poison_the_other_route() {
        let (_, original) =
            build_dynamic_process_oci_bundle(&datasette_fixture_root(), &datasette_spec()).unwrap();
        let contract_ref = original.index.root_contract_ref.clone();
        let process_ref = route_ref(&original, PortableRealizationKind::LocalProcess);
        let oci_ref = route_ref(&original, PortableRealizationKind::OciContainer);

        let mut broken_process = original.clone();
        let broken_process_ref =
            replace_derivation(&mut broken_process, &process_ref, |derivation| {
                derivation.steps[0].argv.clear();
            });
        assert_eq!(broken_process.index.root_contract_ref, contract_ref);
        validate_bundle_for_derivation(&broken_process, &oci_ref).unwrap();
        assert!(validate_bundle_for_derivation(&broken_process, &broken_process_ref).is_err());

        let mut broken_oci = original.clone();
        let broken_oci_ref = replace_derivation(&mut broken_oci, &oci_ref, |derivation| {
            derivation.runtimes.insert(
                OCI_IMAGE_RUNTIME.to_owned(),
                "docker.io/example/datasette:latest".to_owned(),
            );
        });
        assert_eq!(broken_oci.index.root_contract_ref, contract_ref);
        validate_bundle_for_derivation(&broken_oci, &process_ref).unwrap();
        assert!(validate_bundle_for_derivation(&broken_oci, &broken_oci_ref).is_err());
    }

    #[test]
    fn multiple_derivations_require_explicit_selection() {
        let (_, bundle) =
            build_multi_derivation_bundle(&multi_fixture_root(), "Ato multi-route proof").unwrap();
        assert!(
            validate_bundle(&bundle)
                .unwrap_err()
                .to_string()
                .contains("select one explicitly")
        );
        assert!(
            validate_bundle_for_derivation(&bundle, &format!("sha256:{}", "0".repeat(64)))
                .unwrap_err()
                .to_string()
                .contains("is not declared")
        );
    }

    #[test]
    fn each_derivation_can_fail_without_changing_k_or_the_other_route() {
        let (_, original) =
            build_multi_derivation_bundle(&multi_fixture_root(), "Ato multi-route proof").unwrap();
        let contract_ref = original.index.root_contract_ref.clone();
        let static_ref = route_ref(&original, PortableRealizationKind::StaticWeb);
        let process_ref = route_ref(&original, PortableRealizationKind::LocalProcess);

        let mut broken_static = original.clone();
        let broken_static_ref = replace_derivation(&mut broken_static, &static_ref, |derivation| {
            derivation.steps[0].entry = Some("missing.html".to_owned());
        });
        assert_eq!(broken_static.index.root_contract_ref, contract_ref);
        validate_bundle_for_derivation(&broken_static, &process_ref).unwrap();
        assert!(validate_bundle_for_derivation(&broken_static, &broken_static_ref).is_err());

        let mut broken_process = original.clone();
        let broken_process_ref =
            replace_derivation(&mut broken_process, &process_ref, |derivation| {
                derivation.steps[0].argv.clear();
            });
        assert_eq!(broken_process.index.root_contract_ref, contract_ref);
        validate_bundle_for_derivation(&broken_process, &static_ref).unwrap();
        assert!(validate_bundle_for_derivation(&broken_process, &broken_process_ref).is_err());
    }

    #[test]
    fn derivation_membership_and_order_never_enter_contract_identity() {
        let (canonical_bytes, original) =
            build_multi_derivation_bundle(&multi_fixture_root(), "Ato multi-route proof").unwrap();
        let contract_ref = original.index.root_contract_ref.clone();
        let process_ref = route_ref(&original, PortableRealizationKind::LocalProcess);

        let mut reordered = original.clone();
        reordered.index.derivations.reverse();
        assert_eq!(reordered.index.root_contract_ref, contract_ref);
        assert!(encode_portable_application_bundle(&reordered).is_err());
        reordered.index.derivations.sort();
        assert_eq!(
            encode_portable_application_bundle(&reordered).unwrap(),
            canonical_bytes
        );

        let mut removed = original.clone();
        remove_derivation(&mut removed, &process_ref);
        assert_eq!(removed.index.root_contract_ref, contract_ref);
        validate_all_derivations(&removed).unwrap();

        let mut added = original.clone();
        let mut alternate = derivation(&added, &process_ref);
        alternate.steps[0].id = "process-site-alternate".to_owned();
        alternate.ports[0].from = "process-site-alternate".to_owned();
        add_derivation(&mut added, &alternate);
        assert_eq!(added.index.root_contract_ref, contract_ref);
        assert_eq!(validate_all_derivations(&added).unwrap().len(), 3);
    }

    #[test]
    fn rewritten_derivation_reference_fails_closure_validation() {
        let (_, mut bundle) =
            build_multi_derivation_bundle(&multi_fixture_root(), "Ato multi-route proof").unwrap();
        bundle.index.derivations[0] = format!("sha256:{}", "0".repeat(64));
        bundle.index.derivations.sort();
        assert!(
            validate_all_derivations(&bundle)
                .unwrap_err()
                .to_string()
                .contains("closure is incomplete")
        );
    }
}
