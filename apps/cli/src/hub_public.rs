//! Public Capsule Hub client: `ato run capsules/<owner>/<name>` resolution and
//! `ato hub publish`.
//!
//! This is the public registry surface described by the Capsule Hub MVP. It is
//! deliberately separate from [`crate::hub`], which is the private, end-to-end
//! encrypted capsulehub push/pull: nothing here changes the meaning of a
//! capsulehub link, its fragment key, or the ciphertext store.
//!
//! Two orders are fixed by the contract:
//!
//! - **Resolution** for `ato run capsules/<owner>/<name>[:tag|@digest]`:
//!   `GET /v1/hub/capsules/<owner>/<name>/resolve`, then the returned
//!   `artifact_path`, then verify the served bytes against the resolved
//!   `digest` and `size_bytes` *before* anything is executed. Only then are the
//!   bytes stored under `$ATO_HOME/hub/<hex>.capsule` (0600, fsync, atomic
//!   rename) and handed to the ordinary `ato run <file.capsule>` path, which
//!   keeps its own validation, execution and receipt behaviour unchanged.
//! - **Publishing**: validate the bundle with the same local validator that
//!   admits an `ato run`, then `PUT /v1/hub/capsules/<owner>/<name>` with the
//!   base64 bundle and its metadata, authenticated by the credential `ato
//!   login` saved for that API origin.
//!
//! A resolved Run reports its receipt with `POST .../runs` only when the
//! receipt is `fully_satisfied`. Reporting is best-effort: it never changes the
//! Run's exit code, and nothing about the coordinator, the token, or the
//! receipt body is written to a log.
//!
//! The HTTP layer takes its base URL as an argument so tests can point it at a
//! loopback stub instead of the real API.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use ato_formation::native_surface::NativePresentation;
use ato_formation::verify::ContractVerificationReceipt;
use ato_objects::PortableApplicationBundle;
use ato_portable_application::portable_run::execution_requirements_for;
use ato_portable_application::{
    PortableRealizationKind, ValidatedPortableApplication, bundle_sha256,
};
use base64::Engine;
use clap::Args;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

/// Reserved first-argument prefix for a Hub reference.
pub const HUB_REF_PREFIX: &str = "capsules/";
/// The tag a reference without a selector resolves.
pub const DEFAULT_TAG: &str = "latest";
/// The API's per-bundle bound (8 MiB, RFC 4.5). Checked before downloading.
pub const MAX_BUNDLE_BYTES: u64 = 8 * 1024 * 1024;

pub const ERR_INVALID_REF: &str = "hub-invalid-ref";
pub const ERR_INVALID_DIGEST: &str = "hub-invalid-digest";
pub const ERR_INVALID_TAG: &str = "hub-invalid-tag";
pub const ERR_DIGEST_MISMATCH: &str = "hub-digest-mismatch";
pub const ERR_SIZE_MISMATCH: &str = "hub-size-mismatch";
pub const ERR_NOT_FOUND: &str = "hub-not-found";
pub const ERR_UNAUTHORIZED: &str = "hub-unauthorized";
pub const ERR_FORBIDDEN: &str = "hub-forbidden";
pub const ERR_INVALID_REQUEST: &str = "hub-invalid-request";
pub const ERR_BUNDLE_TOO_LARGE: &str = "hub-bundle-too-large";
pub const ERR_REDIRECT_REFUSED: &str = "hub-redirect-refused";
pub const ERR_INVALID_BUNDLE: &str = "hub-invalid-bundle";
pub const ERR_API_FAILED: &str = "hub-api-failed";
pub const ERR_CAPABILITIES_UNREPRESENTABLE: &str = "hub-capabilities-unrepresentable";
pub const ERR_CAPABILITY_UNAVAILABLE: &str = "hub-capability-unavailable";

const HUB_CAPSULES_PATH: &str = "/v1/hub/capsules/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const REPORT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 3;
/// The Hub capability metadata bound (RFC §3): at most eight Derivation routes.
pub const MAX_CAPABILITY_DERIVATIONS: usize = 8;
/// The Hub capability metadata bound (RFC §3): at most eight surfaces per route.
pub const MAX_CAPABILITY_SURFACES: usize = 8;
/// The API's bound on the whole `metadata.capabilities` document.
pub const MAX_CAPABILITY_DOCUMENT_BYTES: usize = 16 * 1024;
/// The API's bound on a surface's declared logical path.
pub const MAX_CAPABILITY_SURFACE_PATH_BYTES: usize = 2048;
/// The API's bound on a surface label (UTF-16 code units, like the API).
pub const MAX_CAPABILITY_SURFACE_ID_UTF16_UNITS: usize = 128;
/// The API's bound on a logical Port token.
pub const MAX_CAPABILITY_PORT_BYTES: usize = 128;
/// The API's bound on an os/arch platform value.
pub const MAX_CAPABILITY_PLATFORM_BYTES: usize = 64;
/// `metadata.capabilities.source` for metadata derived from the bundle bytes.
pub const CAPABILITIES_SOURCE_BUNDLE: &str = "bundle";

/// One error whose first token is a stable machine-readable code.
fn hub_error(code: &str, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{code}: {message}")
}

/// How a reference selects one immutable Capsule version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubSelector {
    /// A mutable tag pointer (`latest` by default).
    Tag(String),
    /// An immutable `sha256:<64 lowercase hex>` bundle digest.
    Digest(String),
}

/// A parsed `capsules/<owner>/<name>[:tag|@digest]` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubRef {
    pub owner: String,
    pub name: String,
    pub selector: HubSelector,
}

impl HubRef {
    /// Parse the argument, distinguishing "not a Hub reference" from "a
    /// malformed one".
    ///
    /// Only the reserved `capsules/` prefix is claimable. Everything else is
    /// `Ok(None)`, so an ordinary local path keeps exactly its old meaning.
    pub fn parse(value: &str) -> Result<Option<Self>> {
        let Some(rest) = value.strip_prefix(HUB_REF_PREFIX) else {
            return Ok(None);
        };
        if rest.is_empty() {
            bail!(hub_error(
                ERR_INVALID_REF,
                format!("`{value}` names no owner and no capsule")
            ));
        }
        let (owner, tail) = rest.split_once('/').ok_or_else(|| {
            hub_error(
                ERR_INVALID_REF,
                format!("`{value}` must be `{HUB_REF_PREFIX}<owner>/<name>`"),
            )
        })?;
        if !is_kebab_identifier(owner) {
            bail!(hub_error(
                ERR_INVALID_REF,
                format!(
                    "`{owner}` is not a publisher handle (lowercase letters, digits and hyphens)"
                )
            ));
        }
        let (name, selector) = split_selector(tail).map_err(|message| {
            hub_error(
                ERR_INVALID_REF,
                format!("`{value}` is not a valid Hub reference: {message}"),
            )
        })?;
        if !is_kebab_identifier(name) {
            bail!(hub_error(
                ERR_INVALID_REF,
                format!("`{name}` is not a capsule name (lowercase letters, digits and hyphens)")
            ));
        }
        Ok(Some(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
            selector,
        }))
    }

    /// The canonical `capsules/<owner>/<name>` form, without a selector.
    pub fn reference(&self) -> String {
        format!("{HUB_REF_PREFIX}{}/{}", self.owner, self.name)
    }

    /// The `resolve` query string for this reference's selector.
    pub fn query(&self) -> String {
        match &self.selector {
            HubSelector::Tag(tag) => format!("tag={tag}"),
            HubSelector::Digest(digest) => format!("digest={digest}"),
        }
    }
}

fn split_selector(tail: &str) -> Result<(&str, HubSelector), String> {
    if let Some((name, digest)) = tail.split_once('@') {
        if digest.is_empty() {
            return Err("``` must be followed by a digest".to_owned());
        }
        return Ok((name, HubSelector::Digest(parse_digest(digest)?)));
    }
    if let Some((name, tag)) = tail.split_once(':') {
        if !is_valid_tag(tag) {
            return Err(format!("`{tag}` is not a valid tag"));
        }
        return Ok((name, HubSelector::Tag(tag.to_owned())));
    }
    Ok((tail, HubSelector::Tag(DEFAULT_TAG.to_owned())))
}

/// Lowercase kebab-case identity segment: the shape of a publisher handle and
/// of a capsule slug (`^[a-z0-9]([a-z0-9-]*[a-z0-9])?$`, at most 63 chars).
fn is_kebab_identifier(value: &str) -> bool {
    if value.is_empty() || value.len() > 63 {
        return false;
    }
    let bytes = value.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    let Some(last) = bytes.last() else {
        return false;
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return false;
    }
    if !last.is_ascii_lowercase() && !last.is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

/// `^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$` — the API's tag shape (RFC 4.5).
pub fn is_valid_tag(tag: &str) -> bool {
    if tag.len() > 64 {
        return false;
    }
    let mut characters = tag.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
}

/// Parse `sha256:<64 hex>` and normalize the hex to lowercase.
pub fn parse_digest(value: &str) -> Result<String, String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(format!("`{value}` must start with `sha256:`"));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "`{value}` must be `sha256:` followed by 64 hexadecimal characters"
        ));
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

/// Parse `<owner>/<name>` as given to `ato hub publish --name`.
pub fn parse_owner_name(value: &str) -> Result<(String, String)> {
    let (owner, name) = value.split_once('/').ok_or_else(|| {
        hub_error(
            ERR_INVALID_REF,
            format!("`{value}` must be `<owner>/<name>`"),
        )
    })?;
    if !is_kebab_identifier(owner) {
        bail!(hub_error(
            ERR_INVALID_REF,
            format!("`{owner}` is not a publisher handle (lowercase letters, digits and hyphens)")
        ));
    }
    // The Store creates slugs with a three-character minimum; reject a name the
    // server could not create rather than uploading first.
    if !is_kebab_identifier(name) || name.len() < 3 {
        bail!(hub_error(
            ERR_INVALID_REF,
            format!(
                "`{name}` is not a capsule name (lowercase letters, digits and hyphens, at least 3 characters)"
            )
        ));
    }
    Ok((owner.to_owned(), name.to_owned()))
}

/// A capsule resolved from the Hub and stored locally.
#[derive(Debug, Clone)]
pub struct ResolvedHubCapsule {
    pub reference: HubRef,
    /// The immutable digest the API resolved and the bytes matched.
    pub digest: String,
    /// The API origin the capsule was resolved from (used for run reporting).
    pub api_url: String,
    /// `$ATO_HOME/hub/<hex>.capsule`, verified byte-for-byte against `digest`.
    pub bundle_path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct ResolveResponse {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub tag: Option<String>,
    pub digest: String,
    pub size_bytes: u64,
    pub artifact_path: String,
}

/// One immutable release as the API lists it.
#[derive(Debug, Deserialize)]
pub struct PublishResponse {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub tag: Option<String>,
    pub digest: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub created: Option<bool>,
}

/// The Hub capability metadata derived from a validated bundle (M2 §3).
///
/// This is evidence about the bundle bytes, never a publisher's declaration.
/// The publisher's `--surface` and `--environment` values stay in the separate
/// declared fields of [`PublishMetadata`] and never overwrite these facts.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HubCapabilities {
    /// Always [`CAPABILITIES_SOURCE_BUNDLE`].
    pub source: &'static str,
    pub contract_ref: String,
    pub derivations: Vec<HubDerivationCapability>,
}

/// One Derivation's execution capability, in the Hub's five-value vocabulary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HubDerivationCapability {
    pub derivation_ref: String,
    /// `static_web`, `local_process`, `oci`, `native_terminal` or
    /// `native_pixel`.
    pub realization: &'static str,
    /// The OCI platform the route's images are pinned to, when it declares
    /// one.
    pub platform: Option<String>,
    /// The `os/arch` a native desktop artifact was built for, when declared.
    pub native_platform: Option<String>,
    pub requires: HubCapabilityRequires,
    pub surfaces: Vec<HubCapabilitySurface>,
}

/// What the selected route needs from a host. No path, endpoint or runtime
/// selection is part of it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HubCapabilityRequires {
    pub container: bool,
    pub state: bool,
}

/// One logical Surface as the Hub registry describes it.
///
/// `kind` is one of `http`, `static_web`, `native_terminal` or
/// `native_pixel`; `path` is a declared logical path, never a host
/// filesystem path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HubCapabilitySurface {
    pub id: String,
    pub kind: &'static str,
    pub port: String,
    pub path: String,
}

/// Project only the declared facts of one validated route into Hub vocabulary.
fn hub_capability_projection(
    validated: &ValidatedPortableApplication,
) -> (&'static str, &'static str) {
    if let Some(native) = &validated.derivation.native_surface {
        return match native.surface {
            NativePresentation::Terminal { .. } => ("native_terminal", "native_terminal"),
            NativePresentation::PixelStream { .. } => ("native_pixel", "native_pixel"),
        };
    }
    match validated.realization {
        PortableRealizationKind::StaticWeb => ("static_web", "static_web"),
        PortableRealizationKind::LocalProcess | PortableRealizationKind::ProcessServiceGroup => {
            ("local_process", "http")
        }
        PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup => {
            ("oci", "http")
        }
        // `ato.native@1` is one desktop artifact: its presentation is the
        // app's own window, and it declares no terminal/pixel adapter. The
        // platform it was built for is carried by `native_platform`.
        PortableRealizationKind::NativeDesktop => ("native_pixel", "native_pixel"),
    }
}

/// Derive `metadata.capabilities` from every validated Derivation.
///
/// `validated` is the output of the same `validate_bundle_for_derivation` path
/// that admits a local Run (`validate_formed_bundle_document` runs it for each
/// declared D), so no publisher-supplied value enters this projection.
/// Host-owned values (`ExecutionRequirements::bound_paths`, endpoints, PIDs and
/// selected runtimes) are never serialized.
pub fn derive_capabilities(
    bundle: &PortableApplicationBundle,
    validated: &[ValidatedPortableApplication],
) -> Result<HubCapabilities> {
    if validated.is_empty() {
        bail!(hub_error(
            ERR_CAPABILITIES_UNREPRESENTABLE,
            "the bundle declares no derivation"
        ));
    }
    ensure!(
        validated.len() <= MAX_CAPABILITY_DERIVATIONS,
        hub_error(
            ERR_CAPABILITIES_UNREPRESENTABLE,
            format!(
                "the bundle declares {} derivations; Hub capability metadata carries at most {MAX_CAPABILITY_DERIVATIONS}",
                validated.len()
            )
        )
    );
    ensure!(
        validated.len() == bundle.index.derivations.len(),
        hub_error(
            ERR_CAPABILITIES_UNREPRESENTABLE,
            format!(
                "{} derivations were validated but the bundle index declares {}",
                validated.len(),
                bundle.index.derivations.len()
            )
        )
    );
    let mut contract_ref: Option<String> = None;
    let mut derivations = Vec::with_capacity(validated.len());
    // Emit in the bundle's canonical (sorted) Derivation order so the same
    // bytes always produce the same metadata.
    for declared in &bundle.index.derivations {
        let route = validated
            .iter()
            .find(|route| route.derivation_ref.as_str() == declared)
            .ok_or_else(|| {
                hub_error(
                    ERR_CAPABILITIES_UNREPRESENTABLE,
                    format!("the bundle index declares `{declared}` but it was not validated"),
                )
            })?;
        let canonical_contract_ref = route.contract_ref.to_string();
        match &contract_ref {
            Some(expected) if expected != &canonical_contract_ref => {
                bail!(hub_error(
                    ERR_CAPABILITIES_UNREPRESENTABLE,
                    "validated derivations disagree on the ContractRef"
                ))
            }
            _ => contract_ref = Some(canonical_contract_ref),
        }
        let requirements = execution_requirements_for(route, Path::new(""));
        let (realization, surface_kind) = hub_capability_projection(route);
        for (label, value) in [
            ("platform", requirements.platform.as_deref()),
            ("native_platform", requirements.native_platform.as_deref()),
        ] {
            if let Some(value) = value
                && !is_capability_platform(value)
            {
                bail!(hub_error(
                    ERR_CAPABILITIES_UNREPRESENTABLE,
                    format!(
                        "`{declared}` declares {label} `{value}`, which is not a lowercase os/arch"
                    )
                ));
            }
        }
        let mut surfaces = Vec::with_capacity(route.application.surfaces.len());
        for surface in &route.application.surfaces {
            if !is_capability_surface_id(&surface.id) {
                bail!(hub_error(
                    ERR_CAPABILITIES_UNREPRESENTABLE,
                    format!(
                        "`{declared}` declares the surface label `{}`, which Hub metadata cannot carry",
                        surface.id
                    )
                ));
            }
            if !is_capability_port_token(&surface.port) {
                bail!(hub_error(
                    ERR_CAPABILITIES_UNREPRESENTABLE,
                    format!(
                        "`{declared}` declares the surface port `{}`, which Hub metadata cannot carry",
                        surface.port
                    )
                ));
            }
            if !is_capability_surface_path(&surface.path) {
                bail!(hub_error(
                    ERR_CAPABILITIES_UNREPRESENTABLE,
                    format!(
                        "`{declared}` declares the surface path `{}`, which Hub metadata cannot carry",
                        surface.path
                    )
                ));
            }
            surfaces.push(HubCapabilitySurface {
                id: surface.id.clone(),
                kind: surface_kind,
                port: surface.port.clone(),
                path: surface.path.clone(),
            });
        }
        if surfaces.is_empty() {
            bail!(hub_error(
                ERR_CAPABILITIES_UNREPRESENTABLE,
                format!("`{declared}` declares no application surface")
            ));
        }
        ensure!(
            surfaces.len() <= MAX_CAPABILITY_SURFACES,
            hub_error(
                ERR_CAPABILITIES_UNREPRESENTABLE,
                format!(
                    "`{declared}` declares {} surfaces; Hub capability metadata carries at most {MAX_CAPABILITY_SURFACES}",
                    surfaces.len()
                )
            )
        );
        derivations.push(HubDerivationCapability {
            derivation_ref: declared.clone(),
            realization,
            platform: requirements.platform,
            native_platform: requirements.native_platform,
            requires: HubCapabilityRequires {
                container: requirements.needs_container,
                state: requirements.needs_state,
            },
            surfaces,
        });
    }
    let capabilities = HubCapabilities {
        source: CAPABILITIES_SOURCE_BUNDLE,
        contract_ref: contract_ref.expect("a non-empty validated route list"),
        derivations,
    };
    // The API rejects a document over its bound, so fail here with the reason
    // rather than after uploading the bundle.
    let encoded = serde_json::to_vec(&capabilities).map_err(|error| {
        hub_error(
            ERR_CAPABILITIES_UNREPRESENTABLE,
            format!("the derived Hub capabilities cannot be encoded: {error}"),
        )
    })?;
    ensure!(
        encoded.len() <= MAX_CAPABILITY_DOCUMENT_BYTES,
        hub_error(
            ERR_CAPABILITIES_UNREPRESENTABLE,
            format!(
                "the derived capabilities are {} bytes; Hub metadata carries at most {MAX_CAPABILITY_DOCUMENT_BYTES}",
                encoded.len()
            )
        )
    );
    Ok(capabilities)
}

/// A control character the API refuses: C0 controls except tab and newline,
/// plus DEL. Mirrors the API's metadata rule exactly.
fn is_capability_forbidden_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\t' | '\n')
}

/// A surface label: 1–128 UTF-16 code units, without refused controls.
///
/// The bundle validator does not constrain an Application surface's label
/// shape, and the API accepts it as a label rather than a token, so only the
/// API's code-unit bound and control rule apply.
fn is_capability_surface_id(value: &str) -> bool {
    let units = value.encode_utf16().count();
    (1..=MAX_CAPABILITY_SURFACE_ID_UTF16_UNITS).contains(&units)
        && !value.chars().any(is_capability_forbidden_control)
}

/// `[A-Za-z0-9._:-]{1,128}`: the API's logical Port token shape and bound.
fn is_capability_port_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CAPABILITY_PORT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

/// A declared logical path: absolute, ≤2048 UTF-8 bytes, no refused controls.
fn is_capability_surface_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= MAX_CAPABILITY_SURFACE_PATH_BYTES
        && !value.chars().any(is_capability_forbidden_control)
}

/// A lowercase `os/arch` pair, ≤64 UTF-8 bytes, without refused controls.
fn is_capability_platform(value: &str) -> bool {
    let Some((os, arch)) = value.split_once('/') else {
        return false;
    };
    !os.is_empty()
        && !arch.is_empty()
        && !arch.contains('/')
        && value.len() <= MAX_CAPABILITY_PLATFORM_BYTES
        && value == value.to_lowercase()
        && !value.chars().any(is_capability_forbidden_control)
}

#[derive(Debug, Serialize)]
pub struct PublishMetadata {
    pub display_name: Option<String>,
    pub summary: Option<String>,
    pub readme_markdown: Option<String>,
    pub license_spdx: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub porter: Option<String>,
    pub surfaces: Vec<String>,
    pub tags: Vec<String>,
    pub environments: Vec<String>,
    /// Capabilities derived from the validated bundle; never the declared
    /// `--surface` / `--environment` values.
    pub capabilities: HubCapabilities,
}

#[derive(Debug, Serialize)]
pub struct PublishRequest {
    pub tag: String,
    pub digest: String,
    pub bundle_base64: String,
    pub metadata: PublishMetadata,
}

/// `ato hub publish <file.capsule> --name <owner>/<name>`.
#[derive(Debug, Args)]
pub struct PublishArgs {
    /// Formed `.capsule` bundle to publish.
    pub capsule: PathBuf,
    /// Registry identity: `<owner>/<name>`.
    #[arg(long)]
    pub name: String,
    /// Release tag the new version becomes. Defaults to `latest`.
    #[arg(long, default_value = DEFAULT_TAG)]
    pub tag: String,
    /// SPDX license identifier (for example `MPL-2.0`).
    #[arg(long)]
    pub license: Option<String>,
    /// README markdown file; `-` reads standard input.
    #[arg(long)]
    pub readme: Option<PathBuf>,
    /// Source repository URL (http/https).
    #[arg(long)]
    pub repository: Option<String>,
    /// Project homepage URL (http/https).
    #[arg(long)]
    pub homepage: Option<String>,
    /// Declared surface (repeatable, e.g. `web_ui`).
    #[arg(long = "surface")]
    pub surfaces: Vec<String>,
    /// Supported environment (repeatable, e.g. `macos-arm64`).
    #[arg(long = "environment")]
    pub environments: Vec<String>,
    /// One-line plain-text summary.
    #[arg(long)]
    pub summary: Option<String>,
    /// Human-readable display name.
    #[arg(long)]
    pub display_name: Option<String>,
    /// Free-form porter credit.
    #[arg(long)]
    pub porter: Option<String>,
    /// Ato API origin. Same origin as `ato login --api-url`.
    #[arg(long, env = "ATO_API_URL", default_value = crate::auth_device::DEFAULT_API_URL)]
    pub api_url: String,
    /// Bearer token. Falls back to `ATO_API_TOKEN`, then to the credential
    /// saved by `ato login` for this API origin.
    #[arg(long, env = "ATO_API_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
}

/// Resolve a Hub reference for `ato run`, if the argument is one.
///
/// `capsules/` is a reserved prefix, but a path that actually exists keeps its
/// old local meaning: `ato run` of a real file must never go to the network.
pub fn resolve_for_run(
    path: &Path,
    api_url: &str,
    ato_home: &Path,
) -> Result<Option<ResolvedHubCapsule>> {
    let Some(value) = path.to_str() else {
        return Ok(None);
    };
    let Some(reference) = hub_reference_for_run(value, path.exists())? else {
        return Ok(None);
    };

    let base = api_base(api_url)?;
    let client = api_client()?;
    let resolved = resolve_remote(&client, &base, &reference)?;
    if resolved.reference != reference.reference() {
        bail!(hub_error(
            ERR_API_FAILED,
            format!(
                "the API resolved `{}` for {}",
                resolved.reference,
                reference.reference()
            )
        ));
    }
    if let (HubSelector::Tag(requested), Some(served)) =
        (&reference.selector, resolved.tag.as_deref())
        && requested != served
    {
        bail!(hub_error(
            ERR_API_FAILED,
            format!("the API resolved tag `{served}` for `{requested}`")
        ));
    }
    let digest = parse_digest(&resolved.digest).map_err(|message| {
        hub_error(
            ERR_INVALID_DIGEST,
            format!(
                "the API resolved {} to `{}`: {message}",
                reference.reference(),
                resolved.digest
            ),
        )
    })?;
    if resolved.size_bytes > MAX_BUNDLE_BYTES {
        bail!(hub_error(
            ERR_BUNDLE_TOO_LARGE,
            format!(
                "{} is {} bytes; a Hub bundle carries at most {MAX_BUNDLE_BYTES} bytes",
                reference.reference(),
                resolved.size_bytes
            )
        ));
    }
    let download_client = artifact_client(&base)?;
    let bytes = fetch_artifact(
        &download_client,
        &base,
        &resolved.artifact_path,
        &digest,
        resolved.size_bytes,
    )
    .with_context(|| format!("failed to fetch {}", reference.reference()))?;
    let bundle_path = store_bundle(ato_home, &digest, &bytes)?;
    Ok(Some(ResolvedHubCapsule {
        reference,
        digest,
        api_url: base,
        bundle_path,
    }))
}

/// Decide whether a `ato run` argument is a Hub reference.
///
/// The reserved prefix loses to a path that exists: a local file keeps its old
/// meaning, and no request is made for it.
fn hub_reference_for_run(value: &str, exists: bool) -> Result<Option<HubRef>> {
    if !value.starts_with(HUB_REF_PREFIX) || exists {
        // An existing path keeps its local meaning even when its name would be
        // a malformed reference (for example `capsules/local/x.capsule`).
        return Ok(None);
    }
    HubRef::parse(value)
}

fn resolve_remote(client: &Client, base: &str, reference: &HubRef) -> Result<ResolveResponse> {
    let url = format!(
        "{base}{HUB_CAPSULES_PATH}{}/{}/resolve?{}",
        reference.owner,
        reference.name,
        reference.query()
    );
    let response = client
        .get(&url)
        .header(ACCEPT, "application/json")
        .send()
        .with_context(|| format!("failed to reach the Ato API at {base}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_failure(
            "resolve",
            &reference.reference(),
            base,
            status,
            &body,
        ));
    }
    serde_json::from_str(&body).map_err(|error| {
        hub_error(
            ERR_API_FAILED,
            format!(
                "the resolve response for {} is malformed: {error}",
                reference.reference()
            ),
        )
    })
}

/// Download the artifact named by `artifact_path` and verify it.
///
/// Nothing is written until both the byte count and the SHA-256 match the
/// resolved version: a mismatch means the Run must not happen.
fn fetch_artifact(
    client: &Client,
    base: &str,
    artifact_path: &str,
    expected_digest: &str,
    expected_size: u64,
) -> Result<Vec<u8>> {
    let path = origin_relative_path(artifact_path)?;
    let url = format!("{base}{path}");
    let response = client
        .get(&url)
        .header(ACCEPT, "application/octet-stream")
        .send()
        .with_context(|| format!("failed to reach the Ato API at {base}"))?;
    let status = response.status();
    if status.is_redirection() {
        bail!(hub_error(
            ERR_REDIRECT_REFUSED,
            format!("the artifact request was redirected ({status}) away from {base}")
        ));
    }
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(api_failure(
            "download the capsule artifact",
            "",
            base,
            status,
            &body,
        ));
    }
    let bound = expected_size.saturating_add(1);
    let mut bytes = Vec::with_capacity(expected_size.min(MAX_BUNDLE_BYTES) as usize);
    response
        .take(bound)
        .read_to_end(&mut bytes)
        .context("failed while downloading the capsule artifact")?;
    if bytes.len() as u64 != expected_size {
        bail!(hub_error(
            ERR_SIZE_MISMATCH,
            format!(
                "the artifact is {} bytes; the resolved version is {expected_size} bytes",
                bytes.len()
            )
        ));
    }
    let actual = bundle_sha256(&bytes);
    if actual != expected_digest {
        bail!(hub_error(
            ERR_DIGEST_MISMATCH,
            format!("the artifact hashes to {actual}; the resolved version is {expected_digest}")
        ));
    }
    Ok(bytes)
}

/// The API returns an origin-relative path. An absolute URL would let the API
/// redirect the download to another origin, so it is refused.
fn origin_relative_path(value: &str) -> Result<&str> {
    ensure!(
        value.starts_with('/') && !value.starts_with("//"),
        hub_error(
            ERR_API_FAILED,
            format!("the API returned an unusable artifact path `{value}`")
        )
    );
    ensure!(
        !value.contains("://"),
        hub_error(
            ERR_API_FAILED,
            format!("the API returned an absolute artifact URL `{value}`")
        )
    );
    Ok(value)
}

/// Store verified bytes under `$ATO_HOME/hub/<hex>.capsule`.
///
/// An existing file is reused only when it still hashes to `digest`; anything
/// else in the way of that name is a corrupt cache entry and is replaced. The
/// write is 0600, fsynced, and renamed into place.
pub fn store_bundle(ato_home: &Path, digest: &str, bytes: &[u8]) -> Result<PathBuf> {
    let hex = digest
        .strip_prefix("sha256:")
        .expect("digest is validated before storage");
    let directory = ato_home.join("hub");
    fs::create_dir_all(&directory)
        .with_context(|| format!("create the Hub cache directory {}", directory.display()))?;
    let path = directory.join(format!("{hex}.capsule"));
    if path.is_file() {
        let existing = fs::read(&path)
            .with_context(|| format!("read the cached Hub capsule {}", path.display()))?;
        if bundle_sha256(&existing) == digest {
            return Ok(path);
        }
    }
    let temporary = directory.join(format!(
        ".{hex}.{}.{}.new",
        std::process::id(),
        unix_now_nanos()
    ));
    let result = write_private_file(&temporary, bytes).and_then(|()| {
        fs::rename(&temporary, &path)
            .with_context(|| format!("move the Hub capsule into {}", path.display()))
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    crate::hub::sync_directory_best_effort(&directory);
    Ok(path)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create the Hub capsule {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write the Hub capsule {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flush the Hub capsule {}", path.display()))
}

fn unix_now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// The API origin, validated the same way `ato login` validates its URL.
fn api_base(api_url: &str) -> Result<String> {
    let url = reqwest::Url::parse(api_url).context("invalid Ato API URL")?;
    ensure!(
        url.scheme() == "https"
            || matches!(
                url.host_str(),
                Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
            ),
        hub_error(
            ERR_API_FAILED,
            format!(
                "the Ato API URL `{api_url}` must use HTTPS (plain HTTP is allowed only for a loopback host)"
            )
        )
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        hub_error(
            ERR_API_FAILED,
            format!("the Ato API URL `{api_url}` must not carry a query or fragment")
        )
    );
    Ok(api_url.trim_end_matches('/').to_owned())
}

fn api_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("ato/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build the Ato API HTTP client")
}

/// An artifact download may be redirected, but only within the API origin.
fn artifact_client(base: &str) -> Result<Client> {
    let origin = reqwest::Url::parse(base)
        .context("invalid Ato API URL")?
        .origin()
        .ascii_serialization();
    Client::builder()
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.url().origin().ascii_serialization() != origin {
                return attempt.stop();
            }
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            attempt.follow()
        }))
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("ato/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build the Ato API HTTP client")
}

/// Map a non-success API response onto a coded error.
fn api_failure(
    action: &str,
    target: &str,
    base: &str,
    status: StatusCode,
    body: &str,
) -> anyhow::Error {
    api_failure_code(action, target, base, status, &api_error_code(body))
}

/// The `error` member of an API error body, bounded and never echoed blindly.
pub(crate) fn api_error_code(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(error) = value.get("error").and_then(|error| error.as_str()) else {
        return String::new();
    };
    error
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect()
}

/// Publish one formed bundle: `PUT /v1/hub/capsules/<owner>/<name>`.
pub fn publish(args: PublishArgs) -> Result<()> {
    publish_base64(args)
}

/// What both publish lanes need before they touch the network.
struct PreparedPublish {
    owner: String,
    name: String,
    tag: String,
    base: String,
    token: String,
    readme: Option<String>,
}

fn prepare_publish(args: &PublishArgs) -> Result<PreparedPublish> {
    let (owner, name) = parse_owner_name(&args.name)?;
    if !is_valid_tag(&args.tag) {
        bail!(hub_error(
            ERR_INVALID_TAG,
            format!("`{}` is not a valid tag", args.tag)
        ));
    }
    let readme = match args.readme.as_deref() {
        Some(path) => Some(read_readme(path)?),
        None => None,
    };
    Ok(PreparedPublish {
        owner,
        name,
        tag: args.tag.clone(),
        base: api_base(&args.api_url)?,
        token: crate::auth_device::resolve_token(args.token.clone(), &args.api_url)?,
        readme,
    })
}

/// The metadata both lanes send: the publisher's declared fields, the README,
/// and capabilities derived from the validated bundle (never a declaration).
fn publish_metadata(
    args: &PublishArgs,
    readme: Option<&str>,
    capabilities: HubCapabilities,
) -> PublishMetadata {
    PublishMetadata {
        display_name: args.display_name.clone(),
        summary: args.summary.clone(),
        readme_markdown: readme.map(str::to_owned),
        license_spdx: args.license.clone(),
        homepage: args.homepage.clone(),
        repository: args.repository.clone(),
        porter: args.porter.clone(),
        surfaces: args.surfaces.clone(),
        tags: Vec::new(),
        environments: args.environments.clone(),
        capabilities,
    }
}

/// Refuse to print a success line for different bytes or a different identity
/// than the ones just sent, in either lane.
fn finish_publish(
    prepared: &PreparedPublish,
    digest: &str,
    size: u64,
    response: PublishResponse,
) -> Result<()> {
    let expected_reference = format!("{HUB_REF_PREFIX}{}/{}", prepared.owner, prepared.name);
    if response.reference != expected_reference {
        bail!(hub_error(
            ERR_API_FAILED,
            format!(
                "the API published `{}` instead of `{expected_reference}`",
                response.reference
            )
        ));
    }
    let published = parse_digest(&response.digest).map_err(|message| {
        hub_error(
            ERR_API_FAILED,
            format!("the API published `{}`: {message}", response.digest),
        )
    })?;
    if published != digest {
        bail!(hub_error(
            ERR_DIGEST_MISMATCH,
            format!("the API stored {published}; the uploaded bundle hashes to {digest}")
        ));
    }
    if response.size_bytes != size {
        bail!(hub_error(
            ERR_SIZE_MISMATCH,
            format!(
                "the API stored {} bytes; the uploaded bundle is {size} bytes",
                response.size_bytes
            )
        ));
    }
    if let Some(served_tag) = response.tag.as_deref()
        && served_tag != prepared.tag
    {
        bail!(hub_error(
            ERR_API_FAILED,
            format!(
                "the API published tag `{served_tag}` instead of `{}`",
                prepared.tag
            )
        ));
    }
    println!("{expected_reference}@{published}");
    if let Some(url) = response.url.as_deref() {
        println!("{url}");
    }
    if response.created == Some(false) {
        eprintln!(
            "the same bytes were already published; the `{}` tag now points at {published}",
            prepared.tag
        );
    }
    Ok(())
}

/// The unchanged Base64 JSON lane: PUT /v1/hub/capsules/<owner>/<name>.
fn publish_base64(args: PublishArgs) -> Result<()> {
    let bytes = fs::read(&args.capsule)
        .with_context(|| format!("cannot read {}", args.capsule.display()))?;
    if bytes.len() as u64 > MAX_BUNDLE_BYTES {
        bail!(hub_error(
            ERR_BUNDLE_TOO_LARGE,
            format!(
                "{} is {} bytes; a Base64 JSON publish carries at most {MAX_BUNDLE_BYTES} bytes",
                args.capsule.display(),
                bytes.len()
            )
        ));
    }
    // Only a bundle the local Run path would accept is ever published, and
    // the Hub capability metadata is derived from exactly those validated
    // routes rather than from any publisher-declared value.
    let formed = crate::hub::validate_formed_bundle_document(&bytes)
        .with_context(|| format!("{} must not be published", args.capsule.display()))?;
    let capabilities =
        derive_capabilities(&formed.bundle, &formed.derivations).with_context(|| {
            format!(
                "the capabilities of {} cannot be represented in Hub metadata",
                args.capsule.display()
            )
        })?;
    let prepared = prepare_publish(&args)?;
    let metadata = publish_metadata(&args, prepared.readme.as_deref(), capabilities);
    let digest = bundle_sha256(&bytes);
    let request = PublishRequest {
        tag: prepared.tag.clone(),
        digest: digest.clone(),
        bundle_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        metadata,
    };
    let client = api_client()?;
    let response = put_publish(
        &client,
        &prepared.base,
        &prepared.token,
        &prepared.owner,
        &prepared.name,
        &request,
    )?;
    finish_publish(&prepared, &digest, bytes.len() as u64, response)
}

fn api_failure_code(
    action: &str,
    target: &str,
    base: &str,
    status: StatusCode,
    detail: &str,
) -> anyhow::Error {
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    };
    let subject = if target.is_empty() {
        String::new()
    } else {
        format!(" for {target}")
    };
    match status.as_u16() {
        300..=399 => hub_error(
            ERR_REDIRECT_REFUSED,
            format!("the API redirected the request to {action}{subject} ({status})"),
        ),
        401 => hub_error(
            ERR_UNAUTHORIZED,
            format!(
                "the API at {base} refused to {action}{subject}{suffix}; run ato login --api-url {base} first"
            ),
        ),
        403 => hub_error(
            ERR_FORBIDDEN,
            format!(
                "the API refused to {action}{subject}{suffix}; this account does not own that namespace"
            ),
        ),
        404 => hub_error(
            ERR_NOT_FOUND,
            format!("the Hub at {base} has no such capsule or version{subject}{suffix}"),
        ),
        410 => hub_error(
            ERR_API_FAILED,
            format!("the API expired the request to {action}{subject}{suffix}"),
        ),
        409 => hub_error(
            ERR_DIGEST_MISMATCH,
            format!("the API reported a conflict while trying to {action}{subject}{suffix}"),
        ),
        413 => hub_error(
            ERR_BUNDLE_TOO_LARGE,
            format!("the API refused the bundle as too large{subject}{suffix}"),
        ),
        422 => hub_error(
            ERR_INVALID_BUNDLE,
            format!("the API refused the bundle as unreadable{subject}{suffix}"),
        ),
        400 => hub_error(
            ERR_INVALID_REQUEST,
            format!("the API rejected the request to {action}{subject}{suffix}"),
        ),
        _ => hub_error(
            ERR_API_FAILED,
            format!("failed to {action}{subject}: HTTP {status}{suffix}"),
        ),
    }
}
fn read_readme(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut text = String::new();
        io::stdin()
            .read_to_string(&mut text)
            .context("read the README from standard input")?;
        return Ok(text);
    }
    fs::read_to_string(path).with_context(|| format!("read the README {}", path.display()))
}

fn put_publish(
    client: &Client,
    base: &str,
    token: &str,
    owner: &str,
    name: &str,
    request: &PublishRequest,
) -> Result<PublishResponse> {
    let url = format!("{base}{HUB_CAPSULES_PATH}{owner}/{name}");
    let response = client
        .put(&url)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(CONTENT_TYPE, "application/json")
        .json(request)
        .send()
        .with_context(|| format!("failed to reach the Ato API at {base}"))?;
    let status = response.status();
    let body = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_failure(
            "publish",
            &format!("{HUB_REF_PREFIX}{owner}/{name}"),
            base,
            status,
            &body,
        ));
    }
    serde_json::from_str(&body).map_err(|error| {
        hub_error(
            ERR_API_FAILED,
            format!("the publish response is malformed: {error}"),
        )
    })
}

/// Report a resolved Run's receipt. Best-effort: the caller's exit code is the
/// Run's, and a failure is a warning only.
pub fn report_successful_run(resolved: &ResolvedHubCapsule, receipt: &ContractVerificationReceipt) {
    if let Err(error) = post_run(resolved, receipt) {
        eprintln!(
            "warning: the Capsule Hub did not record this Run (the Run itself is unaffected): {error:#}"
        );
    }
}

fn post_run(resolved: &ResolvedHubCapsule, receipt: &ContractVerificationReceipt) -> Result<()> {
    let body = run_report_body(resolved, receipt)?;
    let base = api_base(&resolved.api_url)?;
    let url = format!(
        "{base}{HUB_CAPSULES_PATH}{}/{}/runs",
        resolved.reference.owner, resolved.reference.name
    );
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REPORT_TIMEOUT)
        .user_agent(concat!("ato/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build the Ato API HTTP client")?;
    let response = client
        .post(&url)
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .with_context(|| format!("failed to reach the Ato API at {base}"))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    if !status.is_success() {
        return Err(api_failure(
            "record the Run",
            &resolved.reference.reference(),
            &base,
            status,
            &text,
        ));
    }
    let counted = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value.get("counted").and_then(|counted| counted.as_bool()))
        .unwrap_or(false);
    if counted {
        println!(
            "Capsule Hub: run recorded for {}",
            resolved.reference.reference()
        );
    }
    Ok(())
}

/// The request body for `POST .../runs`, refusing to report evidence the API
/// would reject anyway so a mis-attributed Run is never counted.
fn run_report_body(
    resolved: &ResolvedHubCapsule,
    receipt: &ContractVerificationReceipt,
) -> Result<serde_json::Value> {
    if !receipt.fully_satisfied {
        bail!("the verification receipt is not fully satisfied");
    }
    if receipt.bundle_sha256.as_deref() != Some(resolved.digest.as_str()) {
        bail!(
            "the receipt names bundle {} but the resolved capsule is {}",
            receipt.bundle_sha256.as_deref().unwrap_or("none"),
            resolved.digest
        );
    }
    let attempt_id = receipt
        .execution
        .as_ref()
        .and_then(|execution| execution.attempt_id.as_deref())
        .context("the verification receipt carries no execution.attempt_id")?;
    if !is_reportable_attempt_id(attempt_id) {
        bail!("the verification receipt attempt_id `{attempt_id}` is not reportable");
    }
    Ok(serde_json::json!({
        "digest": resolved.digest,
        "receipt": receipt,
    }))
}

/// `^[A-Za-z0-9._:-]{1,128}$` — the API's attempt-id shape (RFC 4.6).
fn is_reportable_attempt_id(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};

    use ato_formation::verify::{
        VerificationExecutionEvidence, VerificationNativeEvidence, VerificationTarget,
        VerificationTargetKind,
    };
    use base64::Engine;

    use super::*;

    /// The committed interop sample's bundle digest.
    const SAMPLE_DIGEST: &str =
        "sha256:95b837f4e8ed3c3354a4560e56820030cdb2ba34ba2a7c65aa6d26e139bdd813";

    fn sample_bundle() -> Vec<u8> {
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule"))
            .expect("the interop-static fixture is committed")
    }

    // ---- reference parsing ------------------------------------------------

    #[test]
    fn a_capsule_argument_parses_with_the_default_tag() {
        let reference = HubRef::parse("capsules/koh0920/interop-static")
            .unwrap()
            .expect("a capsules/ argument is a Hub reference");
        assert_eq!(reference.owner, "koh0920");
        assert_eq!(reference.name, "interop-static");
        assert_eq!(reference.selector, HubSelector::Tag(DEFAULT_TAG.to_owned()));
        assert_eq!(reference.reference(), "capsules/koh0920/interop-static");
        assert_eq!(reference.query(), "tag=latest");
    }

    #[test]
    fn a_reference_selects_a_tag_or_a_pinned_digest() {
        let tagged = HubRef::parse("capsules/koh0920/interop-static:v1.2.3")
            .unwrap()
            .unwrap();
        assert_eq!(tagged.selector, HubSelector::Tag("v1.2.3".to_owned()));
        assert_eq!(tagged.query(), "tag=v1.2.3");

        let pinned = HubRef::parse(&format!("capsules/koh0920/interop-static@{SAMPLE_DIGEST}"))
            .unwrap()
            .unwrap();
        assert_eq!(
            pinned.selector,
            HubSelector::Digest(SAMPLE_DIGEST.to_owned())
        );
        assert_eq!(pinned.query(), format!("digest={SAMPLE_DIGEST}"));
    }

    #[test]
    fn an_ordinary_path_is_not_a_hub_reference() {
        for value in [
            "./x.capsule",
            "samples/interop-static.capsule",
            "/abs/interop-static.capsule",
            "capsules.capsule",
            "Capsules/koh0920/interop-static",
        ] {
            assert_eq!(HubRef::parse(value).unwrap(), None, "{value}");
        }
    }

    #[test]
    fn malformed_hub_references_are_named_errors() {
        for value in [
            "capsules/",
            "capsules/koh0920",
            "capsules//interop-static",
            "capsules/koh0920/",
            "capsules/Koh0920/interop-static",
            "capsules/koh0920/Interop-static",
            "capsules/koh0920/interop_static",
            "capsules/koh0920/interop-static:",
            "capsules/koh0920/interop-static:bad tag",
            "capsules/koh0920/interop-static@",
            "capsules/koh0920/interop-static@sha256:abc",
            "capsules/koh0920/interop-static@md5:0000",
        ] {
            let error = HubRef::parse(value).expect_err(value);
            assert!(
                error.to_string().starts_with(ERR_INVALID_REF),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn digests_are_normalized_and_validated() {
        assert_eq!(parse_digest(SAMPLE_DIGEST).unwrap(), SAMPLE_DIGEST);
        let upper = format!("sha256:{}", "A".repeat(64));
        assert_eq!(
            parse_digest(&upper).unwrap(),
            format!("sha256:{}", "a".repeat(64))
        );
        for value in [
            String::new(),
            "sha256:".to_owned(),
            "sha256:xyz".to_owned(),
            format!("sha256:{}", "g".repeat(64)),
            format!("sha512:{}", "a".repeat(64)),
            "a".repeat(64),
        ] {
            assert!(parse_digest(&value).is_err(), "{value}");
        }
    }

    #[test]
    fn tags_follow_the_api_shape() {
        for tag in ["latest", "v1.2.3", "a", "A_b-9", "0"] {
            assert!(is_valid_tag(tag), "{tag}");
        }
        for tag in [
            String::new(),
            "-latest".to_owned(),
            "lat:est".to_owned(),
            "lat/est".to_owned(),
            "lat est".to_owned(),
            "@latest".to_owned(),
            "a".repeat(65),
        ] {
            assert!(!is_valid_tag(&tag), "{tag}");
        }
    }

    #[test]
    fn publish_names_are_owner_and_name() {
        assert_eq!(
            parse_owner_name("koh0920/interop-static").unwrap(),
            ("koh0920".to_owned(), "interop-static".to_owned())
        );
        for value in [
            "koh0920",
            "koh0920/ab",
            "Koh0920/interop-static",
            "koh0920/interop_static",
            "/interop-static",
            "koh0920/",
        ] {
            let error = parse_owner_name(value).expect_err(value);
            assert!(
                error.to_string().starts_with(ERR_INVALID_REF),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn an_existing_local_path_wins_over_the_reserved_prefix() {
        assert_eq!(
            hub_reference_for_run("capsules/koh0920/interop-static", true).unwrap(),
            None
        );
        assert!(
            hub_reference_for_run("capsules/koh0920/interop-static", false)
                .unwrap()
                .is_some()
        );
        // A local file whose name would not even be a valid reference is still
        // local: existence is decided before the reference is parsed.
        assert_eq!(
            hub_reference_for_run("capsules/local/x.capsule", true).unwrap(),
            None
        );
        assert!(
            hub_reference_for_run("capsules/local/x.capsule", false).is_err(),
            "a malformed reference that does not exist must fail"
        );
        assert_eq!(hub_reference_for_run("./x.capsule", false).unwrap(), None);
    }

    #[test]
    fn the_api_url_must_be_https_or_loopback() {
        assert_eq!(
            api_base("https://staging.api.ato.run/").unwrap(),
            "https://staging.api.ato.run"
        );
        assert!(api_base("http://127.0.0.1:8787").is_ok());
        assert!(api_base("http://localhost:8787").is_ok());
        for value in [
            "http://api.ato.run",
            "https://api.ato.run?x=1",
            "https://api.ato.run#fragment",
            "not a url",
        ] {
            assert!(api_base(value).is_err(), "{value}");
        }
    }

    // ---- the Hub cache ----------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn the_hub_cache_file_is_private_and_reused() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let bytes = sample_bundle();
        let path = store_bundle(home.path(), SAMPLE_DIGEST, &bytes).unwrap();
        assert_eq!(
            path,
            home.path().join("hub").join(format!(
                "{}.capsule",
                SAMPLE_DIGEST.trim_start_matches("sha256:")
            ))
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode is {:o}", mode & 0o777);

        let again = store_bundle(home.path(), SAMPLE_DIGEST, &bytes).unwrap();
        assert_eq!(again, path);
    }

    #[test]
    fn a_corrupt_hub_cache_entry_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let directory = home.path().join("hub");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!(
            "{}.capsule",
            SAMPLE_DIGEST.trim_start_matches("sha256:")
        ));
        fs::write(&path, b"tampered").unwrap();

        let bytes = sample_bundle();
        let stored = store_bundle(home.path(), SAMPLE_DIGEST, &bytes).unwrap();
        assert_eq!(stored, path);
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }

    // ---- a loopback stub API ---------------------------------------------

    fn http_response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    fn json_response(status: &str, value: serde_json::Value) -> Vec<u8> {
        http_response(
            status,
            "application/json",
            &serde_json::to_vec(&value).unwrap(),
        )
    }

    fn binary_response(status: &str, body: &[u8]) -> Vec<u8> {
        http_response(status, "application/octet-stream", body)
    }

    fn redirect_response(location: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
        .into_bytes()
    }

    fn resolve_body(bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({
            "ref": "capsules/koh0920/interop-static",
            "tag": "latest",
            "digest": SAMPLE_DIGEST,
            "size_bytes": bytes.len(),
            "artifact_path": format!(
                "/v1/hub/capsules/koh0920/interop-static/artifacts/{SAMPLE_DIGEST}"
            ),
            "published_at": "2026-11-16 05:00:00",
            "verification": { "state": "experimental", "successful_runs": 0, "last_run_at": null },
        })
    }

    fn read_request(mut stream: TcpStream) -> String {
        // A socket accepted from a non-blocking listener can inherit that
        // mode; a read would then fail with WouldBlock before any byte of the
        // request has arrived.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            if std::time::Instant::now() > deadline {
                break;
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(position) =
                        buffer.windows(4).position(|window| window == b"\r\n\r\n")
                    {
                        let head =
                            String::from_utf8_lossy(&buffer[..position]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buffer.len() >= position + 4 + length {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    struct StubApi {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl StubApi {
        fn new(responses: Vec<Vec<u8>>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let handle = {
                let requests = Arc::clone(&requests);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    for response in responses {
                        let deadline = std::time::Instant::now() + Duration::from_secs(10);
                        let stream = loop {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            match listener.accept() {
                                Ok((stream, _)) => break stream,
                                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                    if std::time::Instant::now() > deadline {
                                        return;
                                    }
                                    thread::sleep(Duration::from_millis(2));
                                }
                                Err(_) => return,
                            }
                        };
                        let request = read_request(stream.try_clone().unwrap());
                        requests.lock().unwrap().push(request);
                        let mut stream = stream;
                        let _ = stream.write_all(&response);
                        let _ = stream.flush();
                    }
                })
            };
            Self {
                base,
                requests,
                stop,
                handle: Some(handle),
            }
        }

        fn wait_requests(&self, count: usize) -> Vec<String> {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let requests = self.requests.lock().unwrap().clone();
                if requests.len() >= count || std::time::Instant::now() > deadline {
                    return requests;
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl Drop for StubApi {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    // ---- resolution -------------------------------------------------------

    #[test]
    fn resolution_downloads_a_verified_artifact_and_stores_it() {
        let bytes = sample_bundle();
        let api = StubApi::new(vec![
            json_response("200 OK", resolve_body(&bytes)),
            binary_response("200 OK", &bytes),
        ]);
        let home = tempfile::tempdir().unwrap();
        let resolved = resolve_for_run(
            Path::new("capsules/koh0920/interop-static"),
            &api.base,
            home.path(),
        )
        .unwrap()
        .expect("a Hub reference resolves");

        assert_eq!(resolved.digest, SAMPLE_DIGEST);
        assert_eq!(resolved.api_url, api.base);
        assert_eq!(fs::read(&resolved.bundle_path).unwrap(), bytes);

        let requests = api.wait_requests(2);
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .starts_with("GET /v1/hub/capsules/koh0920/interop-static/resolve?tag=latest "),
            "{}",
            requests[0]
        );
        assert!(
            requests[1].starts_with(&format!(
                "GET /v1/hub/capsules/koh0920/interop-static/artifacts/{SAMPLE_DIGEST} "
            )),
            "{}",
            requests[1]
        );
    }

    #[test]
    fn a_pinned_reference_asks_the_api_by_digest() {
        let bytes = sample_bundle();
        let api = StubApi::new(vec![
            json_response("200 OK", resolve_body(&bytes)),
            binary_response("200 OK", &bytes),
        ]);
        let home = tempfile::tempdir().unwrap();
        resolve_for_run(
            Path::new(&format!("capsules/koh0920/interop-static@{SAMPLE_DIGEST}")),
            &api.base,
            home.path(),
        )
        .unwrap()
        .expect("a pinned reference resolves");
        let requests = api.wait_requests(1);
        assert!(
            requests[0].starts_with(&format!(
                "GET /v1/hub/capsules/koh0920/interop-static/resolve?digest={SAMPLE_DIGEST} "
            )),
            "{}",
            requests[0]
        );
    }

    #[test]
    fn a_tampered_artifact_is_refused_before_anything_is_stored() {
        let bytes = sample_bundle();
        let mut tampered = bytes.clone();
        tampered[0] ^= 0xff;
        let api = StubApi::new(vec![
            json_response("200 OK", resolve_body(&bytes)),
            binary_response("200 OK", &tampered),
        ]);
        let home = tempfile::tempdir().unwrap();
        let error = resolve_for_run(
            Path::new("capsules/koh0920/interop-static"),
            &api.base,
            home.path(),
        )
        .expect_err("tampered bytes must not resolve");
        let message = format!("{error:#}");
        assert!(message.contains(ERR_DIGEST_MISMATCH), "{message}");
        assert!(!home.path().join("hub").exists(), "nothing may be stored");
    }

    #[test]
    fn an_artifact_of_the_wrong_size_is_refused() {
        let bytes = sample_bundle();
        let mut body = resolve_body(&bytes);
        body["size_bytes"] = serde_json::Value::from(bytes.len() as u64 + 5);
        let api = StubApi::new(vec![
            json_response("200 OK", body),
            binary_response("200 OK", &bytes),
        ]);
        let home = tempfile::tempdir().unwrap();
        let error = resolve_for_run(
            Path::new("capsules/koh0920/interop-static"),
            &api.base,
            home.path(),
        )
        .expect_err("a size mismatch must not resolve");
        let message = format!("{error:#}");
        assert!(message.contains(ERR_SIZE_MISMATCH), "{message}");
        assert!(!home.path().join("hub").exists(), "nothing may be stored");
    }

    #[test]
    fn a_resolution_the_api_denies_names_its_coded_reason() {
        for (status, code, hint) in [
            ("401 Unauthorized", ERR_UNAUTHORIZED, Some("ato login")),
            ("403 Forbidden", ERR_FORBIDDEN, None),
            ("404 Not Found", ERR_NOT_FOUND, None),
        ] {
            let api = StubApi::new(vec![json_response(
                status,
                serde_json::json!({"error": "no"}),
            )]);
            let home = tempfile::tempdir().unwrap();
            let error = resolve_for_run(
                Path::new("capsules/koh0920/interop-static"),
                &api.base,
                home.path(),
            )
            .expect_err("a refused resolution must fail");
            let message = format!("{error:#}");
            assert!(message.contains(code), "{status}: {message}");
            if let Some(hint) = hint {
                assert!(message.contains(hint), "{status}: {message}");
            }
        }
    }

    #[test]
    fn an_artifact_the_api_denies_fails_before_storing() {
        let bytes = sample_bundle();
        let api = StubApi::new(vec![
            json_response("200 OK", resolve_body(&bytes)),
            json_response("403 Forbidden", serde_json::json!({"error": "forbidden"})),
        ]);
        let home = tempfile::tempdir().unwrap();
        let error = resolve_for_run(
            Path::new("capsules/koh0920/interop-static"),
            &api.base,
            home.path(),
        )
        .expect_err("a refused artifact must fail");
        assert!(format!("{error:#}").contains(ERR_FORBIDDEN));
    }

    #[test]
    fn an_artifact_redirect_within_the_api_origin_is_followed() {
        let bytes = sample_bundle();
        let api = StubApi::new(vec![
            json_response("200 OK", resolve_body(&bytes)),
            redirect_response("/moved-artifact"),
            binary_response("200 OK", &bytes),
        ]);
        let home = tempfile::tempdir().unwrap();
        let resolved = resolve_for_run(
            Path::new("capsules/koh0920/interop-static"),
            &api.base,
            home.path(),
        )
        .unwrap()
        .expect("a same-origin redirect resolves");
        assert_eq!(fs::read(&resolved.bundle_path).unwrap(), bytes);
        let requests = api.wait_requests(3);
        assert!(
            requests[2].starts_with("GET /moved-artifact "),
            "{}",
            requests[2]
        );
    }

    #[test]
    fn an_artifact_redirect_to_another_origin_is_refused() {
        let bytes = sample_bundle();
        let api = StubApi::new(vec![
            json_response("200 OK", resolve_body(&bytes)),
            redirect_response("https://elsewhere.invalid/capsule"),
        ]);
        let home = tempfile::tempdir().unwrap();
        let error = resolve_for_run(
            Path::new("capsules/koh0920/interop-static"),
            &api.base,
            home.path(),
        )
        .expect_err("a cross-origin redirect must not be followed");
        assert!(format!("{error:#}").contains(ERR_REDIRECT_REFUSED));
        assert!(!home.path().join("hub").exists());
    }

    // ---- run accounting ---------------------------------------------------

    fn receipt(
        digest: &str,
        fully_satisfied: bool,
        attempt_id: Option<&str>,
    ) -> ContractVerificationReceipt {
        ContractVerificationReceipt {
            schema: "ato.contract-verification-receipt/1".to_owned(),
            bundle_sha256: Some(digest.to_owned()),
            contract_ref: format!("sha256:{}", "1".repeat(64)),
            derivation_ref: format!("sha256:{}", "2".repeat(64)),
            target: VerificationTarget {
                kind: VerificationTargetKind::CliLocal,
            },
            execution: attempt_id.map(|attempt_id| VerificationExecutionEvidence {
                attempt_id: Some(attempt_id.to_owned()),
                ..Default::default()
            }),
            observations: Vec::new(),
            fully_satisfied,
        }
    }

    fn resolved_at(api_url: &str) -> ResolvedHubCapsule {
        ResolvedHubCapsule {
            reference: HubRef::parse("capsules/koh0920/interop-static")
                .unwrap()
                .unwrap(),
            digest: SAMPLE_DIGEST.to_owned(),
            api_url: api_url.to_owned(),
            bundle_path: PathBuf::from("unused.capsule"),
        }
    }

    #[test]
    fn only_a_satisfied_receipt_for_the_resolved_digest_is_reportable() {
        let resolved = resolved_at("http://127.0.0.1:9");
        assert!(
            run_report_body(&resolved, &receipt(SAMPLE_DIGEST, true, Some("attempt-1"))).is_ok()
        );
        let error = run_report_body(&resolved, &receipt(SAMPLE_DIGEST, false, Some("attempt-1")))
            .expect_err("a refused Run is not reported");
        assert!(error.to_string().contains("not fully satisfied"), "{error}");

        let other = format!("sha256:{}", "3".repeat(64));
        let error = run_report_body(&resolved, &receipt(&other, true, Some("attempt-1")))
            .expect_err("a receipt for other bytes is not reported");
        assert!(error.to_string().contains("resolved capsule"), "{error}");

        let error = run_report_body(&resolved, &receipt(SAMPLE_DIGEST, true, None))
            .expect_err("a receipt without an attempt is not reported");
        assert!(error.to_string().contains("attempt_id"), "{error}");

        let error = run_report_body(&resolved, &receipt(SAMPLE_DIGEST, true, Some("bad id")))
            .expect_err("an unshaped attempt id is not reported");
        assert!(error.to_string().contains("not reportable"), "{error}");
    }

    #[test]
    fn every_receipt_schema_is_reported_unchanged() {
        // The API accepts /1 through /5 with one common rule set; the CLI must
        // carry a receipt through by schema, not narrow it to the one this
        // build happens to produce for a local Static Web Run.
        let resolved = resolved_at("http://127.0.0.1:9");
        for (schema, native) in [
            ("ato.contract-verification-receipt/1", false),
            ("ato.contract-verification-receipt/2", false),
            ("ato.contract-verification-receipt/3", false),
            ("ato.contract-verification-receipt/4", false),
            ("ato.contract-verification-receipt/5", true),
        ] {
            let mut receipt = receipt(SAMPLE_DIGEST, true, Some("attempt-1"));
            receipt.schema = schema.to_owned();
            if native {
                receipt
                    .execution
                    .as_mut()
                    .expect("the helper builds execution evidence")
                    .native = Some(VerificationNativeEvidence {
                    artifact_ref: format!("sha256:{}", "4".repeat(64)),
                    materialized_tree_digest: format!("sha256:{}", "5".repeat(64)),
                    entry: "app/run.sh".to_owned(),
                    signature_policy: "require-valid-or-local-override".to_owned(),
                    signature_target: "executable".to_owned(),
                    verified_target: "app/run.sh".to_owned(),
                    codesign_verify: "valid".to_owned(),
                    signature_kind: "adhoc".to_owned(),
                    codesign_detail: None,
                    gatekeeper: "unavailable".to_owned(),
                    gatekeeper_detail: None,
                    local_override: false,
                    sandbox: "seatbelt".to_owned(),
                    enforced_read_only: vec!["/app".to_owned()],
                    enforced_read_write: Vec::new(),
                    enforced_inbound_ports: vec![18080],
                    process_group: None,
                    window_containment: "none".to_owned(),
                });
            }
            let body = run_report_body(&resolved, &receipt)
                .expect("a satisfied receipt is reportable whatever its schema");
            assert_eq!(body["receipt"]["schema"], schema);
            assert_eq!(body["receipt"]["fully_satisfied"], true);
            if native {
                assert_eq!(
                    body["receipt"]["execution"]["native"]["sandbox"],
                    "seatbelt"
                );
            }
        }
    }

    #[test]
    fn a_successful_run_posts_its_receipt_and_digest() {
        let api = StubApi::new(vec![json_response(
            "200 OK",
            serde_json::json!({"counted": true, "run_count": 3}),
        )]);
        let resolved = resolved_at(&api.base);
        post_run(&resolved, &receipt(SAMPLE_DIGEST, true, Some("attempt-1")))
            .expect("the run is reported");

        let requests = api.wait_requests(1);
        assert!(
            requests[0].starts_with("POST /v1/hub/capsules/koh0920/interop-static/runs "),
            "{:?}",
            requests
        );
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{}",
            requests[0]
        );
        let body: serde_json::Value =
            serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["digest"], SAMPLE_DIGEST);
        assert_eq!(body["receipt"]["fully_satisfied"], true);
        assert_eq!(
            body["receipt"]["schema"],
            "ato.contract-verification-receipt/1"
        );
        assert_eq!(body["receipt"]["execution"]["attempt_id"], "attempt-1");
    }

    #[test]
    fn a_failed_run_report_never_becomes_a_run_failure() {
        let api = StubApi::new(vec![json_response(
            "500 Internal Server Error",
            serde_json::json!({"error": "boom"}),
        )]);
        let resolved = resolved_at(&api.base);
        let error = post_run(&resolved, &receipt(SAMPLE_DIGEST, true, Some("attempt-1")))
            .expect_err("the API failure surfaces as an error");
        assert!(error.to_string().contains(ERR_API_FAILED), "{error}");
        // The public wrapper is warning-only: it must not panic or propagate.
        report_successful_run(
            &resolved_at("http://127.0.0.1:9"),
            &receipt(SAMPLE_DIGEST, true, Some("attempt-1")),
        );
    }

    // ---- capabilities derived from the bundle ------------------------------

    /// The canonical refs of the committed multi-derivation fixture.
    const MULTI_PROCESS_DERIVATION: &str =
        "sha256:5d1ec4660745130f196102c1bd81828993ab75d69130f0fdf2e1e2598fd9f3cc";
    const MULTI_STATIC_DERIVATION: &str =
        "sha256:5db5b2ba96e84aef2ff2c0e0931e5014ea2e5fb7b400eed8b9b45c40b5755633";
    const MULTI_CONTRACT: &str =
        "sha256:94d7bd2900fa01d8b000d1cf095b0f47bfcc279898c830f0542dd53ee99ffdaf";

    fn capabilities_of(bytes: &[u8]) -> HubCapabilities {
        let formed = crate::hub::validate_formed_bundle_document(bytes)
            .expect("the fixture is a valid formed bundle");
        derive_capabilities(&formed.bundle, &formed.derivations)
            .expect("every declared route is representable")
    }

    #[test]
    fn capabilities_are_derived_from_a_multi_derivation_bundle() {
        let bytes = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../samples/interop-multi-derivation.capsule"),
        )
        .expect("the multi-derivation fixture is committed");
        let capabilities = capabilities_of(&bytes);
        assert_eq!(capabilities.source, CAPABILITIES_SOURCE_BUNDLE);
        assert_eq!(capabilities.contract_ref, MULTI_CONTRACT);
        // The output is exactly the documented shape: over & requires are
        // derived, and neither a host path nor an undeclared runtime leaks in.
        assert_eq!(
            serde_json::to_value(&capabilities).unwrap(),
            serde_json::json!({
                "source": "bundle",
                "contract_ref": MULTI_CONTRACT,
                "derivations": [
                    {
                        "derivation_ref": MULTI_PROCESS_DERIVATION,
                        "realization": "local_process",
                        "platform": null,
                        "native_platform": null,
                        "requires": { "container": false, "state": false },
                        "surfaces": [
                            { "id": "main", "kind": "http", "port": "app.http", "path": "/" }
                        ]
                    },
                    {
                        "derivation_ref": MULTI_STATIC_DERIVATION,
                        "realization": "static_web",
                        "platform": null,
                        "native_platform": null,
                        "requires": { "container": false, "state": false },
                        "surfaces": [
                            { "id": "main", "kind": "static_web", "port": "app.http", "path": "/" }
                        ]
                    }
                ]
            })
        );
        let rendered = serde_json::to_string(&capabilities).unwrap();
        assert!(!rendered.contains("bound_paths"), "{rendered}");
        assert!(!rendered.contains(env!("CARGO_MANIFEST_DIR")), "{rendered}");
    }

    #[test]
    fn capabilities_name_an_oci_platform_and_container_requirement() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/oci-service-group-proof");
        let (bytes, _) = ato_portable_application::build_authored_bundle_v2(&source)
            .expect("the OCI fixture packs");
        let capabilities = capabilities_of(&bytes);
        assert_eq!(capabilities.derivations.len(), 1);
        let derivation = &capabilities.derivations[0];
        assert_eq!(derivation.realization, "oci");
        assert_eq!(derivation.platform.as_deref(), Some("linux/amd64"));
        assert_eq!(derivation.native_platform, None);
        assert!(derivation.requires.container);
        assert!(derivation.requires.state);
        assert_eq!(derivation.surfaces.len(), 1);
        assert_eq!(derivation.surfaces[0].kind, "http");
        assert_eq!(derivation.surfaces[0].path, "/");
    }

    #[test]
    fn hub_capability_tokens_match_the_api_shape() {
        for valid in ["app.http", "app_probe", "a:b", "a-b", "0"] {
            assert!(is_capability_port_token(valid), "{valid}");
        }
        for invalid in ["", "app http", "app/http", "app?x", "app\n"] {
            assert!(!is_capability_port_token(invalid), "{invalid}");
        }
        assert!(!is_capability_port_token(
            &"a".repeat(MAX_CAPABILITY_PORT_BYTES + 1)
        ));
        // A surface label is a label, not a token: spaces, tabs and newlines
        // are allowed; only the UTF-16 code-unit bound and refused controls
        // apply.
        assert!(is_capability_surface_id("main"));
        assert!(is_capability_surface_id("My Surface 1"));
        assert!(is_capability_surface_id("a\tb\nc"));
        assert!(!is_capability_surface_id(""));
        assert!(!is_capability_surface_id("a\rb"));
        assert!(!is_capability_surface_id("a\x7fb"));
        assert!(!is_capability_surface_id(
            &"a".repeat(MAX_CAPABILITY_SURFACE_ID_UTF16_UNITS + 1)
        ));
        // An astral character is one Rust `char` but two UTF-16 code units, so
        // 64 of them fill the bound and 65 overflow it.
        assert!(is_capability_surface_id(&"😀".repeat(64)));
        assert!(!is_capability_surface_id(&"😀".repeat(65)));
        assert!(is_capability_surface_path("/"));
        assert!(is_capability_surface_path("/proof.txt"));
        assert!(is_capability_surface_path("/a\nb"));
        assert!(!is_capability_surface_path("proof.txt"));
        assert!(!is_capability_surface_path("/a\rb"));
        assert!(!is_capability_surface_path("/a\x7fb"));
        assert!(!is_capability_surface_path(&format!(
            "/{}",
            "a".repeat(MAX_CAPABILITY_SURFACE_PATH_BYTES)
        )));
        // The path bound is UTF-8 bytes: 682 `あ` are 2046 bytes plus `/` =
        // 2047, while 683 are 2049 plus `/` = 2050.
        assert!(is_capability_surface_path(&format!(
            "/{}",
            "あ".repeat(682)
        )));
        assert!(!is_capability_surface_path(&format!(
            "/{}",
            "あ".repeat(683)
        )));
        assert!(is_capability_platform("linux/amd64"));
        assert!(is_capability_platform("macos/aarch64"));
        assert!(!is_capability_platform("Linux/amd64"));
        assert!(!is_capability_platform("linux/amd64/extra"));
        assert!(!is_capability_platform("amd64"));
        assert!(!is_capability_platform(&format!(
            "{}/amd64",
            "o".repeat(MAX_CAPABILITY_PLATFORM_BYTES)
        )));
        // 21 `あ` + `/` + 21 `い` are 43 code units but 129 UTF-8 bytes.
        assert!(!is_capability_platform(&format!(
            "{}/{}",
            "あ".repeat(21),
            "い".repeat(21)
        )));
    }

    #[test]
    fn a_derived_capability_document_fits_the_api_bound() {
        let bytes = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../samples/interop-multi-derivation.capsule"),
        )
        .expect("the multi-derivation fixture is committed");
        let capabilities = capabilities_of(&bytes);
        let encoded = serde_json::to_vec(&capabilities).unwrap();
        assert!(
            encoded.len() <= MAX_CAPABILITY_DOCUMENT_BYTES,
            "{} bytes",
            encoded.len()
        );
    }

    /// The authoring fixture the portable-application crate uses for a native
    /// Terminal adapter. `native_surface` rides an OCI route, so the derived
    /// capability keeps `container = true`.
    const NATIVE_SURFACE_MANIFEST: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "oci.image"
version = "docker.io/library/python@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
[[runtime]]
name = "oci.platform"
version = "linux/amd64"
[[runtime]]
name = "oci.memory_bytes"
version = "268435456"
[[runtime]]
name = "oci.cpu_limit_millis"
version = "1000"
[[runtime]]
name = "oci.pids_limit"
version = "128"
[[runtime]]
name = "oci.user"
version = "1000:1000"

[[derive.step]]
id = "serve"
use = "ato.oci@1"
op = "serve"
argv = ["python3", "pty_app.py"]
cwd = "."

[derive.native_surface]
kind = "terminal"
profile = "ato.terminal-surface.v1"
cols = 120
rows = 40
port = "app.native"
probe_port = "app.probe"
probe_path = "/result"
probe_input = "go\n"
probe_output_delimiter = "END"
probe_max_output_bytes = 4096
probe_timeout_ms = 5000

[[port]]
id = "app.native"
use = "ato.pty@1"
from = "serve"

[[port]]
id = "app.probe"
use = "ato.http@1"
from = "serve"

[[contract.require]]
id = "result"
use = "ato.contract.http@1"
port = "app.probe"
path = "/result"
expect = { status = 200, body_digest = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
"#;

    fn native_surface_source(manifest: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("tempdir");
        fs::write(directory.path().join("capsule.toml"), manifest).expect("manifest");
        fs::write(
            directory.path().join("pty_app.py"),
            "import sys\nsys.stdin.readline()\n",
        )
        .expect("app");
        directory
    }

    #[test]
    fn capabilities_split_native_terminal_and_pixel_routes() {
        let directory = native_surface_source(NATIVE_SURFACE_MANIFEST);
        let terminal = ato_portable_application::build_native_terminal_bundle(
            directory.path(),
            &directory.path().join("capsule.toml"),
            "Native Terminal",
        )
        .expect("the terminal fixture packs");
        let capabilities = capabilities_of(&terminal.0);
        let derivation = &capabilities.derivations[0];
        assert_eq!(derivation.realization, "native_terminal");
        assert_eq!(derivation.platform.as_deref(), Some("linux/amd64"));
        assert!(derivation.requires.container);
        assert!(!derivation.requires.state);
        assert_eq!(derivation.surfaces.len(), 1);
        assert_eq!(derivation.surfaces[0].id, "main");
        assert_eq!(derivation.surfaces[0].kind, "native_terminal");
        assert_eq!(derivation.surfaces[0].port, "app.native");
        assert_eq!(derivation.surfaces[0].path, "/");

        let pixel_manifest = NATIVE_SURFACE_MANIFEST
            .replace(
                "kind = \"terminal\"\nprofile = \"ato.terminal-surface.v1\"\ncols = 120\nrows = 40",
                "kind = \"pixel_stream\"\nprofile = \"ato.pixel-stream.v1\"\nwidth = 640\nheight = 480",
            )
            .replace(
                "probe_input = \"go\\n\"\nprobe_output_delimiter = \"END\"\nprobe_max_output_bytes = 4096",
                "probe_keysyms = [103]\nprobe_max_frame_bytes = 1228800",
            )
            .replace("use = \"ato.pty@1\"", "use = \"ato.tcp@1\"");
        let directory = native_surface_source(&pixel_manifest);
        let pixel = ato_portable_application::build_native_bundle(
            directory.path(),
            &directory.path().join("capsule.toml"),
            "Native Pixel",
        )
        .expect("the pixel fixture packs");
        let capabilities = capabilities_of(&pixel.0);
        let derivation = &capabilities.derivations[0];
        assert_eq!(derivation.realization, "native_pixel");
        assert_eq!(derivation.surfaces[0].kind, "native_pixel");
        assert_eq!(derivation.surfaces[0].port, "app.native");
    }

    #[test]
    fn capabilities_name_the_native_desktop_platform() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/native-desktop-multi-d");
        let (bytes, _) = ato_portable_application::build_authored_bundle_v2(&source)
            .expect("the native desktop fixture packs");
        let capabilities = capabilities_of(&bytes);
        assert_eq!(capabilities.derivations.len(), 3);
        let static_web = capabilities
            .derivations
            .iter()
            .find(|derivation| derivation.realization == "static_web")
            .expect("the fixture declares a static route");
        assert_eq!(static_web.native_platform, None);
        assert_eq!(static_web.surfaces[0].kind, "static_web");
        let mut platforms: Vec<_> = capabilities
            .derivations
            .iter()
            .filter_map(|derivation| derivation.native_platform.clone())
            .collect();
        platforms.sort();
        assert_eq!(platforms, vec!["macos/aarch64", "windows/x86_64"]);
        for derivation in &capabilities.derivations {
            if derivation.native_platform.is_some() {
                assert_eq!(derivation.realization, "native_pixel");
                assert!(!derivation.requires.container);
                assert!(!derivation.requires.state);
                assert_eq!(derivation.surfaces[0].kind, "native_pixel");
            }
        }
    }

    // ---- publishing -------------------------------------------------------
    fn publish_request(bytes: &[u8]) -> PublishRequest {
        PublishRequest {
            tag: "latest".to_owned(),
            digest: SAMPLE_DIGEST.to_owned(),
            bundle_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            metadata: PublishMetadata {
                display_name: Some("Interop Static".to_owned()),
                summary: Some("Static web proof capsule".to_owned()),
                readme_markdown: Some("# Interop Static".to_owned()),
                license_spdx: Some("MPL-2.0".to_owned()),
                homepage: None,
                repository: Some("https://github.com/ato-run/ato".to_owned()),
                porter: None,
                surfaces: vec!["web_ui".to_owned()],
                tags: Vec::new(),
                environments: vec!["macos-arm64".to_owned()],
                capabilities: HubCapabilities {
                    source: CAPABILITIES_SOURCE_BUNDLE,
                    contract_ref: format!("sha256:{}", "c".repeat(64)),
                    derivations: Vec::new(),
                },
            },
        }
    }

    #[test]
    fn a_readme_file_is_read_verbatim() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("README.md");
        fs::write(&path, "# Interop Static\n\nMPL-2.0\n").unwrap();
        assert_eq!(read_readme(&path).unwrap(), "# Interop Static\n\nMPL-2.0\n");
    }

    #[test]
    fn publishing_sends_the_bundle_metadata_and_bearer_credential() {
        let bytes = sample_bundle();
        let api = StubApi::new(vec![json_response(
            "200 OK",
            serde_json::json!({
                "ref": "capsules/koh0920/interop-static",
                "tag": "latest",
                "digest": SAMPLE_DIGEST,
                "size_bytes": bytes.len(),
                "url": "https://stg-app.ato.run/capsules/koh0920/interop-static",
                "created": true,
            }),
        )]);
        let client = api_client().unwrap();
        let response = put_publish(
            &client,
            &api.base,
            "ato_dev_test_token",
            "koh0920",
            "interop-static",
            &publish_request(&bytes),
        )
        .expect("the publish succeeds");
        assert_eq!(response.digest, SAMPLE_DIGEST);
        assert_eq!(response.size_bytes, bytes.len() as u64);

        let requests = api.wait_requests(1);
        assert!(
            requests[0].starts_with("PUT /v1/hub/capsules/koh0920/interop-static "),
            "{}",
            requests[0]
        );
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer ato_dev_test_token"),
            "{}",
            requests[0]
        );
        let body: serde_json::Value =
            serde_json::from_str(requests[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["tag"], "latest");
        assert_eq!(body["digest"], SAMPLE_DIGEST);
        assert_eq!(body["metadata"]["license_spdx"], "MPL-2.0");
        assert_eq!(body["metadata"]["surfaces"][0], "web_ui");
        assert_eq!(body["metadata"]["environments"][0], "macos-arm64");
        let encoded = body["bundle_base64"].as_str().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn publish_failures_are_coded_and_never_echo_the_credential() {
        let bytes = sample_bundle();
        for (status, code) in [
            ("401 Unauthorized", ERR_UNAUTHORIZED),
            ("403 Forbidden", ERR_FORBIDDEN),
            ("409 Conflict", ERR_DIGEST_MISMATCH),
            ("413 Payload Too Large", ERR_BUNDLE_TOO_LARGE),
            ("400 Bad Request", ERR_INVALID_REQUEST),
        ] {
            let api = StubApi::new(vec![json_response(
                status,
                serde_json::json!({"error": "no"}),
            )]);
            let client = api_client().unwrap();
            let error = put_publish(
                &client,
                &api.base,
                "ato_dev_secret",
                "koh0920",
                "interop-static",
                &publish_request(&bytes),
            )
            .expect_err("a refused publish fails");
            let message = error.to_string();
            assert!(message.starts_with(code), "{status}: {message}");
            assert!(!message.contains("ato_dev_secret"), "{status}: {message}");
        }
    }

    #[test]
    fn the_origin_relative_artifact_path_is_checked() {
        assert_eq!(
            origin_relative_path("/v1/hub/capsules/a/b/artifacts/sha256:0").unwrap(),
            "/v1/hub/capsules/a/b/artifacts/sha256:0"
        );
        for value in [
            "https://elsewhere.invalid/x",
            "//elsewhere.invalid/x",
            "v1/hub/x",
            "",
        ] {
            assert!(origin_relative_path(value).is_err(), "{value}");
        }
    }
}
