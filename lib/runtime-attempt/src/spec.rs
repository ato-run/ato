//! What an attempt verifies, independent of how the candidate came to be.
//!
//! A Formation attempt plans a candidate from source (`PlannedCandidate`);
//! a Run starts from a validated `.capsule`. Both are the same attempt to
//! the Runtime: a frozen K, a Derivation, and the facts the Contract's
//! observations need. Nothing here asks for a `ProgramIntent`, a build
//! plan or source detection — those belong to the executor that builds.

use std::collections::BTreeMap;

use ato_formation::authoring::{BoundContract, BoundDerivation};
use ato_formation::scenario::ScenarioV1;

/// The shape of the running candidate, for the checks that depend on it
/// (a browser Contract needs a process candidate today).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateShape {
    /// A process that serves its ports.
    Process,
    /// Files served to a browser.
    StaticWeb,
}

/// One Scenario a Runtime must execute, paired with the Contract requirement
/// that names it.
///
/// The requirement id names the transcript sidecar and the verdict; the object
/// is content-addressed by the Contract, and `scenario_ref` is the reference K
/// pins. The port selects which of the candidate's realized endpoints the
/// execution talks to — the verifier never constructs an address of its own.
#[derive(Debug, Clone)]
pub struct AttemptScenarioV1 {
    pub requirement_id: String,
    pub port: String,
    pub scenario_ref: String,
    pub scenario: ScenarioV1,
}

/// The attempt a Runtime is asked to make: which K, which D.
#[derive(Debug, Clone)]
pub struct AttemptSpec<'a> {
    /// The frozen base Contract the receipt covers.
    pub contract: &'a BoundContract,
    pub contract_ref: &'a str,
    pub derivation: &'a BoundDerivation,
    pub derivation_ref: &'a str,
    pub shape: CandidateShape,
    /// The resolved identity of each input, for input-identity
    /// observations: the workspace tree the candidate runs from.
    pub input_refs: BTreeMap<String, String>,
    /// A portable Instance snapshot restored before the candidate started,
    /// for snapshot observations.
    pub instance_snapshot_ref: Option<String>,
    /// The Contract's scenario requirements this attempt can execute. Empty
    /// where the caller cannot resolve scenario objects (Formation, hosted):
    /// a scenario requirement then fails closed as unobserved.
    pub scenarios: Vec<AttemptScenarioV1>,
}
