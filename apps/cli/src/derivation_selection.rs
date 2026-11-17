//! Which declared Derivation a Run starts (M2 §1).
//!
//! Two layers, deliberately separate:
//!
//! - [candidates] projects a decoded bundle into [DerivationCandidates]: a
//!   serializable DTO built from the same validate_bundle_for_derivation and
//!   ato_host_facts admission a Run uses. It prints nothing and starts
//!   nothing, so the Desktop can consume exactly the facts a terminal shows.
//! - [resolve_choice] is the decision over the declared references, and
//!   [render_candidates] / [render_permissions] are pure text. The prompt that
//!   reads a terminal lives in the CLI's Run path, not here.
//!
//! A choice never rewrites K: the bundle bytes and the validated route handed
//! to the runtime are the ones that were decoded, and the receipt records the
//! selected DerivationRef. Nothing here substitutes another Derivation when the
//! selected one cannot run.

use std::path::Path;

use ato_formation::authoring::{BoundDerivation, StateAccess, StepNetwork};
use ato_formation::native_surface::NativePresentation;
use ato_formation::requirements::{ExecutionPhase, ResourceOperation};
use ato_host_facts::{CapabilityState, ExecutionRequirements, HostFacts, Reason};
use ato_objects::PortableApplicationBundle;
use ato_portable_application::portable_run::execution_requirements_for;
use ato_portable_application::{
    PortableRealizationKind, ValidatedPortableApplication, validate_bundle_for_derivation,
};
use serde::Serialize;

/// The schema of "ato run --list-derivations".
pub const LIST_SCHEMA: &str = "ato.derivation-candidates/1";

/// Stable refusal code: the bundle declares several routes and none was named.
pub const ERR_SELECTION_REQUIRED: &str = "derivation-selection-required";

/// Stable refusal code: the selected route cannot run on this host.
pub const ERR_CAPABILITY_UNAVAILABLE: &str = "derivation-capability-unavailable";

/// An invocation that needs an explicit decision (a Derivation to name) exits
/// with this code, never with a bare failure.
const EXIT_NEEDS_EXPLICIT_DECISION: i32 = 2;

/// Every declared Derivation of one bundle: one selection screen, or one
/// "ato run --list-derivations" document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DerivationCandidates {
    /// Always [LIST_SCHEMA].
    pub schema: &'static str,
    /// SHA-256 of the bundle bytes the Run would execute, unchanged by a choice.
    pub bundle_sha256: String,
    /// The bundle's ContractRef. Selection does not change it.
    pub contract_ref: String,
    /// The host the admission states were decided on.
    pub host: CandidateHost,
    /// In the bundle's canonical (sorted) Derivation order, so the same bytes
    /// always produce the same document.
    pub derivations: Vec<DerivationCandidate>,
}

/// The measured host the admission states belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateHost {
    pub os: String,
    pub arch: String,
}

/// One declared Derivation and what choosing it would start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DerivationCandidate {
    pub derivation_ref: String,
    /// The bundle's application title. None when the route does not validate,
    /// because then the bundle states no readable name for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// static_web, process, oci, native_terminal or native_pixel.
    /// None when the route does not validate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realization: Option<&'static str>,
    /// The validated route's label, e.g. "OCI Service Group". Kept beside
    /// realization because the capability lines print it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<&'static str>,
    /// The os/arch platform this route's images are pinned to, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The os/arch a native desktop route is built for, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_platform: Option<String>,
    pub requires: CandidateRequires,
    /// Only declared permissions: nothing is inferred from argv or a filename.
    pub permissions: Vec<CandidatePermission>,
    /// Declared runtimes at exact versions, plus the host tools the route's
    /// realization needs.
    pub tools: Vec<String>,
    /// Bytes this route would still have to fetch, when the bundle declares
    /// them (Model Set objects and pinned remote artifacts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_bytes: Option<u64>,
    /// Whether this host's measured facts admit the route. Never a guess:
    /// false covers both proven-impossible and not-proven.
    pub runnable: bool,
    /// The Host Facts decision and its reasons. None when the route does not
    /// validate, in which case [Self::invalid] names the format error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission: Option<CandidateAdmission>,
    /// Why the route does not validate. A format failure, not a host verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
    /// Application surface labels this route exposes.
    pub surfaces: Vec<String>,
}

/// What the selected route needs from the host, in the admission vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CandidateRequires {
    pub container: bool,
    pub state: bool,
}

/// One declared permission the selected route would exercise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidatePermission {
    /// container, state, native, network or authority.
    pub kind: &'static str,
    /// The declared object, host included. Never a host path.
    pub detail: String,
}

/// The measured verdict on this host, with the causes that decided it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateAdmission {
    /// advertised, unavailable or unverified (unknown is never available,
    /// exactly as the control plane treats it).
    pub state: CapabilityState,
    /// How an OCI route reaches its Ports here: direct, relay, none, process
    /// or not-applicable.
    pub connector: &'static str,
    pub reasons: Vec<Reason>,
}

/// What naming a Derivation did, before any host question is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DerivationChoice {
    /// The caller named a DerivationRef. It is used as named -- declared or
    /// not, the validator is what reports an undeclared one, by name.
    Explicit(String),
    /// The bundle declares exactly one Derivation, so there is nothing to pick.
    Only(String),
    /// More than one Derivation is declared and none was named.
    NeedsSelection,
}

/// The decision over the declared references. Pure: no host, no I/O.
///
/// A bundle with several routes never yields one of them, and an explicit
/// reference is never replaced by a declared one.
pub fn resolve_choice(declared: &[String], requested: Option<&str>) -> DerivationChoice {
    if let Some(reference) = requested {
        return DerivationChoice::Explicit(reference.to_owned());
    }
    match declared {
        [only] => DerivationChoice::Only(only.clone()),
        _ => DerivationChoice::NeedsSelection,
    }
}

/// Project every declared Derivation of a decoded bundle.
///
/// admission_root is the Run scratch the admission is decided for; the Run
/// passes the directory it will actually use, so the printed verdict and the
/// admission the Run applies cannot disagree.
pub fn candidates(
    bundle: &PortableApplicationBundle,
    bundle_sha256: &str,
    admission_root: &Path,
) -> anyhow::Result<DerivationCandidates> {
    let declared = &bundle.index.derivations;
    let mut probed = Vec::with_capacity(declared.len());
    let mut invalid = Vec::with_capacity(declared.len());
    for reference in declared {
        match validate_bundle_for_derivation(bundle, reference) {
            Ok(route) => {
                let requirements = execution_requirements_for(&route, admission_root);
                probed.push((route, requirements));
            }
            Err(error) => invalid.push((reference.clone(), format!("{error}"))),
        }
    }
    // A bundle whose closure or every route is malformed is a broken bundle,
    // not a choice: report the format failure the Run would hit instead of a
    // list of empty candidates.
    if probed.is_empty()
        && let Some((_, error)) = invalid.first()
    {
        anyhow::bail!("{error}");
    }
    let requirements: Vec<ExecutionRequirements> = probed
        .iter()
        .map(|(_, requirements)| requirements.clone())
        .collect();
    let host = HostFacts::probe(&requirements);
    let mut derivations = Vec::with_capacity(declared.len());
    for reference in declared {
        match probed
            .iter()
            .find(|(route, _)| route.derivation_ref.as_str() == reference)
        {
            Some((route, requirements)) => {
                derivations.push(project(route, requirements, &host));
            }
            None => {
                let error = invalid
                    .iter()
                    .find(|(invalid, _)| invalid == reference)
                    .map(|(_, error)| error.clone())
                    .unwrap_or_else(|| "the route did not validate".to_owned());
                derivations.push(unvalidated(reference, error));
            }
        }
    }
    Ok(DerivationCandidates {
        schema: LIST_SCHEMA,
        bundle_sha256: bundle_sha256.to_owned(),
        contract_ref: bundle.index.root_contract_ref.clone(),
        host: CandidateHost {
            os: host.os.clone(),
            arch: host.arch.clone(),
        },
        derivations,
    })
}

/// Project one named Derivation, validating only it.
///
/// None means the bundle does not declare the reference; the Run path then
/// reports that by name, exactly as it did before, instead of substituting a
/// declared route.
pub fn candidate(
    bundle: &PortableApplicationBundle,
    derivation_ref: &str,
    admission_root: &Path,
) -> anyhow::Result<Option<DerivationCandidate>> {
    if !bundle
        .index
        .derivations
        .iter()
        .any(|declared| declared == derivation_ref)
    {
        return Ok(None);
    }
    let route = validate_bundle_for_derivation(bundle, derivation_ref)?;
    let requirements = execution_requirements_for(&route, admission_root);
    let host = HostFacts::probe(std::slice::from_ref(&requirements));
    Ok(Some(project(&route, &requirements, &host)))
}

fn project(
    route: &ValidatedPortableApplication,
    requirements: &ExecutionRequirements,
    host: &HostFacts,
) -> DerivationCandidate {
    let status = ato_host_facts::execution_status(host, requirements);
    let derivation = &route.derivation;
    let mut tools = declared_tools(derivation);
    if requirements.needs_container {
        tools.insert("docker".to_owned());
    }
    if requirements.needs_state && !requirements.needs_container {
        tools.insert("bubblewrap".to_owned());
    }
    // A macOS native route needs the OS tools its mandatory policy uses.
    // Windows signature verification is an OS API, not a CLI tool.
    if let Some(native) = &route.native
        && native.descriptor.os.as_str() == "macos"
    {
        tools.insert("codesign".to_owned());
        tools.insert("sandbox-exec".to_owned());
        tools.insert("spctl".to_owned());
    }
    let mut download_bytes = route
        .model_sets
        .iter()
        .map(|set| set.manifest.total_bytes())
        .sum::<u64>();
    if let Some(declared) = derivation.runtimes.get("ato.native.remote_bytes")
        && let Ok(bytes) = declared.parse::<u64>()
    {
        download_bytes = download_bytes.saturating_add(bytes);
    }
    DerivationCandidate {
        derivation_ref: route.derivation_ref.to_string(),
        display_name: Some(route.application.title.clone()),
        realization: Some(realization_token(route)),
        route: Some(route.realization.label()),
        platform: requirements.platform.clone(),
        native_platform: requirements.native_platform.clone(),
        requires: CandidateRequires {
            container: requirements.needs_container,
            state: requirements.needs_state,
        },
        permissions: permissions(route, requirements),
        tools: tools.into_iter().collect(),
        download_bytes: (download_bytes > 0).then_some(download_bytes),
        runnable: status.state == CapabilityState::Advertised,
        admission: Some(CandidateAdmission {
            state: status.state,
            connector: connector_for(route, host),
            reasons: status.reasons,
        }),
        invalid: None,
        surfaces: route
            .application
            .surfaces
            .iter()
            .map(|surface| format!("{}:{}", surface.id, surface.port))
            .collect(),
    }
}

fn unvalidated(derivation_ref: &str, error: String) -> DerivationCandidate {
    DerivationCandidate {
        derivation_ref: derivation_ref.to_owned(),
        display_name: None,
        realization: None,
        route: None,
        platform: None,
        native_platform: None,
        requires: CandidateRequires {
            container: false,
            state: false,
        },
        permissions: Vec::new(),
        tools: Vec::new(),
        download_bytes: None,
        runnable: false,
        admission: None,
        invalid: Some(error),
        surfaces: Vec::new(),
    }
}

/// The contract's five-value realization vocabulary.
///
/// Deliberately not the Hub metadata vocabulary: a Hub capability document
/// names a local process local_process, and this document is read by a person
/// choosing what to run.
pub fn realization_token(route: &ValidatedPortableApplication) -> &'static str {
    if let Some(native) = &route.derivation.native_surface {
        return match native.surface {
            NativePresentation::Terminal { .. } => "native_terminal",
            NativePresentation::PixelStream { .. } => "native_pixel",
        };
    }
    match route.realization {
        PortableRealizationKind::StaticWeb => "static_web",
        PortableRealizationKind::LocalProcess | PortableRealizationKind::ProcessServiceGroup => {
            "process"
        }
        PortableRealizationKind::OciContainer | PortableRealizationKind::OciServiceGroup => "oci",
        // One desktop artifact whose presentation is the app's own window; the
        // Hub metadata names the same route native_pixel.
        PortableRealizationKind::NativeDesktop => "native_pixel",
    }
}

/// How this host reaches the route's logical Ports.
///
/// The same projection ato pack and a Hub Run preflight print, so a route is
/// described with one answer everywhere.
pub fn connector_for(route: &ValidatedPortableApplication, host: &HostFacts) -> &'static str {
    match route.realization {
        PortableRealizationKind::StaticWeb => "not-applicable",
        PortableRealizationKind::LocalProcess => "process",
        _ => host
            .connector_choice()
            .map(|choice| choice.as_str())
            .unwrap_or("none"),
    }
}

/// The lines ato pack prints for one Derivation.
///
/// Kept as a pure function so the wording is snapshot-tested rather than
/// assembled in three places.
pub fn capability_lines(
    derivation: &str,
    route: &str,
    platform: &str,
    connector: &str,
    status: &ato_host_facts::CapabilityStatus,
) -> Vec<String> {
    let runnable = if status.state == CapabilityState::Advertised {
        "yes"
    } else {
        "no"
    };
    let mut lines = vec![format!(
        "capability derivation_ref={derivation} route={route} platform={platform} connector={connector} runnable={runnable} state={:?}",
        status.state
    )];
    for reason in &status.reasons {
        let kind = if reason.code.is_warning() {
            "warning"
        } else {
            "reason"
        };
        lines.push(format!(
            "capability {kind}={} message={}",
            reason.code.as_str(),
            reason.message
        ));
    }
    lines
}

/// The capability lines for one candidate, in that same vocabulary.
pub fn candidate_capability_lines(candidate: &DerivationCandidate) -> Vec<String> {
    let Some(admission) = &candidate.admission else {
        return Vec::new();
    };
    let status = ato_host_facts::CapabilityStatus {
        capability: "route:execution".to_owned(),
        state: admission.state,
        reasons: admission.reasons.clone(),
    };
    capability_lines(
        &candidate.derivation_ref,
        candidate.route.unwrap_or("unknown"),
        &candidate.required_platform(),
        admission.connector,
        &status,
    )
}

impl DerivationCandidate {
    /// The os/arch this route is pinned to, for the capability line.
    pub fn required_platform(&self) -> String {
        self.platform
            .as_deref()
            .or(self.native_platform.as_deref())
            .unwrap_or("none")
            .to_owned()
    }

    /// The "required platform=... native_platform=... container=... state=..."
    /// phrase a refusal prints.
    pub fn required_environment(&self) -> String {
        format!(
            "platform={} native_platform={} container={} state={}",
            self.platform.as_deref().unwrap_or("none"),
            self.native_platform.as_deref().unwrap_or("none"),
            self.requires.container,
            self.requires.state,
        )
    }
}

fn declared_tools(derivation: &BoundDerivation) -> std::collections::BTreeSet<String> {
    let mut tools = std::collections::BTreeSet::new();
    for facts in derivation
        .steps
        .iter()
        .map(|step| &step.runtimes)
        .chain(std::iter::once(&derivation.runtimes))
    {
        for (name, version) in facts {
            // "ato.*" keys are adapter-owned facts (an artifact reference, an
            // image digest), not a runtime a host must provision.
            if !name.starts_with("ato.") {
                tools.insert(format!("{name} {version}"));
            }
        }
    }
    tools
}

fn permissions(
    route: &ValidatedPortableApplication,
    requirements: &ExecutionRequirements,
) -> Vec<CandidatePermission> {
    let derivation = &route.derivation;
    let mut permissions = Vec::new();
    if requirements.needs_container {
        permissions.push(CandidatePermission {
            kind: "container",
            detail: "run this route in an OCI container on the local daemon".to_owned(),
        });
    }
    if !derivation.state.is_empty() {
        let slots: Vec<String> = derivation
            .state
            .iter()
            .map(|slot| {
                let access = match slot.access {
                    StateAccess::ReadOnly => "read-only",
                    StateAccess::ReadWrite => "read-write",
                };
                format!("{} ({access})", slot.mount)
            })
            .collect();
        permissions.push(CandidatePermission {
            kind: "state",
            detail: format!("the Run's filesystem state at {}", slots.join(", ")),
        });
    }
    if let Some(native) = &route.native {
        permissions.push(CandidatePermission {
            kind: "native",
            detail: format!(
                "launch a native {} application on this desktop (signature policy: {}, host sandbox required)",
                native.descriptor.os.as_str(),
                native.descriptor.signature.as_str(),
            ),
        });
    }
    // A pinned remote artifact is a declared download, so it is a declared
    // network permission as well: a remote URL is never fetched silently.
    if let Some(url) = derivation.runtimes.get("ato.native.remote_url") {
        let bytes = derivation
            .runtimes
            .get("ato.native.remote_bytes")
            .map(|declared| format!(" ({declared} bytes)"))
            .unwrap_or_default();
        permissions.push(CandidatePermission {
            kind: "network",
            detail: format!("download {url}{bytes} with its declared SHA-256"),
        });
    }
    for requirement in &derivation.requirements.network {
        permissions.push(CandidatePermission {
            kind: "network",
            detail: format!(
                "{} {}:{}",
                phase_token(requirement.phase),
                requirement.host,
                requirement.port
            ),
        });
    }
    for step in &derivation.steps {
        if !step.network.is_denied() {
            permissions.push(CandidatePermission {
                kind: "network",
                detail: format!(
                    "step {} needs {}",
                    step.id,
                    step_network_token(step.network)
                ),
            });
        }
    }
    for requirement in &derivation.requirements.authority {
        permissions.push(CandidatePermission {
            kind: "authority",
            detail: format!(
                "{} {}:{}:{}",
                phase_token(requirement.phase),
                requirement.protocol,
                requirement.resource,
                operation_token(requirement.operation),
            ),
        });
    }
    permissions
}

fn phase_token(phase: ExecutionPhase) -> &'static str {
    match phase {
        ExecutionPhase::Dependencies => "dependencies",
        ExecutionPhase::Build => "build",
        ExecutionPhase::Runtime => "runtime",
    }
}

fn operation_token(operation: ResourceOperation) -> &'static str {
    match operation {
        ResourceOperation::Read => "read",
        ResourceOperation::Write => "write",
        ResourceOperation::Create => "create",
        ResourceOperation::Execute => "execute",
        ResourceOperation::Bind => "bind",
    }
}

fn step_network_token(network: StepNetwork) -> &'static str {
    match network {
        StepNetwork::Denied => "denied",
        StepNetwork::DependencyResolution => "dependency-resolution",
        StepNetwork::ScopedDependencies => "scoped-dependencies",
        StepNetwork::ScopedBuild => "scoped-build",
        StepNetwork::ScopedRuntime => "scoped-runtime",
    }
}

/// The numbered selection screen, without a prompt.
///
/// Every fact a choice depends on is here: the name the bundle carries, how the
/// route runs, the platform, the tools and declared permissions, the bytes it
/// would still fetch, and this host's measured verdict with its reasons.
pub fn render_candidates(candidates: &DerivationCandidates) -> Vec<String> {
    let mut lines = vec![format!(
        "Capsule {} declares {} derivations. Select one to run:",
        candidates.bundle_sha256,
        candidates.derivations.len()
    )];
    for (index, candidate) in candidates.derivations.iter().enumerate() {
        lines.push(String::new());
        lines.push(format!(
            "  {}) {}",
            index + 1,
            candidate
                .display_name
                .as_deref()
                .unwrap_or("(unvalidated derivation)")
        ));
        match (candidate.realization, candidate.route) {
            (Some(realization), Some(route)) => {
                lines.push(format!("     route: {realization} ({route})"));
            }
            _ => lines.push("     route: not validated".to_owned()),
        }
        lines.push(format!("     platform: {}", candidate.required_platform()));
        lines.push(format!("     tools: {}", join_or_none(&candidate.tools)));
        lines.push(format!(
            "     permissions: {}",
            permission_list(&candidate.permissions)
        ));
        lines.push(format!(
            "     download: {}",
            match candidate.download_bytes {
                Some(bytes) => format!("{} declared", human_bytes(bytes)),
                None => "none declared".to_owned(),
            }
        ));
        lines.push(format!("     host: {}", host_verdict(candidate)));
        for reason in candidate
            .admission
            .as_ref()
            .map(|admission| admission.reasons.as_slice())
            .unwrap_or(&[])
        {
            lines.push(format!(
                "       reason: {} - {}",
                reason.code.as_str(),
                reason.message
            ));
        }
        if let Some(invalid) = &candidate.invalid {
            lines.push(format!("       invalid: {invalid}"));
        }
        if !candidate.surfaces.is_empty() {
            lines.push(format!("     surfaces: {}", candidate.surfaces.join(", ")));
        }
        lines.push(format!("     derivation: {}", candidate.derivation_ref));
    }
    lines
}

/// The permissions the selected route would exercise, one per line.
pub fn render_permissions(candidate: &DerivationCandidate) -> Vec<String> {
    if candidate.permissions.is_empty() {
        return vec![format!(
            "Route {} requires no host permissions.",
            candidate.derivation_ref
        )];
    }
    let mut lines = vec![format!("Route {} requires:", candidate.derivation_ref)];
    for permission in &candidate.permissions {
        lines.push(format!("  - {}: {}", permission.kind, permission.detail));
    }
    lines
}

fn host_verdict(candidate: &DerivationCandidate) -> &'static str {
    if candidate.invalid.is_some() {
        return "not validated";
    }
    match candidate
        .admission
        .as_ref()
        .map(|admission| admission.state)
    {
        Some(CapabilityState::Advertised) => "runnable on this host",
        Some(CapabilityState::Unavailable) => "not runnable on this host",
        // Unknown is never reported as available.
        _ => "not proven runnable on this host",
    }
}

fn permission_list(permissions: &[CandidatePermission]) -> String {
    if permissions.is_empty() {
        return "none".to_owned();
    }
    permissions
        .iter()
        .map(|permission| format!("{}: {}", permission.kind, permission.detail))
        .collect::<Vec<_>>()
        .join("; ")
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Whether a Run has to show the permission confirmation.
///
/// Only a route chosen from the menu, with declared permissions and without
/// the explicit acceptance flag asks. An explicitly named Derivation is its own
/// decision; a non-interactive Run has no prompt at all, so it must name one.
pub fn needs_permission_confirmation(
    chosen_from_menu: bool,
    accept_permissions: bool,
    candidate: &DerivationCandidate,
) -> bool {
    chosen_from_menu && !accept_permissions && !candidate.permissions.is_empty()
}

/// One answer to the numbered menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAnswer {
    /// Zero-based index of the chosen candidate.
    Index(usize),
    /// The user asked to stop without starting anything.
    Quit,
}

/// Parse one line of the numbered menu. None means "ask again".
pub fn parse_menu_answer(answer: &str, count: usize) -> Option<MenuAnswer> {
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("q") || answer.eq_ignore_ascii_case("quit") {
        return Some(MenuAnswer::Quit);
    }
    let index: usize = answer.parse().ok()?;
    (1..=count)
        .contains(&index)
        .then(|| MenuAnswer::Index(index - 1))
}

/// A refusal whose process exit code is part of its contract.
#[derive(Debug)]
pub struct Refusal {
    code: &'static str,
    exit_code: i32,
    message: String,
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Refusal {}

/// The exit code a refusal contract asks for; everything else stays 1.
pub fn exit_code(error: &anyhow::Error) -> i32 {
    error
        .downcast_ref::<Refusal>()
        .map(Refusal::exit_code)
        .unwrap_or(1)
}

/// Refuse a bundle that declares several routes without naming one.
pub fn selection_required(candidates: &DerivationCandidates) -> anyhow::Error {
    let listed: Vec<String> = candidates
        .derivations
        .iter()
        .map(|candidate| format!("  {}", candidate.derivation_ref))
        .collect();
    Refusal {
        code: ERR_SELECTION_REQUIRED,
        exit_code: EXIT_NEEDS_EXPLICIT_DECISION,
        message: format!(
            "this portable Capsule declares {} derivations; select one with --derivation <sha256:...>; ato run never selects one implicitly:\n{}\nato run <capsule> --list-derivations prints the same candidates as JSON",
            candidates.derivations.len(),
            listed.join("\n")
        ),
    }
    .into()
}

/// Refuse a route this host cannot run, before anything is realized.
pub fn capability_unavailable(candidate: &DerivationCandidate, reference: &str) -> anyhow::Error {
    Refusal {
        code: ERR_CAPABILITY_UNAVAILABLE,
        exit_code: 1,
        message: format!(
            "{reference} cannot run {} on this host; required {}; the Run was not started",
            candidate.derivation_ref,
            candidate.required_environment()
        ),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples")
            .join(name)
    }

    fn native_multi() -> PortableApplicationBundle {
        let source = sample("native-desktop-multi-d");
        let (_, bundle) =
            ato_portable_application::build_authored_bundle_v2(&source).expect("the fixture packs");
        assert_eq!(bundle.index.derivations.len(), 3, "three declared routes");
        bundle
    }

    fn route(
        candidates: &DerivationCandidates,
        predicate: impl Fn(&DerivationCandidate) -> bool,
    ) -> &DerivationCandidate {
        candidates
            .derivations
            .iter()
            .find(|candidate| predicate(candidate))
            .expect("the declared route")
    }

    fn permission(kind: &'static str, detail: &str) -> CandidatePermission {
        CandidatePermission {
            kind,
            detail: detail.to_owned(),
        }
    }

    /// One declared route is not a choice, and a bundle with several routes
    /// never yields one of them.
    #[test]
    fn several_declared_routes_never_pick_one_implicitly() {
        let one = vec!["sha256:aa".to_owned()];
        assert_eq!(
            resolve_choice(&one, None),
            DerivationChoice::Only("sha256:aa".to_owned())
        );

        let several = vec!["sha256:aa".to_owned(), "sha256:bb".to_owned()];
        assert_eq!(
            resolve_choice(&several, None),
            DerivationChoice::NeedsSelection
        );

        // An explicitly named reference is used as named, declared or not: the
        // validator reports an undeclared one by name instead of substituting a
        // declared route.
        assert_eq!(
            resolve_choice(&several, Some("sha256:cc")),
            DerivationChoice::Explicit("sha256:cc".to_owned())
        );
        assert_eq!(resolve_choice(&[], None), DerivationChoice::NeedsSelection);
    }

    /// Every declared route appears once, in the bundle's canonical order.
    /// Admission is a live host observation: another probe may legitimately
    /// see a changed daemon or connector, without changing bundle identity.
    #[test]
    fn the_list_document_keeps_bundle_order_and_current_host_admission() {
        let bundle = native_multi();
        let first = candidates(&bundle, "sha256:fixture", Path::new("run")).expect("projected");
        assert_eq!(first.schema, LIST_SCHEMA);
        assert_eq!(first.bundle_sha256, "sha256:fixture");
        assert_eq!(first.contract_ref, bundle.index.root_contract_ref);
        assert_eq!(
            first
                .derivations
                .iter()
                .map(|candidate| candidate.derivation_ref.clone())
                .collect::<Vec<_>>(),
            bundle.index.derivations,
            "the document keeps the bundle's canonical order"
        );
        let encoded = serde_json::to_value(&first).expect("a serializable document");
        assert_eq!(encoded["schema"], LIST_SCHEMA);
        assert_eq!(encoded["derivations"].as_array().expect("a list").len(), 3);
        assert_eq!(encoded["host"]["os"], first.host.os);

        let web = route(&first, |candidate| {
            candidate.realization == Some("static_web")
        });
        assert_eq!(web.route, Some("Static Web"));
        assert!(web.runnable, "a static web route needs nothing from a host");
        assert_eq!(web.required_platform(), "none");

        // A Windows native route is fail-closed on every host: another OS has
        // no adapter for it, and Windows lacks the mandatory containment.
        let windows = route(&first, |candidate| {
            candidate.native_platform.as_deref() == Some("windows/x86_64")
        });
        assert_eq!(windows.realization, Some("native_pixel"));
        assert!(!windows.runnable);
        assert_eq!(
            windows.admission.as_ref().expect("a host verdict").state,
            CapabilityState::Unavailable
        );
    }

    /// The screen answers "which one, and what would it start" without running
    /// anything: name, realization, platform, tools, permissions, download and
    /// the measured host verdict with its reasons.
    #[test]
    fn the_screen_shows_every_fact_a_choice_depends_on() {
        let bundle = native_multi();
        let candidates =
            candidates(&bundle, "sha256:fixture", Path::new("run")).expect("projected");
        let screen = render_candidates(&candidates).join("\n");
        assert!(screen.contains("declares 3 derivations"), "{screen}");
        assert!(
            screen.contains("  1) Native desktop multi-derivation"),
            "{screen}"
        );
        assert!(
            screen.contains("  3) Native desktop multi-derivation"),
            "{screen}"
        );
        assert!(
            screen.contains("route: static_web (Static Web)"),
            "{screen}"
        );
        assert!(
            screen.contains("route: native_pixel (Native Desktop)"),
            "{screen}"
        );
        assert!(screen.contains("platform: windows/x86_64"), "{screen}");
        assert!(screen.contains("tools: "), "{screen}");
        // A macOS native route names the OS tools its mandatory policy needs;
        // another host has no native Adapter, which the admission states.
        #[cfg(target_os = "macos")]
        assert!(
            screen.contains("tools: codesign, sandbox-exec, spctl"),
            "{screen}"
        );
        assert!(
            screen.contains(
                "permissions: native: launch a native windows application on this desktop"
            ),
            "{screen}"
        );
        assert!(screen.contains("permissions: "), "{screen}");
        assert!(screen.contains("download: none declared"), "{screen}");
        assert!(screen.contains("host: runnable on this host"), "{screen}");
        assert!(
            screen.contains("host: not runnable on this host"),
            "{screen}"
        );
        assert!(
            screen.contains("reason: native_platform_mismatch - "),
            "{screen}"
        );
        assert!(screen.contains("derivation: sha256:"), "{screen}");
    }

    /// A refused route reuses the exact lines ato pack and a Hub Run preflight
    /// print, so the reason is named with one vocabulary everywhere.
    #[test]
    fn a_refused_route_uses_the_pack_capability_vocabulary() {
        let bundle = native_multi();
        let candidates =
            candidates(&bundle, "sha256:fixture", Path::new("run")).expect("projected");
        let windows = route(&candidates, |candidate| {
            candidate.native_platform.as_deref() == Some("windows/x86_64")
        });
        let lines = candidate_capability_lines(windows);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].starts_with(&format!(
                "capability derivation_ref={} route=Native Desktop platform=windows/x86_64 connector={} runnable=no state=Unavailable",
                windows.derivation_ref, windows.admission.as_ref().expect("host verdict").connector
            )),
            "{lines:?}"
        );
        let reason = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            "native_windows_containment_unavailable"
        } else {
            "native_platform_mismatch"
        };
        assert!(
            lines[1].starts_with(&format!("capability reason={reason} message=")),
            "{lines:?}"
        );
        let refusal = capability_unavailable(windows, "fixture.capsule");
        assert_eq!(exit_code(&refusal), 1);
        let message = format!("{refusal}");
        assert!(message.starts_with(ERR_CAPABILITY_UNAVAILABLE), "{message}");
        assert!(
            message.contains("required platform=none native_platform=windows/x86_64"),
            "{message}"
        );
        assert!(message.contains("the Run was not started"), "{message}");
    }

    /// Declared facts, not guesses: a container route that writes a state slot
    /// states both permissions, and the state mount is the declared one.
    #[test]
    fn declared_permissions_come_from_the_derivation() {
        let source = sample("oci-service-group-proof");
        let (_, bundle) =
            ato_portable_application::build_authored_bundle_v2(&source).expect("the fixture packs");
        let candidates =
            candidates(&bundle, "sha256:fixture", Path::new("run")).expect("projected");
        let only = &candidates.derivations[0];
        assert_eq!(only.realization, Some("oci"));
        assert!(only.requires.container);
        assert!(only.requires.state);
        assert_eq!(
            only.required_environment(),
            "platform=linux/amd64 native_platform=none container=true state=true"
        );
        assert!(
            only.permissions
                .iter()
                .any(|permission| permission.kind == "container"),
            "{:?}",
            only.permissions
        );
        assert!(
            only.permissions
                .iter()
                .any(|permission| permission.kind == "state"
                    && permission.detail.contains("/data (read-write)")),
            "{:?}",
            only.permissions
        );
        assert!(
            only.tools.iter().any(|tool| tool == "docker"),
            "{:?}",
            only.tools
        );
        let rendered = render_permissions(only).join("\n");
        assert!(rendered.contains("  - container: "), "{rendered}");
        assert!(rendered.contains("  - state: "), "{rendered}");
    }

    /// The confirmation is the price of the CLI choosing for you: never for an
    /// explicitly named route, never without declared permissions, and never
    /// when the person already accepted them.
    #[test]
    fn the_permission_confirmation_is_asked_only_for_a_menu_choice() {
        let mut candidate = DerivationCandidate {
            derivation_ref: "sha256:abc".to_owned(),
            display_name: Some("Fixture".to_owned()),
            realization: Some("oci"),
            route: Some("OCI Container"),
            platform: Some("linux/amd64".to_owned()),
            native_platform: None,
            requires: CandidateRequires {
                container: true,
                state: false,
            },
            permissions: vec![permission("container", "run it in a container")],
            tools: vec!["docker".to_owned()],
            download_bytes: Some(1536),
            runnable: true,
            admission: Some(CandidateAdmission {
                state: CapabilityState::Advertised,
                connector: "direct",
                reasons: Vec::new(),
            }),
            invalid: None,
            surfaces: vec!["main:app.http".to_owned()],
        };
        assert!(needs_permission_confirmation(true, false, &candidate));
        assert!(!needs_permission_confirmation(false, false, &candidate));
        assert!(!needs_permission_confirmation(true, true, &candidate));
        candidate.permissions.clear();
        assert!(!needs_permission_confirmation(true, false, &candidate));

        candidate.permissions = vec![permission("state", "write /data")];
        let screen = render_candidates(&DerivationCandidates {
            schema: LIST_SCHEMA,
            bundle_sha256: "sha256:fixture".to_owned(),
            contract_ref: "sha256:contract".to_owned(),
            host: CandidateHost {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
            },
            derivations: vec![candidate],
        })
        .join("\n");
        assert!(screen.contains("download: 1.5 KiB declared"), "{screen}");
        assert!(
            screen.contains("permissions: state: write /data"),
            "{screen}"
        );
        assert!(screen.contains("surfaces: main:app.http"), "{screen}");
    }

    #[test]
    fn menu_answers_are_bounded_and_quit_takes_no_route() {
        assert_eq!(parse_menu_answer("1", 3), Some(MenuAnswer::Index(0)));
        assert_eq!(parse_menu_answer(" 3 ", 3), Some(MenuAnswer::Index(2)));
        assert_eq!(parse_menu_answer("0", 3), None);
        assert_eq!(parse_menu_answer("4", 3), None);
        assert_eq!(parse_menu_answer("", 3), None);
        assert_eq!(parse_menu_answer("one", 3), None);
        assert_eq!(parse_menu_answer("q", 3), Some(MenuAnswer::Quit));
        assert_eq!(parse_menu_answer("QUIT", 3), Some(MenuAnswer::Quit));
    }

    /// The refusal that sends a caller to --derivation names every candidate and
    /// keeps the wording the CLI already used.
    #[test]
    fn an_ambiguous_bundle_names_every_candidate_and_the_exit_contract() {
        let bundle = native_multi();
        let candidates =
            candidates(&bundle, "sha256:fixture", Path::new("run")).expect("projected");
        let refusal = selection_required(&candidates);
        assert_eq!(exit_code(&refusal), 2);
        let message = format!("{refusal}");
        assert!(message.starts_with(ERR_SELECTION_REQUIRED), "{message}");
        assert!(
            message.contains("select one with --derivation <sha256:...>"),
            "{message}"
        );
        for candidate in &candidates.derivations {
            assert!(message.contains(&candidate.derivation_ref), "{message}");
        }
        assert!(message.contains("--list-derivations"), "{message}");
    }

    /// A route that does not validate is reported as a format failure, not as a
    /// host verdict, and it never sends the Run to another declared route.
    #[test]
    fn an_unvalidated_route_is_reported_as_a_format_failure() {
        let failure = unvalidated("sha256:broken", "the route failed validation".to_owned());
        assert!(!failure.runnable);
        assert!(failure.admission.is_none());
        assert_eq!(
            failure.invalid.as_deref(),
            Some("the route failed validation")
        );
        let mut candidates = DerivationCandidates {
            schema: LIST_SCHEMA,
            bundle_sha256: "sha256:fixture".to_owned(),
            contract_ref: "sha256:contract".to_owned(),
            host: CandidateHost {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
            },
            derivations: vec![failure],
        };
        let screen = render_candidates(&candidates).join("\n");
        assert!(screen.contains("route: not validated"), "{screen}");
        assert!(screen.contains("host: not validated"), "{screen}");
        assert!(
            screen.contains("invalid: the route failed validation"),
            "{screen}"
        );
        assert!(candidate_capability_lines(&candidates.derivations[0]).is_empty());
        candidates.derivations.clear();
    }
}
