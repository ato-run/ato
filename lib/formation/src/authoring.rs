//! The canonical authoring compiler: drafts in, addressable Contract and
//! Derivation out.
//!
//! ## What a Capsule is here
//!
//! A Capsule's identity is its Contract `K` — the author-chosen observable
//! conditions that decide whether a future computation counts as the same
//! resumable point. `ContractRef = H(canonical(bound K))`. A Derivation is an
//! executable route that attempts to produce a continuation satisfying `K`, and
//! is separately addressed: `DerivationRef = H(canonical(bound D))`. One
//! Capsule, many Derivations.
//!
//! ## Two frontends, one compiler
//!
//! `capsule.toml` and App Presets are two ways of writing the same pair. A
//! Preset is a synthesizer, not a lane: it produces a `ContractDraft` and a
//! `DerivationDraft` for a source simple enough to describe without help, and
//! from here on nothing can tell which door a draft came through. That is the
//! point — an author who writes out by hand exactly what the Preset would have
//! synthesized gets the SAME `ContractRef` and the SAME `DerivationRef`.
//!
//! ## What must never enter a digest
//!
//! Provenance. Whether a draft was synthesized by a Preset, typed by a person
//! or produced by a model; which file it came from; when; on whose behalf; from
//! which repository; through which host. None of it is what the author chose to
//! preserve, and all of it would split one Capsule into many that mean the same
//! thing. It travels beside the digest, in `AuthoringProvenance`, and is
//! deliberately not serialized into either bound object.
//!
//! ## Binding
//!
//! A draft is not addressable. It names a workspace by path and may ask for an
//! observation to be *captured* rather than stated. Binding resolves both: paths
//! become content addresses, `capture` becomes the concrete value observed at
//! seal time. Only then is there something to hash.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ─────────────────────────────────────────────────────────── protocol vocabulary
//
// Every identifier below already exists in this workspace. None is minted for
// Step 10: a fictional protocol id is a promise that an adapter honours it, and
// nothing here can honour a promise it invented.

/// The workspace an input names. `lib/objects`, `ato-adapter-workspace`.
pub const WORKSPACE_PROTOCOL: &str = "ato.workspace@1";
/// Process execution. `ato-adapter-process`.
pub const PROCESS_PROTOCOL: &str = "ato.process@1";
/// A surface the browser evaluates — what a Static Compute is.
pub const BROWSER_PROTOCOL: &str = "ato.browser@1";
/// An exported HTTP port. `ato-adapter-http`.
pub const HTTP_PROTOCOL: &str = "ato.http@1";
/// Writable continuation state, filesystem-shaped. Already the protocol every
/// Formation `state_slot_declaration` carries.
pub const STATE_FILESYSTEM_PROTOCOL: &str = "ato.state.filesystem@1";
/// The HTTP acceptance verifier. `extensions/contracts`.
pub const HTTP_CONTRACT_VERIFIER: &str = "ato.contract.http@1";
/// The workspace-content acceptance verifier. `extensions/contracts`.
pub const WORKSPACE_CONTRACT_VERIFIER: &str = "ato.contract.workspace@1";

pub const BOUND_CONTRACT_SCHEMA: &str = "ato.contract/1";
pub const BOUND_DERIVATION_SCHEMA: &str = "ato.derivation/1";

// ────────────────────────────────────────────────────────────────────── errors

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthoringError {
    #[error("{field} is malformed: {detail}")]
    Malformed { field: String, detail: String },
    #[error("{what} {id:?} is declared twice")]
    Duplicate { what: &'static str, id: String },
    #[error("{referrer} names {what} {id:?}, which is not declared")]
    Unresolved {
        referrer: String,
        what: &'static str,
        id: String,
    },
    #[error("{field} asks for a captured observation this build cannot resolve: {detail}")]
    CaptureUnresolvable { field: String, detail: String },
    #[error("canonicalization failed: {detail}")]
    Canonicalization { detail: String },
}

impl AuthoringError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed { .. } => "authoring_malformed",
            Self::Duplicate { .. } => "authoring_duplicate_id",
            Self::Unresolved { .. } => "authoring_unresolved_reference",
            Self::CaptureUnresolvable { .. } => "authoring_capture_unresolvable",
            Self::Canonicalization { .. } => "authoring_canonicalization_failed",
        }
    }
}

pub(crate) fn malformed(field: impl Into<String>, detail: impl Into<String>) -> AuthoringError {
    AuthoringError::Malformed {
        field: field.into(),
        detail: detail.into(),
    }
}

// ──────────────────────────────────────────────────────────────────── the draft

/// An observation the author asked for but did not state: resolved at seal time
/// into a concrete value that becomes part of `K`.
///
/// This is what lets one grammar express both "any usable HTTP app" and "this
/// exact content", without Ato deciding which the author meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed<T> {
    Capture,
    Stated(T),
}

/// How an author names an input's identity requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputIdentityRequirement {
    pub input: String,
    pub digest: Observed<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequirement {
    pub port: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    /// A literal digest only. `capture` is refused — see `bind`.
    pub body_digest: Option<Observed<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementDraft {
    Http(HttpRequirement),
    InputIdentity(InputIdentityRequirement),
}

/// One condition that must hold for a continuation to be this Capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationDraft {
    pub id: String,
    pub requirement: RequirementDraft,
}

/// The author's proposed identity boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContractDraft {
    pub requirements: Vec<ObservationDraft>,
}

/// An immutable object the route consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDraft {
    pub id: String,
    pub protocol: String,
    /// Relative to the source root. `"."` is the whole tree.
    pub path: String,
}

/// One typed action in the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepDraft {
    pub id: String,
    pub protocol: String,
    pub op: String,
    /// `ato.process@1` — the argv, exactly as authored. Never inferred.
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    /// `ato.browser@1` — the input whose tree is served.
    pub source: Option<String>,
    /// The subtree that is served, relative to the input. `None` is the root.
    pub root: Option<String>,
    pub entry: Option<String>,
    pub spa_fallback: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDraft {
    pub id: String,
    pub protocol: String,
    pub from: String,
    pub guest_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDraft {
    pub id: String,
    pub protocol: String,
    pub mount: String,
    pub access: StateAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateAccess {
    ReadOnly,
    ReadWrite,
}

/// A runtime this route needs provisioned, at an exact version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDraft {
    pub name: String,
    pub version: String,
}

/// What re-executing this route does to the world OUTSIDE the continuation.
///
/// Deliberately separate from `K`, and deliberately not consulted by anything
/// yet. Two executions can both satisfy `K` while charging a card twice —
/// "produces a K-equivalent continuation" and "is safe to run again" are
/// different claims, and collapsing them is how a planner learns to re-send
/// somebody's email. v0 carries the declaration so a planner can be written
/// against it later; it never infers one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EffectClass {
    /// No effect outside the continuation. The default a build with no external
    /// effects declares by saying nothing.
    #[default]
    Pure,
    Idempotent,
    RecordSubstitutable,
    RequiresConfirmation,
    NonRepeatable,
}

/// The proposed route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivationDraft {
    pub inputs: Vec<InputDraft>,
    pub runtimes: Vec<RuntimeDraft>,
    pub steps: Vec<StepDraft>,
    pub ports: Vec<PortDraft>,
    pub state: Vec<StateDraft>,
    /// The route needs the workspace built before it is served, into this
    /// subtree.
    ///
    /// Synthesized by `node-static/v1` and, in v0, by nothing else: expressing
    /// a package-manager build as explicit `ato.process@1` steps needs the
    /// resolver that picks the package manager and the Node version, which
    /// still lives in the execution projection. Carried in the digest because
    /// it changes what the route does, and deliberately not yet authorable —
    /// see the projection's `unsupported_step` refusal.
    pub workspace_build: Option<String>,
    pub effects: EffectClass,
}

/// Where a draft came from. **Never digested.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthoringProvenance {
    /// An author wrote `capsule.toml`.
    Authored,
    /// A Preset synthesized it because no `capsule.toml` existed.
    PresetSynthesized { preset: &'static str },
}

/// What either frontend produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringDraft {
    pub contract: ContractDraft,
    pub derivation: DerivationDraft,
    /// Carried alongside so a person can be told where this came from, and
    /// excluded from both digests so it cannot split one Capsule into two.
    pub provenance: AuthoringProvenance,
}

// ────────────────────────────────────────────────────────────── bound + digests

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundRequirement {
    pub id: String,
    /// The acceptance verifier that decides this condition.
    pub verifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// The Capsule's identity, once every captured observation is a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundContract {
    pub schema: String,
    /// Sorted by id. A contract is a SET of conditions; the order somebody
    /// happened to type them in is not part of what they chose to preserve.
    pub requirements: Vec<BoundRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundInput {
    pub id: String,
    pub protocol: String,
    /// The resolved immutable reference. Two different trees are two different
    /// Derivations, which is the whole reason binding happens before hashing.
    pub content_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundStep {
    pub id: String,
    pub protocol: String,
    pub op: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spa_fallback: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundPort {
    pub id: String,
    pub protocol: String,
    pub from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundState {
    pub id: String,
    pub protocol: String,
    pub mount: String,
    pub access: StateAccess,
}

/// One executable route, addressable and reusable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundDerivation {
    pub schema: String,
    pub inputs: Vec<BoundInput>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub runtimes: BTreeMap<String, String>,
    /// Steps keep AUTHORED ORDER. A route is a sequence; sorting it would
    /// change what it does.
    pub steps: Vec<BoundStep>,
    pub ports: Vec<BoundPort>,
    pub state: Vec<BoundState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_build: Option<String>,
    pub effects: EffectClass,
}

fn digest_of<T: Serialize>(value: &T) -> Result<String, AuthoringError> {
    let bytes = serde_jcs::to_vec(value).map_err(|error| AuthoringError::Canonicalization {
        detail: error.to_string(),
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(&bytes)))
}

impl BoundContract {
    /// `CapsuleId`. The canonical Contract and nothing else — no route, no
    /// source, no host, no time, no actor.
    pub fn contract_ref(&self) -> Result<String, AuthoringError> {
        digest_of(self)
    }
}

impl BoundDerivation {
    pub fn derivation_ref(&self) -> Result<String, AuthoringError> {
        digest_of(self)
    }
}

/// What the seal-time environment can answer.
///
/// Small on purpose: everything here is something the Formation worker already
/// holds when it finishes acquiring a source. A binding context that needed a
/// running process would move binding after execution, and then a Derivation
/// could not be addressed before being run.
pub struct BindingContext<'a> {
    /// The resolved source closure of the tree this build was handed.
    pub source_closure_ref: &'a str,
}

/// Resolve a draft into the two addressable objects.
pub fn bind(
    draft: &AuthoringDraft,
    context: &BindingContext<'_>,
) -> Result<(BoundContract, BoundDerivation), AuthoringError> {
    let derivation = bind_derivation(&draft.derivation, context)?;
    let known_inputs: BTreeSet<&str> = derivation.inputs.iter().map(|i| i.id.as_str()).collect();
    let known_ports: BTreeSet<&str> = derivation.ports.iter().map(|p| p.id.as_str()).collect();
    let contract = bind_contract(&draft.contract, &derivation, &known_inputs, &known_ports)?;
    Ok((contract, derivation))
}

fn bind_derivation(
    draft: &DerivationDraft,
    context: &BindingContext<'_>,
) -> Result<BoundDerivation, AuthoringError> {
    let mut seen = BTreeSet::new();
    let mut inputs = Vec::with_capacity(draft.inputs.len());
    for input in &draft.inputs {
        if !seen.insert(input.id.clone()) {
            return Err(AuthoringError::Duplicate {
                what: "input",
                id: input.id.clone(),
            });
        }
        // v0 resolves exactly one kind of input: the source tree this build was
        // handed, whose closure the worker has already verified. A subtree
        // input would need its own closure and is refused rather than bound to
        // the wrong digest.
        if input.protocol != WORKSPACE_PROTOCOL {
            return Err(malformed(
                format!("input.{}", input.id),
                format!("this build resolves only {WORKSPACE_PROTOCOL} inputs"),
            ));
        }
        if input.path != "." {
            return Err(malformed(
                format!("input.{}.path", input.id),
                "this build resolves only the source root \".\"",
            ));
        }
        inputs.push(BoundInput {
            id: input.id.clone(),
            protocol: input.protocol.clone(),
            content_ref: context.source_closure_ref.to_owned(),
        });
    }
    inputs.sort_by(|a, b| a.id.cmp(&b.id));

    let mut runtimes = BTreeMap::new();
    for runtime in &draft.runtimes {
        if runtimes
            .insert(runtime.name.clone(), runtime.version.clone())
            .is_some()
        {
            return Err(AuthoringError::Duplicate {
                what: "runtime",
                id: runtime.name.clone(),
            });
        }
    }

    let mut step_ids = BTreeSet::new();
    let mut steps = Vec::with_capacity(draft.steps.len());
    for step in &draft.steps {
        if !step_ids.insert(step.id.clone()) {
            return Err(AuthoringError::Duplicate {
                what: "derive.step",
                id: step.id.clone(),
            });
        }
        if let Some(source) = &step.source
            && !inputs.iter().any(|input| &input.id == source)
        {
            return Err(AuthoringError::Unresolved {
                referrer: format!("derive.step.{}", step.id),
                what: "input",
                id: source.clone(),
            });
        }
        steps.push(BoundStep {
            id: step.id.clone(),
            protocol: step.protocol.clone(),
            op: step.op.clone(),
            argv: step.argv.clone(),
            cwd: step.cwd.clone(),
            env: step.env.clone(),
            source: step.source.clone(),
            root: step.root.clone(),
            entry: step.entry.clone(),
            spa_fallback: step.spa_fallback,
        });
    }

    let mut port_ids = BTreeSet::new();
    let mut ports = Vec::with_capacity(draft.ports.len());
    for port in &draft.ports {
        if !port_ids.insert(port.id.clone()) {
            return Err(AuthoringError::Duplicate {
                what: "port",
                id: port.id.clone(),
            });
        }
        if !step_ids.contains(&port.from) {
            return Err(AuthoringError::Unresolved {
                referrer: format!("port.{}", port.id),
                what: "derive.step",
                id: port.from.clone(),
            });
        }
        ports.push(BoundPort {
            id: port.id.clone(),
            protocol: port.protocol.clone(),
            from: port.from.clone(),
            guest_port: port.guest_port,
        });
    }
    ports.sort_by(|a, b| a.id.cmp(&b.id));

    let mut state_ids = BTreeSet::new();
    let mut state = Vec::with_capacity(draft.state.len());
    for slot in &draft.state {
        if !state_ids.insert(slot.id.clone()) {
            return Err(AuthoringError::Duplicate {
                what: "state",
                id: slot.id.clone(),
            });
        }
        if !slot.mount.starts_with('/')
            || slot.mount.contains("/../")
            || slot.mount.ends_with("/..")
        {
            return Err(malformed(
                format!("state.{}.mount", slot.id),
                format!(
                    "{:?} is not an absolute, traversal-free guest path",
                    slot.mount
                ),
            ));
        }
        state.push(BoundState {
            id: slot.id.clone(),
            protocol: slot.protocol.clone(),
            mount: slot.mount.clone(),
            access: slot.access,
        });
    }
    state.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(BoundDerivation {
        schema: BOUND_DERIVATION_SCHEMA.to_owned(),
        inputs,
        runtimes,
        steps,
        ports,
        state,
        workspace_build: draft.workspace_build.clone(),
        effects: draft.effects,
    })
}

fn bind_contract(
    draft: &ContractDraft,
    derivation: &BoundDerivation,
    known_inputs: &BTreeSet<&str>,
    known_ports: &BTreeSet<&str>,
) -> Result<BoundContract, AuthoringError> {
    let mut ids = BTreeSet::new();
    let mut requirements = Vec::with_capacity(draft.requirements.len());
    for observation in &draft.requirements {
        if !ids.insert(observation.id.clone()) {
            return Err(AuthoringError::Duplicate {
                what: "contract.require",
                id: observation.id.clone(),
            });
        }
        let bound = match &observation.requirement {
            RequirementDraft::Http(http) => {
                if !known_ports.contains(http.port.as_str()) {
                    return Err(AuthoringError::Unresolved {
                        referrer: format!("contract.require.{}", observation.id),
                        what: "port",
                        id: http.port.clone(),
                    });
                }
                let body_digest = match &http.body_digest {
                    None => None,
                    Some(Observed::Stated(value)) => Some(value.clone()),
                    // Resolving this needs a live response from the running
                    // continuation, and Formation does not run one: the process
                    // starts on a Runner, later. Accepting it and hashing
                    // nothing would mint a Capsule identity that claims to
                    // observe content nobody looked at.
                    Some(Observed::Capture) => {
                        return Err(AuthoringError::CaptureUnresolvable {
                            field: format!(
                                "contract.require.{}.expect.body_digest",
                                observation.id
                            ),
                            detail:
                                "a response body can only be captured from a running continuation, \
                                 which this build does not have; state the digest, or observe the \
                                 input's identity instead"
                                    .to_owned(),
                        });
                    }
                };
                BoundRequirement {
                    id: observation.id.clone(),
                    verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
                    port: Some(http.port.clone()),
                    method: Some(http.method.clone()),
                    path: Some(http.path.clone()),
                    status: Some(http.status),
                    body_digest,
                    input: None,
                    digest: None,
                }
            }
            RequirementDraft::InputIdentity(identity) => {
                if !known_inputs.contains(identity.input.as_str()) {
                    return Err(AuthoringError::Unresolved {
                        referrer: format!("contract.require.{}", observation.id),
                        what: "input",
                        id: identity.input.clone(),
                    });
                }
                let digest = match &identity.digest {
                    Observed::Stated(value) => value.clone(),
                    // Resolvable, and this is the one the Preset uses: the tree
                    // is right here and its closure has already been verified.
                    Observed::Capture => derivation
                        .inputs
                        .iter()
                        .find(|input| input.id == identity.input)
                        .map(|input| input.content_ref.clone())
                        .expect("the input was just checked to exist"),
                };
                BoundRequirement {
                    id: observation.id.clone(),
                    verifier: WORKSPACE_CONTRACT_VERIFIER.to_owned(),
                    port: None,
                    method: None,
                    path: None,
                    status: None,
                    body_digest: None,
                    input: Some(identity.input.clone()),
                    digest: Some(digest),
                }
            }
        };
        requirements.push(bound);
    }
    requirements.sort_by(|a, b| a.id.cmp(&b.id));

    if requirements.is_empty() {
        // A Capsule whose contract observes nothing is satisfied by every
        // continuation in existence, including the ones that do not work.
        return Err(malformed(
            "contract",
            "a Capsule identified by no observation is satisfied by anything; \
             state at least one condition",
        ));
    }

    Ok(BoundContract {
        schema: BOUND_CONTRACT_SCHEMA.to_owned(),
        requirements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> BindingContext<'static> {
        BindingContext {
            source_closure_ref: "sha256:aa",
        }
    }

    fn minimal(provenance: AuthoringProvenance) -> AuthoringDraft {
        AuthoringDraft {
            contract: ContractDraft {
                requirements: vec![ObservationDraft {
                    id: "root".to_owned(),
                    requirement: RequirementDraft::Http(HttpRequirement {
                        port: "app.http".to_owned(),
                        method: "GET".to_owned(),
                        path: "/".to_owned(),
                        status: 200,
                        body_digest: None,
                    }),
                }],
            },
            derivation: DerivationDraft {
                inputs: vec![InputDraft {
                    id: "workspace".to_owned(),
                    protocol: WORKSPACE_PROTOCOL.to_owned(),
                    path: ".".to_owned(),
                }],
                runtimes: vec![],
                steps: vec![StepDraft {
                    id: "site".to_owned(),
                    protocol: BROWSER_PROTOCOL.to_owned(),
                    op: "serve".to_owned(),
                    argv: vec![],
                    cwd: String::new(),
                    env: BTreeMap::new(),
                    source: Some("workspace".to_owned()),
                    root: None,
                    entry: Some("index.html".to_owned()),
                    spa_fallback: Some(true),
                }],
                ports: vec![PortDraft {
                    id: "app.http".to_owned(),
                    protocol: HTTP_PROTOCOL.to_owned(),
                    from: "site".to_owned(),
                    guest_port: None,
                }],
                state: vec![],
                workspace_build: None,
                effects: EffectClass::Pure,
            },
            provenance,
        }
    }

    #[test]
    fn provenance_is_outside_both_digests() {
        // The invariant the whole Preset unification rests on. A Preset-
        // synthesized draft and a hand-written one that say the same thing are
        // the same Capsule and the same route; recording WHERE they came from
        // must not make them two.
        let (ka, da) = bind(&minimal(AuthoringProvenance::Authored), &ctx()).expect("binds");
        let (kb, db) = bind(
            &minimal(AuthoringProvenance::PresetSynthesized {
                preset: "single-html/v1",
            }),
            &ctx(),
        )
        .expect("binds");
        assert_eq!(ka.contract_ref().unwrap(), kb.contract_ref().unwrap());
        assert_eq!(da.derivation_ref().unwrap(), db.derivation_ref().unwrap());
    }

    #[test]
    fn a_different_source_is_a_different_derivation_and_the_same_contract() {
        // The weak-contract case: an author who observes only "GET / is 200"
        // has said two different sources are the same resumable point. The
        // ROUTE still differs, and is addressed separately — which is exactly
        // what makes one Capsule able to have many Derivations.
        let draft = minimal(AuthoringProvenance::Authored);
        let (k1, d1) = bind(&draft, &ctx()).expect("binds");
        let (k2, d2) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: "sha256:bb",
            },
        )
        .expect("binds");
        assert_eq!(k1.contract_ref().unwrap(), k2.contract_ref().unwrap());
        assert_ne!(d1.derivation_ref().unwrap(), d2.derivation_ref().unwrap());
    }

    #[test]
    fn a_captured_input_identity_makes_the_source_part_of_the_capsule() {
        let mut draft = minimal(AuthoringProvenance::Authored);
        draft.contract.requirements.push(ObservationDraft {
            id: "source-identity".to_owned(),
            requirement: RequirementDraft::InputIdentity(InputIdentityRequirement {
                input: "workspace".to_owned(),
                digest: Observed::Capture,
            }),
        });
        let (k1, _) = bind(&draft, &ctx()).expect("binds");
        let (k2, _) = bind(
            &draft,
            &BindingContext {
                source_closure_ref: "sha256:bb",
            },
        )
        .expect("binds");
        assert_ne!(k1.contract_ref().unwrap(), k2.contract_ref().unwrap());
    }

    #[test]
    fn the_order_conditions_were_typed_in_is_not_part_of_the_contract() {
        let mut forward = minimal(AuthoringProvenance::Authored);
        forward.contract.requirements.push(ObservationDraft {
            id: "source-identity".to_owned(),
            requirement: RequirementDraft::InputIdentity(InputIdentityRequirement {
                input: "workspace".to_owned(),
                digest: Observed::Capture,
            }),
        });
        let mut reversed = forward.clone();
        reversed.contract.requirements.reverse();
        assert_eq!(
            bind(&forward, &ctx()).unwrap().0.contract_ref().unwrap(),
            bind(&reversed, &ctx()).unwrap().0.contract_ref().unwrap(),
        );
    }

    #[test]
    fn a_contract_that_observes_nothing_is_refused() {
        let mut draft = minimal(AuthoringProvenance::Authored);
        draft.contract.requirements.clear();
        assert_eq!(
            bind(&draft, &ctx()).unwrap_err().code(),
            "authoring_malformed"
        );
    }

    #[test]
    fn a_captured_body_digest_is_refused_rather_than_silently_dropped() {
        let mut draft = minimal(AuthoringProvenance::Authored);
        draft.contract.requirements[0].requirement = RequirementDraft::Http(HttpRequirement {
            port: "app.http".to_owned(),
            method: "GET".to_owned(),
            path: "/".to_owned(),
            status: 200,
            body_digest: Some(Observed::Capture),
        });
        assert_eq!(
            bind(&draft, &ctx()).unwrap_err().code(),
            "authoring_capture_unresolvable"
        );
    }

    #[test]
    fn an_observation_of_a_port_no_step_exports_is_refused() {
        let mut draft = minimal(AuthoringProvenance::Authored);
        draft.derivation.ports.clear();
        let error = bind(&draft, &ctx()).unwrap_err();
        assert_eq!(error.code(), "authoring_unresolved_reference");
    }

    #[test]
    fn a_route_is_a_sequence_and_keeps_its_order() {
        let mut draft = minimal(AuthoringProvenance::Authored);
        let second = StepDraft {
            id: "aaa-runs-second".to_owned(),
            protocol: PROCESS_PROTOCOL.to_owned(),
            op: "exec".to_owned(),
            argv: vec!["true".to_owned()],
            cwd: String::new(),
            env: BTreeMap::new(),
            source: None,
            root: None,
            entry: None,
            spa_fallback: None,
        };
        draft.derivation.steps.push(second);
        let (_, bound) = bind(&draft, &ctx()).expect("binds");
        assert_eq!(
            bound
                .steps
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["site", "aaa-runs-second"],
            "sorting a route would change what it does"
        );
    }

    #[test]
    fn a_traversing_state_mount_is_refused() {
        let mut draft = minimal(AuthoringProvenance::Authored);
        draft.derivation.state.push(StateDraft {
            id: "app_data".to_owned(),
            protocol: STATE_FILESYSTEM_PROTOCOL.to_owned(),
            mount: "/data/../etc".to_owned(),
            access: StateAccess::ReadWrite,
        });
        assert_eq!(
            bind(&draft, &ctx()).unwrap_err().code(),
            "authoring_malformed"
        );
    }
}
