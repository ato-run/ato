//! Versioned contracts for selecting and accessing a session presentation surface.
//!
//! A descriptor is stable for the lifetime of a session. Access information is
//! deliberately separate because a gateway can rotate or reissue it without
//! changing what surface the session selected.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Version of the tagged-union session-surface contract in this module.
pub const SESSION_SURFACE_CONTRACT_VERSION: &str = "1";
/// Profile for browser-rendered HTTP applications.
pub const WEB_SURFACE_PROFILE: &str = "ato.web-surface.v1";
/// Profile for browser-rendered RFB pixel streams.
pub const PIXEL_STREAM_PROFILE: &str = "ato.pixel-stream.v1";
/// Reserved v1 profile for interactive terminal streams.
pub const TERMINAL_SURFACE_PROFILE: &str = "ato.terminal-surface.v1";
/// Audience required on assertions accepted by a runner surface gateway.
pub const SURFACE_GATEWAY_ASSERTION_AUDIENCE: &str = "ato.runner.surface-gateway";

/// Coarse kind used during capsule/runner/client capability negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSurfaceKind {
    Web,
    PixelStream,
    Terminal,
    /// A newer producer emitted a kind this build cannot select.
    #[serde(other)]
    Unknown,
}

/// The surface kinds an internal submission can currently be published with.
///
/// [`SessionSurfaceKind`] is what this contract can DESCRIBE. This is the
/// narrower set the submission pipeline can carry end to end — build, preview,
/// operate, capture, disposable-restore acceptance, publish. The two are not
/// the same thing and drift apart on purpose: a surface can be describable,
/// renderable and have a production-wired transport while no submission lane
/// can actually produce one.
///
/// `PixelStream` is exactly that case today, and is deliberately absent. The
/// wizard's interactive lane admits only the `recipe` job kind, and the recipe
/// lane in turn refuses a pixel surface requirement and directs the caller to
/// `dockerfile_import`. Both refusals are real code, not gaps in test coverage,
/// so admitting the kind here would advertise a lane that does not exist and
/// move the failure from submission time to capture time.
///
/// Widening this slice is how Pixel is turned on. The envelope, the descriptor,
/// the access rules and the client renderer are all already pixel-capable, so
/// nothing else in the contract changes when it is.
pub const V1_SUBMISSION_SURFACE_KINDS: &[SessionSurfaceKind] = &[SessionSurfaceKind::Web];

/// Narrows a negotiated surface kind to the submission subset, or refuses.
///
/// Fail-closed by construction: a kind is admitted because it is LISTED, not
/// because it failed to match a refusal arm. A kind added to
/// [`SessionSurfaceKind`] later is therefore refused here until somebody
/// deliberately admits it, which is the opposite of the default a `match` with
/// a catch-all would give.
pub fn v1_submission_surface_kind(
    kind: SessionSurfaceKind,
) -> Result<SessionSurfaceKind, SessionSurfaceContractError> {
    if V1_SUBMISSION_SURFACE_KINDS.contains(&kind) {
        Ok(kind)
    } else {
        Err(SessionSurfaceContractError::SurfaceNotInSubmissionSubset(
            kind,
        ))
    }
}

/// Transport selected for a concrete surface profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSurfaceTransport {
    Https,
    RfbWebsocket,
    TerminalWebsocket,
    /// A newer producer emitted a transport this build cannot use.
    #[serde(other)]
    Unknown,
}

/// Capsule-authored surface requirement after manifest resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSurfaceRequirement {
    pub kind: SessionSurfaceKind,
    /// `None` and `Some([])` remain distinct so malformed declarations do not
    /// silently behave like an unconstrained requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<Vec<String>>,
}

/// One client-supported surface kind and its versioned profiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedSessionSurface {
    pub kind: SessionSurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<Vec<String>>,
}

/// Launch-client presentation capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSessionSurfaceCapabilities {
    /// Omission is different from an explicitly empty capability set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_session_surfaces: Option<Vec<AcceptedSessionSurface>>,
}

/// One runner-supported surface kind, its profiles, and transports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportedSessionSurface {
    pub kind: SessionSurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transports: Option<Vec<SessionSurfaceTransport>>,
}

/// Connected-runner presentation capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerSessionSurfaceCapabilities {
    /// Omission is different from an explicitly empty capability set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_session_surfaces: Option<Vec<SupportedSessionSurface>>,
}

/// Validated result of capsule × client × runner surface negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedSessionSurface {
    pub kind: SessionSurfaceKind,
    pub profile: String,
    pub transport: SessionSurfaceTransport,
}

/// Structural errors in a requirement or capability advertisement.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceAdvertisementError {
    #[error("accepted_session_surfaces is missing")]
    MissingAcceptedSessionSurfaces,
    #[error("accepted_session_surfaces is empty")]
    EmptyAcceptedSessionSurfaces,
    #[error("supported_session_surfaces is missing")]
    MissingSupportedSessionSurfaces,
    #[error("supported_session_surfaces is empty")]
    EmptySupportedSessionSurfaces,
    #[error("surface profiles are missing")]
    MissingProfiles,
    #[error("surface profiles are empty")]
    EmptyProfiles,
    #[error("surface profile identifiers must not be empty")]
    EmptyProfile,
    #[error("runner surface transports are missing")]
    MissingTransports,
    #[error("runner surface transports are empty")]
    EmptyTransports,
    #[error("surface kind is unsupported")]
    UnsupportedKind,
    #[error("surface transport is unsupported")]
    UnsupportedTransport,
    #[error("profile {profile} requires transport {transport:?}")]
    RequiredTransportMissing {
        profile: String,
        transport: SessionSurfaceTransport,
    },
}

impl SessionSurfaceRequirement {
    /// Rejects omitted, empty, and unknown requirements before runner leasing.
    pub fn validate(&self) -> Result<(), SurfaceAdvertisementError> {
        validate_kind(self.kind)?;
        validate_profiles(&self.profiles)
    }
}

impl AcceptedSessionSurface {
    /// Validates one client advertisement entry.
    pub fn validate(&self) -> Result<(), SurfaceAdvertisementError> {
        validate_kind(self.kind)?;
        validate_profiles(&self.profiles)
    }
}

impl ClientSessionSurfaceCapabilities {
    /// Validates the complete client capability field without conflating
    /// omission with an intentionally empty array.
    pub fn validate(&self) -> Result<(), SurfaceAdvertisementError> {
        let surfaces = self
            .accepted_session_surfaces
            .as_deref()
            .ok_or(SurfaceAdvertisementError::MissingAcceptedSessionSurfaces)?;
        if surfaces.is_empty() {
            return Err(SurfaceAdvertisementError::EmptyAcceptedSessionSurfaces);
        }
        surfaces
            .iter()
            .try_for_each(AcceptedSessionSurface::validate)
    }
}

impl SupportedSessionSurface {
    /// Validates one runner advertisement entry and its profile/transport
    /// compatibility.
    pub fn validate(&self) -> Result<(), SurfaceAdvertisementError> {
        validate_kind(self.kind)?;
        validate_profiles(&self.profiles)?;
        let transports = self
            .transports
            .as_deref()
            .ok_or(SurfaceAdvertisementError::MissingTransports)?;
        if transports.is_empty() {
            return Err(SurfaceAdvertisementError::EmptyTransports);
        }
        if transports.contains(&SessionSurfaceTransport::Unknown) {
            return Err(SurfaceAdvertisementError::UnsupportedTransport);
        }
        for profile in self.profiles.as_deref().unwrap_or_default() {
            if let Some(required) = required_transport(self.kind, profile)
                && !transports.contains(&required)
            {
                return Err(SurfaceAdvertisementError::RequiredTransportMissing {
                    profile: profile.clone(),
                    transport: required,
                });
            }
        }
        Ok(())
    }
}

impl RunnerSessionSurfaceCapabilities {
    /// Validates the complete runner advertisement field.
    pub fn validate(&self) -> Result<(), SurfaceAdvertisementError> {
        let surfaces = self
            .supported_session_surfaces
            .as_deref()
            .ok_or(SurfaceAdvertisementError::MissingSupportedSessionSurfaces)?;
        if surfaces.is_empty() {
            return Err(SurfaceAdvertisementError::EmptySupportedSessionSurfaces);
        }
        surfaces
            .iter()
            .try_for_each(SupportedSessionSurface::validate)
    }
}

fn validate_kind(kind: SessionSurfaceKind) -> Result<(), SurfaceAdvertisementError> {
    if kind == SessionSurfaceKind::Unknown {
        return Err(SurfaceAdvertisementError::UnsupportedKind);
    }
    Ok(())
}

fn validate_profiles(profiles: &Option<Vec<String>>) -> Result<(), SurfaceAdvertisementError> {
    let profiles = profiles
        .as_deref()
        .ok_or(SurfaceAdvertisementError::MissingProfiles)?;
    if profiles.is_empty() {
        return Err(SurfaceAdvertisementError::EmptyProfiles);
    }
    if profiles.iter().any(|profile| profile.trim().is_empty()) {
        return Err(SurfaceAdvertisementError::EmptyProfile);
    }
    Ok(())
}

fn required_transport(kind: SessionSurfaceKind, profile: &str) -> Option<SessionSurfaceTransport> {
    match (kind, profile) {
        (SessionSurfaceKind::Web, WEB_SURFACE_PROFILE) => Some(SessionSurfaceTransport::Https),
        (SessionSurfaceKind::PixelStream, PIXEL_STREAM_PROFILE) => {
            Some(SessionSurfaceTransport::RfbWebsocket)
        }
        (SessionSurfaceKind::Terminal, TERMINAL_SURFACE_PROFILE) => {
            Some(SessionSurfaceTransport::TerminalWebsocket)
        }
        _ => None,
    }
}

/// Failure to select a surface before runner allocation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceNegotiationError {
    #[error("invalid capsule surface requirement: {0}")]
    InvalidRequirement(SurfaceAdvertisementError),
    #[error("invalid client surface capabilities: {0}")]
    InvalidClient(SurfaceAdvertisementError),
    #[error("invalid runner surface capabilities: {0}")]
    InvalidRunner(SurfaceAdvertisementError),
    #[error("client does not accept required surface kind {0:?}")]
    ClientDoesNotAccept(SessionSurfaceKind),
    #[error("runner does not support required surface kind {0:?}")]
    RunnerDoesNotSupport(SessionSurfaceKind),
    #[error("no common profile for required surface kind {0:?}")]
    NoCommonProfile(SessionSurfaceKind),
    #[error("runner has no compatible transport for profile {0}")]
    NoCompatibleTransport(String),
}

/// Selects a profile only when capsule, launch client, and runner all agree.
/// Callers must perform the same validation again on the runner immediately
/// before materialization; API-side selection is not a trust boundary.
pub fn negotiate_session_surface(
    requirement: &SessionSurfaceRequirement,
    client: &ClientSessionSurfaceCapabilities,
    runner: &RunnerSessionSurfaceCapabilities,
) -> Result<SelectedSessionSurface, SurfaceNegotiationError> {
    requirement
        .validate()
        .map_err(SurfaceNegotiationError::InvalidRequirement)?;
    client
        .validate()
        .map_err(SurfaceNegotiationError::InvalidClient)?;
    runner
        .validate()
        .map_err(SurfaceNegotiationError::InvalidRunner)?;

    let accepted = client
        .accepted_session_surfaces
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|surface| surface.kind == requirement.kind)
        .ok_or(SurfaceNegotiationError::ClientDoesNotAccept(
            requirement.kind,
        ))?;
    let supported = runner
        .supported_session_surfaces
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|surface| surface.kind == requirement.kind)
        .ok_or(SurfaceNegotiationError::RunnerDoesNotSupport(
            requirement.kind,
        ))?;

    let profile = requirement
        .profiles
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|profile| {
            accepted
                .profiles
                .as_deref()
                .unwrap_or_default()
                .contains(profile)
                && supported
                    .profiles
                    .as_deref()
                    .unwrap_or_default()
                    .contains(profile)
        })
        .ok_or(SurfaceNegotiationError::NoCommonProfile(requirement.kind))?;

    let transports = supported.transports.as_deref().unwrap_or_default();
    let transport = required_transport(requirement.kind, profile)
        .filter(|transport| transports.contains(transport))
        .ok_or_else(|| SurfaceNegotiationError::NoCompatibleTransport(profile.clone()))?;

    Ok(SelectedSessionSurface {
        kind: requirement.kind,
        profile: profile.clone(),
        transport,
    })
}

/// Iframe policy attached to a Web surface descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebEmbedPolicy {
    Sandboxed,
    TrustedUnsandboxed,
    ExternalOnly,
    #[serde(other)]
    Unknown,
}

/// Fixed viewport negotiated for a pixel-stream session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelStreamViewport {
    pub width: u32,
    pub height: u32,
}

/// Scalar capability values keep profile metadata deterministic and prevent
/// arbitrary nested JSON from becoming an accidental secondary protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SurfaceCapabilityValue {
    Boolean(bool),
    Integer(i64),
    Text(String),
}

/// Stable session-surface identity and presentation contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionSurfaceDescriptor {
    Web {
        profile: String,
        surface_id: String,
        embed_policy: WebEmbedPolicy,
    },
    PixelStream {
        profile: String,
        surface_id: String,
        transport: SessionSurfaceTransport,
        viewport: PixelStreamViewport,
        capabilities: BTreeMap<String, SurfaceCapabilityValue>,
    },
    Terminal {
        profile: String,
        surface_id: String,
        transport: SessionSurfaceTransport,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        capabilities: BTreeMap<String, SurfaceCapabilityValue>,
    },
    /// Unknown kinds deserialize to a typed unsupported state. They are never
    /// interpreted as Web surfaces.
    #[serde(other)]
    Unknown,
}

impl SessionSurfaceDescriptor {
    /// Returns the descriptor kind without inspecting legacy display fields.
    pub fn kind(&self) -> SessionSurfaceKind {
        match self {
            Self::Web { .. } => SessionSurfaceKind::Web,
            Self::PixelStream { .. } => SessionSurfaceKind::PixelStream,
            Self::Terminal { .. } => SessionSurfaceKind::Terminal,
            Self::Unknown => SessionSurfaceKind::Unknown,
        }
    }

    /// Returns the immutable surface id for supported descriptor kinds.
    pub fn surface_id(&self) -> Option<&str> {
        match self {
            Self::Web { surface_id, .. }
            | Self::PixelStream { surface_id, .. }
            | Self::Terminal { surface_id, .. } => Some(surface_id),
            Self::Unknown => None,
        }
    }

    /// Validates the profile-specific descriptor contract.
    pub fn validate(&self) -> Result<(), SessionSurfaceContractError> {
        match self {
            Self::Web {
                profile,
                surface_id,
                embed_policy,
            } => {
                validate_descriptor_identity(profile, WEB_SURFACE_PROFILE, surface_id)?;
                if *embed_policy == WebEmbedPolicy::Unknown {
                    return Err(SessionSurfaceContractError::UnsupportedEmbedPolicy);
                }
            }
            Self::PixelStream {
                profile,
                surface_id,
                transport,
                viewport,
                ..
            } => {
                validate_descriptor_identity(profile, PIXEL_STREAM_PROFILE, surface_id)?;
                if *transport != SessionSurfaceTransport::RfbWebsocket {
                    return Err(SessionSurfaceContractError::UnsupportedTransport);
                }
                if viewport.width == 0 || viewport.height == 0 {
                    return Err(SessionSurfaceContractError::InvalidViewport);
                }
            }
            Self::Terminal {
                profile,
                surface_id,
                transport,
                ..
            } => {
                validate_descriptor_identity(profile, TERMINAL_SURFACE_PROFILE, surface_id)?;
                if *transport != SessionSurfaceTransport::TerminalWebsocket {
                    return Err(SessionSurfaceContractError::UnsupportedTransport);
                }
            }
            Self::Unknown => {
                return Err(SessionSurfaceContractError::UnsupportedDescriptorKind);
            }
        }
        Ok(())
    }

    /// Converts a validated immutable descriptor back to the capability
    /// selection it claims. Runners compare this with their own negotiation
    /// result before materialization.
    pub fn as_selected_surface(
        &self,
    ) -> Result<SelectedSessionSurface, SessionSurfaceContractError> {
        self.validate()?;
        match self {
            Self::Web { profile, .. } => Ok(SelectedSessionSurface {
                kind: SessionSurfaceKind::Web,
                profile: profile.clone(),
                transport: SessionSurfaceTransport::Https,
            }),
            Self::PixelStream {
                profile, transport, ..
            } => Ok(SelectedSessionSurface {
                kind: SessionSurfaceKind::PixelStream,
                profile: profile.clone(),
                transport: *transport,
            }),
            Self::Terminal {
                profile, transport, ..
            } => Ok(SelectedSessionSurface {
                kind: SessionSurfaceKind::Terminal,
                profile: profile.clone(),
                transport: *transport,
            }),
            Self::Unknown => Err(SessionSurfaceContractError::UnsupportedDescriptorKind),
        }
    }
}

fn validate_descriptor_identity(
    profile: &str,
    expected_profile: &str,
    surface_id: &str,
) -> Result<(), SessionSurfaceContractError> {
    if surface_id.trim().is_empty() {
        return Err(SessionSurfaceContractError::EmptySurfaceId);
    }
    if profile != expected_profile {
        return Err(SessionSurfaceContractError::UnsupportedProfile(
            profile.to_string(),
        ));
    }
    Ok(())
}

/// Rotatable access information for a stable surface descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSurfaceAccess {
    /// Public token-free URL. Pixel streams use an absolute `wss://` URL.
    pub connect_url: String,
    /// Same-origin one-time exchange endpoint when an access grant is needed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_exchange_url: Option<String>,
    pub expires_at: String,
    /// Increments whenever the gateway regenerates access information.
    pub generation: u64,
}

impl SessionSurfaceAccess {
    /// Rejects structurally incomplete access information.
    pub fn validate(&self) -> Result<(), SessionSurfaceContractError> {
        if self.connect_url.trim().is_empty() {
            return Err(SessionSurfaceContractError::EmptyAccessField("connect_url"));
        }
        if self.expires_at.trim().is_empty() {
            return Err(SessionSurfaceContractError::EmptyAccessField("expires_at"));
        }
        if self
            .auth_exchange_url
            .as_deref()
            .is_some_and(|url| url.trim().is_empty())
        {
            return Err(SessionSurfaceContractError::EmptyAccessField(
                "auth_exchange_url",
            ));
        }
        Ok(())
    }
}

/// A stable descriptor paired with access information valid for one gateway
/// generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSurface {
    pub descriptor: SessionSurfaceDescriptor,
    pub access: SessionSurfaceAccess,
}

impl SessionSurface {
    /// Validates descriptor and access as one response unit.
    pub fn validate(&self) -> Result<(), SessionSurfaceContractError> {
        self.descriptor.validate()?;
        self.access.validate()?;
        match &self.descriptor {
            SessionSurfaceDescriptor::PixelStream { .. } => self.validate_authenticated_socket(
                SessionSurfaceContractError::PixelAccessMustUseWss,
                SessionSurfaceContractError::PixelAuthExchangeRequired,
                SessionSurfaceContractError::PixelAuthExchangeMustUseHttps,
                SessionSurfaceContractError::PixelAccessHostsMustMatch,
            )?,
            SessionSurfaceDescriptor::Terminal { .. } => self.validate_authenticated_socket(
                SessionSurfaceContractError::TerminalAccessMustUseWss,
                SessionSurfaceContractError::TerminalAuthExchangeRequired,
                SessionSurfaceContractError::TerminalAuthExchangeMustUseHttps,
                SessionSurfaceContractError::TerminalAccessHostsMustMatch,
            )?,
            SessionSurfaceDescriptor::Web { .. } | SessionSurfaceDescriptor::Unknown => {}
        }
        Ok(())
    }

    fn validate_authenticated_socket(
        &self,
        invalid_connect_scheme: SessionSurfaceContractError,
        missing_exchange: SessionSurfaceContractError,
        invalid_exchange_scheme: SessionSurfaceContractError,
        host_mismatch: SessionSurfaceContractError,
    ) -> Result<(), SessionSurfaceContractError> {
        let connect_host =
            absolute_url_host(&self.access.connect_url, "wss://").ok_or(invalid_connect_scheme)?;
        let exchange = self
            .access
            .auth_exchange_url
            .as_deref()
            .ok_or(missing_exchange)?;
        let exchange_host =
            absolute_url_host(exchange, "https://").ok_or(invalid_exchange_scheme)?;
        if connect_host != exchange_host {
            return Err(host_mismatch);
        }
        Ok(())
    }

    /// Produces legacy fields only for Web surfaces during the dual-write
    /// migration window. Pixel and terminal surfaces deliberately return
    /// `None` rather than fabricating an `app_url`.
    pub fn legacy_web_projection(&self) -> Option<LegacyWebSurfaceFields> {
        let SessionSurfaceDescriptor::Web { embed_policy, .. } = &self.descriptor else {
            return None;
        };
        Some(LegacyWebSurfaceFields {
            app_url: Some(self.access.connect_url.clone()),
            embed_policy: Some(*embed_policy),
            app_expires_at: Some(self.access.expires_at.clone()),
        })
    }
}

/// Extracts a normalized authority from the two public URL forms used by the
/// surface contract. This intentionally stays local so the protocol DAG root
/// does not acquire a general URL/runtime dependency.
fn absolute_url_host<'a>(value: &'a str, scheme: &str) -> Option<&'a str> {
    let remainder = value.strip_prefix(scheme)?;
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return None;
    }
    Some(authority)
}

/// Canonical ready-envelope fields owned by the session-surface contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSurfaceEnvelope {
    pub surface_contract_version: String,
    pub surface: SessionSurface,
}

impl SessionSurfaceEnvelope {
    /// Validates the version before interpreting the tagged descriptor.
    pub fn validate(&self) -> Result<(), SessionSurfaceContractError> {
        if self.surface_contract_version != SESSION_SURFACE_CONTRACT_VERSION {
            return Err(SessionSurfaceContractError::UnsupportedContractVersion(
                self.surface_contract_version.clone(),
            ));
        }
        self.surface.validate()
    }
}

/// Legacy Web fields retained only for an additive migration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyWebSurfaceFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_policy: Option<WebEmbedPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_expires_at: Option<String>,
}

/// Whether a resolved surface came from the canonical contract or the legacy
/// `app_url` compatibility path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSurfaceSource {
    Contract,
    LegacyAppUrl,
}

/// Result of the single dual-read boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSessionSurface {
    pub source: SessionSurfaceSource,
    pub envelope: SessionSurfaceEnvelope,
}

/// Applies the v1 dual-read rules without hiding a malformed canonical
/// surface behind legacy fields.
pub fn resolve_session_surface(
    surface_field_present: bool,
    surface_contract_version: Option<&str>,
    surface: Option<SessionSurface>,
    legacy: LegacyWebSurfaceFields,
    legacy_surface_id: &str,
) -> Result<Option<ResolvedSessionSurface>, SessionSurfaceContractError> {
    if surface_field_present {
        let surface = surface.ok_or(SessionSurfaceContractError::MalformedSurface)?;
        let version =
            surface_contract_version.ok_or(SessionSurfaceContractError::MissingContractVersion)?;
        let envelope = SessionSurfaceEnvelope {
            surface_contract_version: version.to_string(),
            surface,
        };
        envelope.validate()?;
        return Ok(Some(ResolvedSessionSurface {
            source: SessionSurfaceSource::Contract,
            envelope,
        }));
    }

    let Some(app_url) = legacy.app_url else {
        return Ok(None);
    };
    if legacy_surface_id.trim().is_empty() {
        return Err(SessionSurfaceContractError::EmptySurfaceId);
    }
    let expires_at = legacy
        .app_expires_at
        .ok_or(SessionSurfaceContractError::LegacyWebMissingExpiry)?;
    let embed_policy = legacy.embed_policy.unwrap_or(WebEmbedPolicy::Sandboxed);
    let envelope = SessionSurfaceEnvelope {
        surface_contract_version: SESSION_SURFACE_CONTRACT_VERSION.to_string(),
        surface: SessionSurface {
            descriptor: SessionSurfaceDescriptor::Web {
                profile: WEB_SURFACE_PROFILE.to_string(),
                surface_id: legacy_surface_id.to_string(),
                embed_policy,
            },
            access: SessionSurfaceAccess {
                connect_url: app_url,
                auth_exchange_url: None,
                expires_at,
                generation: 0,
            },
        },
    };
    envelope.validate()?;
    Ok(Some(ResolvedSessionSurface {
        source: SessionSurfaceSource::LegacyAppUrl,
        envelope,
    }))
}

/// Typed failures at the versioned descriptor/access boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionSurfaceContractError {
    #[error("surface_contract_version is missing")]
    MissingContractVersion,
    #[error("surface contract version {0} is unsupported")]
    UnsupportedContractVersion(String),
    #[error("surface field is present but malformed")]
    MalformedSurface,
    #[error("session surface kind is unsupported")]
    UnsupportedDescriptorKind,
    #[error("session surface profile {0} is unsupported")]
    UnsupportedProfile(String),
    #[error("session surface transport is unsupported")]
    UnsupportedTransport,
    #[error("Web embed policy is unsupported")]
    UnsupportedEmbedPolicy,
    #[error("surface_id must not be empty")]
    EmptySurfaceId,
    #[error("pixel stream viewport dimensions must be positive")]
    InvalidViewport,
    #[error("surface access field {0} must not be empty")]
    EmptyAccessField(&'static str),
    #[error("authenticated Pixel access requires auth_exchange_url")]
    PixelAuthExchangeRequired,
    #[error("Pixel connect_url must use wss://")]
    PixelAccessMustUseWss,
    #[error("Pixel auth_exchange_url must use https://")]
    PixelAuthExchangeMustUseHttps,
    #[error("Pixel connect_url and auth_exchange_url must use the same host")]
    PixelAccessHostsMustMatch,
    #[error("authenticated Terminal access requires auth_exchange_url")]
    TerminalAuthExchangeRequired,
    #[error("Terminal connect_url must use wss://")]
    TerminalAccessMustUseWss,
    #[error("Terminal auth_exchange_url must use https://")]
    TerminalAuthExchangeMustUseHttps,
    #[error("Terminal connect_url and auth_exchange_url must use the same host")]
    TerminalAccessHostsMustMatch,
    #[error("legacy app_url response is missing app_expires_at")]
    LegacyWebMissingExpiry,
    /// The surface is well-formed and describable, but no submission lane can
    /// produce one. Distinct from [`Self::UnsupportedDescriptorKind`], which
    /// means the surface itself cannot be interpreted — here it can, and the
    /// operator's next step is a different one.
    #[error("session surface kind {0:?} is not admitted by the v1 submission subset")]
    SurfaceNotInSubmissionSubset(SessionSurfaceKind),
}

/// Principal kind bound into a gateway assertion. `Guest` is a wire
/// reservation only until the SPEC-G issuance path is explicitly approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfacePrincipalKind {
    User,
    Guest,
    #[serde(other)]
    Unknown,
}

/// Principal identity bound into a surface-gateway assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceAssertionPrincipal {
    pub kind: SurfacePrincipalKind,
    pub id: String,
}

/// Signed assertion claims consumed by the runner surface gateway. The
/// signature container and key material intentionally live outside this DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceAssertionClaims {
    pub aud: String,
    pub session_id: String,
    pub surface_id: String,
    pub principal: SurfaceAssertionPrincipal,
    /// Unix timestamp in seconds.
    pub exp: u64,
    pub jti: String,
    pub kid: String,
}

impl SurfaceAssertionClaims {
    /// Performs structural and audience validation. Callers must separately
    /// verify the signature, expiry against their clock, and jti replay state.
    pub fn validate(&self) -> Result<(), SurfaceAssertionError> {
        if self.aud != SURFACE_GATEWAY_ASSERTION_AUDIENCE {
            return Err(SurfaceAssertionError::InvalidAudience);
        }
        validate_claim("session_id", &self.session_id)?;
        validate_claim("surface_id", &self.surface_id)?;
        validate_claim("principal.id", &self.principal.id)?;
        validate_claim("jti", &self.jti)?;
        validate_claim("kid", &self.kid)?;
        if self.principal.kind == SurfacePrincipalKind::Unknown {
            return Err(SurfaceAssertionError::UnsupportedPrincipalKind);
        }
        if self.exp == 0 {
            return Err(SurfaceAssertionError::InvalidExpiry);
        }
        Ok(())
    }
}

fn validate_claim(name: &'static str, value: &str) -> Result<(), SurfaceAssertionError> {
    if value.trim().is_empty() {
        return Err(SurfaceAssertionError::EmptyClaim(name));
    }
    Ok(())
}

/// Structural validation failures for surface-gateway assertions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceAssertionError {
    #[error("surface assertion audience is invalid")]
    InvalidAudience,
    #[error("surface assertion claim {0} must not be empty")]
    EmptyClaim(&'static str),
    #[error("surface assertion principal kind is unsupported")]
    UnsupportedPrincipalKind,
    #[error("surface assertion expiry must be non-zero")]
    InvalidExpiry,
}

/// Semantic role of an endpoint restored with a Ready-State artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointRole {
    AppHttp,
    PixelRfb,
    GuestControl,
}

/// Wire protocol spoken by a restored endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointProtocol {
    Http,
    Tcp,
    Vsock,
}

/// Exposure boundary for a restored endpoint. There is intentionally no
/// serde default: omission and unknown values fail deserialization closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointExposure {
    GuestPrivate,
    HostInternal,
    PublicProxy,
}

/// Readiness signal required before an endpoint is exposed to its consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndpointReadiness {
    None,
    HttpGet {
        path: String,
    },
    TcpConnect,
    /// Pixel stream is usable only after the gateway observes the first
    /// framebuffer update; accepting a bare TCP socket would report ready too soon.
    FirstFrame,
    VsockConnect,
}

/// One endpoint carried by a Ready-State restore contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointContract {
    pub role: EndpointRole,
    pub protocol: EndpointProtocol,
    pub exposure: EndpointExposure,
    /// TCP/HTTP ports are validated to u16; vsock ports use the full u32 range.
    pub port: u32,
    pub readiness: EndpointReadiness,
}

impl EndpointContract {
    /// Enforces role/protocol/exposure compatibility before any listener is
    /// routed. In particular, guest RFB is never public ingress.
    pub fn validate(&self) -> Result<(), EndpointContractError> {
        if self.port == 0 {
            return Err(EndpointContractError::InvalidPort(self.port));
        }
        if matches!(
            self.protocol,
            EndpointProtocol::Http | EndpointProtocol::Tcp
        ) && self.port > u16::MAX.into()
        {
            return Err(EndpointContractError::InvalidPort(self.port));
        }
        if matches!(
            self.role,
            EndpointRole::PixelRfb | EndpointRole::GuestControl
        ) && self.exposure == EndpointExposure::PublicProxy
        {
            return Err(EndpointContractError::RoleCannotBePublic(self.role));
        }
        let expected_protocol = match self.role {
            EndpointRole::AppHttp => EndpointProtocol::Http,
            EndpointRole::PixelRfb => EndpointProtocol::Tcp,
            EndpointRole::GuestControl => EndpointProtocol::Vsock,
        };
        if self.protocol != expected_protocol {
            return Err(EndpointContractError::RoleProtocolMismatch {
                role: self.role,
                protocol: self.protocol,
            });
        }
        if self.role == EndpointRole::PixelRfb && self.readiness != EndpointReadiness::FirstFrame {
            return Err(EndpointContractError::PixelFirstFrameRequired);
        }
        match &self.readiness {
            EndpointReadiness::None => {}
            EndpointReadiness::HttpGet { path } => {
                if self.protocol != EndpointProtocol::Http {
                    return Err(EndpointContractError::ReadinessProtocolMismatch);
                }
                if path.trim().is_empty() {
                    return Err(EndpointContractError::EmptyHealthcheckPath);
                }
            }
            EndpointReadiness::TcpConnect => {
                if self.protocol != EndpointProtocol::Tcp {
                    return Err(EndpointContractError::ReadinessProtocolMismatch);
                }
            }
            EndpointReadiness::FirstFrame => {
                if self.role != EndpointRole::PixelRfb || self.protocol != EndpointProtocol::Tcp {
                    return Err(EndpointContractError::ReadinessProtocolMismatch);
                }
            }
            EndpointReadiness::VsockConnect => {
                if self.protocol != EndpointProtocol::Vsock {
                    return Err(EndpointContractError::ReadinessProtocolMismatch);
                }
            }
        }
        Ok(())
    }
}

/// Restore endpoint contract failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointContractError {
    #[error("endpoint port {0} is invalid for its protocol")]
    InvalidPort(u32),
    #[error("endpoint role {0:?} must not use public_proxy exposure")]
    RoleCannotBePublic(EndpointRole),
    #[error("endpoint role {role:?} is incompatible with protocol {protocol:?}")]
    RoleProtocolMismatch {
        role: EndpointRole,
        protocol: EndpointProtocol,
    },
    #[error("endpoint readiness kind is incompatible with its protocol")]
    ReadinessProtocolMismatch,
    #[error("pixel_rfb readiness must be first_frame")]
    PixelFirstFrameRequired,
    #[error("HTTP readiness path must not be empty")]
    EmptyHealthcheckPath,
}
