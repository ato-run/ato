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
    BOUND_CONTRACT_SCHEMA, BOUND_DERIVATION_SCHEMA, BROWSER_PROTOCOL, BindingContext,
    BoundContract, BoundDerivation, EffectClass, HTTP_CONTRACT_VERIFIER, HTTP_PROTOCOL,
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

pub mod validator_agent;

pub const APPLICATION_SCHEMA: &str = "ato.application/1";
pub const PORTABLE_TREE_SCHEMA: &str = "ato.portable-tree/1";

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
    pub contract: BoundContract,
    pub application: ApplicationV1,
    pub derivation: BoundDerivation,
    pub tree_ref: ContentRef,
    pub tree: PortableTreeV1,
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
        let entry_route = format!("/{}", validated.application.surfaces[0].entry);
        let spa_fallback = validated.application.surfaces[0].spa_fallback;
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

pub fn validate_bundle(
    bundle: &PortableApplicationBundle,
) -> Result<ValidatedPortableApplication, PortableApplicationError> {
    let registry = portable_reference_registry()?;
    validate_portable_application_closure(bundle, &registry)?;

    let contract_ref = parse_ref(&bundle.index.root_contract_ref, "root_contract_ref")?;
    let application_ref = parse_ref(&bundle.index.application_ref, "application_ref")?;
    if bundle.index.derivations.len() != 1 {
        return Err(profile(
            "the initial profile requires exactly one derivation",
        ));
    }
    let derivation_ref = parse_ref(&bundle.index.derivations[0], "derivation")?;
    let contract: BoundContract = structured(bundle, &contract_ref, BOUND_CONTRACT_SCHEMA)?;
    let application: ApplicationV1 = structured(bundle, &application_ref, APPLICATION_SCHEMA)?;
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
    validate_initial_route(&contract, &application, &derivation)?;

    let surface = &application.surfaces[0];
    let tree_ref = parse_ref(&surface.artifact_ref, "surface.artifact_ref")?;
    let tree: PortableTreeV1 = structured(bundle, &tree_ref, PORTABLE_TREE_SCHEMA)?;
    validate_tree(bundle, &tree)?;
    validate_http_paths(&contract, &application, &tree)?;

    Ok(ValidatedPortableApplication {
        contract_ref,
        application_ref,
        derivation_ref,
        contract,
        application,
        derivation,
        tree_ref,
        tree,
    })
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
    let capsule_toml = read(source_root.join(CAPSULE_FILE_NAME))?;
    let capsule_toml = std::str::from_utf8(&capsule_toml)
        .map_err(|error| profile(format!("capsule.toml is not UTF-8: {error}")))?;
    let draft = parse_capsule_toml(capsule_toml)
        .map_err(|error| PortableApplicationError::CapsuleToml(error.to_string()))?;

    let files = collect_static_files(source_root)?;
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
        schema: PORTABLE_TREE_SCHEMA.to_owned(),
        entries,
    };
    let tree_ref = add_structured(&mut objects, &tree, PORTABLE_TREE_SCHEMA)?;
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
    let bundle = PortableApplicationBundle {
        index: PortableBundleIndex {
            version: PORTABLE_APPLICATION_BUNDLE_VERSION,
            profile: PORTABLE_APPLICATION_PROFILE.to_owned(),
            root_contract_ref: contract_ref,
            application_ref,
            derivations: vec![derivation_ref],
            objects: descriptors,
        },
        payloads,
        signatures: Vec::new(),
    };
    validate_bundle(&bundle)?;
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
    application: &ApplicationV1,
    derivation: &BoundDerivation,
) -> Result<(), PortableApplicationError> {
    if application.schema != APPLICATION_SCHEMA || application.title.trim().is_empty() {
        return Err(profile("invalid ato.application/1 object"));
    }
    if application.surfaces.len() != 1
        || derivation.inputs.len() != 1
        || derivation.steps.len() != 1
        || derivation.ports.len() != 1
    {
        return Err(profile(
            "the initial profile requires one surface, input, browser step, and port",
        ));
    }
    if !derivation.runtimes.is_empty()
        || !derivation.state.is_empty()
        || derivation.workspace_build.is_some()
        || derivation.workspace_compiler.is_some()
        || derivation.effects != EffectClass::Pure
    {
        return Err(profile(
            "the initial profile forbids runtimes, state, builds, and non-pure effects",
        ));
    }
    let input = &derivation.inputs[0];
    let step = &derivation.steps[0];
    let port = &derivation.ports[0];
    let surface = &application.surfaces[0];
    if input.protocol != WORKSPACE_PROTOCOL
        || step.protocol != BROWSER_PROTOCOL
        || step.op != "serve"
        || step.source.as_deref() != Some(input.id.as_str())
        || !step.argv.is_empty()
        || !step.cwd.is_empty()
        || !step.env.is_empty()
        || step.root.is_some()
        || port.protocol != HTTP_PROTOCOL
        || port.from != step.id
        || port.guest_port.is_some()
        || surface.port != port.id
        || surface.artifact_ref != input.content_ref
        || surface.entry != step.entry.as_deref().unwrap_or_default()
        || surface.spa_fallback != step.spa_fallback.unwrap_or(false)
    {
        return Err(profile(
            "application surface and derivation do not name one static route",
        ));
    }

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
                        "HTTP requirement does not target the static surface",
                    ));
                }
                if let Some(digest) = &requirement.body_digest {
                    parse_ref(digest, "HTTP body_digest")?;
                }
            }
            verifier => {
                return Err(profile(format!(
                    "unsupported required verifier `{verifier}`"
                )));
            }
        }
    }
    Ok(())
}

fn validate_http_paths(
    contract: &BoundContract,
    application: &ApplicationV1,
    tree: &PortableTreeV1,
) -> Result<(), PortableApplicationError> {
    let surface = &application.surfaces[0];
    for requirement in &contract.requirements {
        if requirement.verifier != HTTP_CONTRACT_VERIFIER {
            continue;
        }
        let path = requirement.path.as_deref().unwrap_or("/");
        let artifact_path = path.strip_prefix('/').unwrap_or(path);
        let artifact_path = if artifact_path.is_empty() {
            surface.entry.as_str()
        } else {
            artifact_path
        };
        if !tree.entries.iter().any(|entry| entry.path == artifact_path) {
            return Err(profile(
                "HTTP requirement does not name a file in the portable tree",
            ));
        }
        if requirement.body_digest.is_some() && artifact_path == surface.entry {
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
) -> Result<(), PortableApplicationError> {
    if tree.schema != PORTABLE_TREE_SCHEMA || tree.entries.is_empty() {
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
        let expected_media_type = media_type_for(&entry.path)
            .ok_or_else(|| profile(format!("unsupported media type for `{}`", entry.path)))?;
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
    registry.register(Arc::new(DerivationReferences))?;
    registry.register(Arc::new(TreeReferences))?;
    Ok(registry)
}

struct ContractReferences;

impl PortableReferenceExtractor for ContractReferences {
    fn schema(&self) -> &str {
        BOUND_CONTRACT_SCHEMA
    }

    fn outgoing(&self, bytes: &[u8]) -> Result<Vec<ContentRef>, PortableBundleError> {
        parse_for_extraction::<BoundContract>(BOUND_CONTRACT_SCHEMA, bytes)?;
        Ok(Vec::new())
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

fn collect_static_files(
    root: &Path,
) -> Result<Vec<(String, PathBuf, String)>, PortableApplicationError> {
    fn visit(
        root: &Path,
        current: &Path,
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
                visit(root, &path, output)?;
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
            let media_type = media_type_for(&relative)
                .ok_or_else(|| profile(format!("unsupported static file `{relative}`")))?;
            output.push((relative, path, media_type.to_owned()));
        }
        Ok(())
    }

    let mut output = Vec::new();
    visit(root, root, &mut output)?;
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

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k")
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
}
