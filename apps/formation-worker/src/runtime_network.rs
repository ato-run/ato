//! Runtime Network Phase 1 — the Runtime side and the requester side.
//!
//! ```text
//! requester                       coordinator (ato-api)            Runtime (this worker)
//!   freeze I, plan each D   ──▶   candidates = D × environments
//!   SatisfyRequest               hard filter, rank, ticket   ──▶   claim ticket
//!                                                                   frozen I → build → temporary
//!                                                                   realization → HTTP K → browser K
//!                                attempt history, fallback   ◀──   result + receipts
//!                                VerifiedRoute on pass
//! ```
//!
//! The requester computes every identity (Contract, effective Contract,
//! Derivation) and every requirement from the Derivation itself, because the
//! canonical forms live here and nowhere else. The coordinator only matches
//! typed facts, and treats the requester's effects and requirements as
//! scheduling hints. The authority is the Runtime: before anything runs it
//! plans the ticket's route from the ticket's archive, refuses a ticket whose
//! refs, environment, effects or platform do not hold, and attests what it
//! established. The coordinator decides fallback from that attestation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ato_formation::authoring::AuthoringDraft;
use ato_formation::browser::{BrowserContractV0, effective_contract_ref};
use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::request::AttemptStatus;
use ato_formation::source::{SourceError, SourceLimits};
use serde::{Deserialize, Serialize};

use crate::attempt::{AttemptRequest, Continuation, ReceiptContext, run_reserved_attempt};
use crate::browser_verify::{BrowserVerification, BrowserVerifierCommand};
use crate::executor::LocalAttemptExecutor;
use crate::job::{PlannedCandidate, digest, plan_candidate};
use crate::journal::{AttemptJournal, AttemptLedger, AttemptPermit, AttemptRecordState};
use crate::local::{self, host_triple};
use crate::sandbox::{BuildLimits, NetworkPolicy, TOOLCHAIN_ROOT, containment_available};
use ato_formation::source::FileVerifiedArchive;
use ato_runtime_attempt::admission::EffectAuthorization;
use ato_runtime_attempt::formation_realizer::FormationRealizer;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, Write};

pub const PROTOCOL: &str = "ato.runtime-network/0";
/// The one execution environment a host advertises in Phase 1: itself.
pub const NATIVE_ENVIRONMENT: &str = "native";
/// Largest source archive a request carries inline.
pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

/// Search budget ceilings (ADR-031). Each is a limit on a whole search, spent
/// across all of its requests — not on one HTTP request, one artifact or one
/// Runtime's disk. The Coordinator holds the same values; both repos check
/// them against `search-budget-ceilings.json`.
pub const MAX_SEARCH_ATTEMPTS: u32 = 32;
pub const MAX_SEARCH_DEADLINE_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const MAX_SEARCH_TRANSFER_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const MAX_SEARCH_EXPANDED_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const MAX_SEARCH_STORED_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// The most one attempt expands of its source: this Runtime's own source
/// ceiling, `SourceLimits::default().max_total_bytes`. The Coordinator never
/// reserves more than this for an attempt.
pub const MAX_ATTEMPT_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
/// The most one attempt keeps: the ceiling of one capsule bundle. An
/// UNKNOWN attempt is charged its full caps, so an attempt's share is
/// bounded here rather than by everything the search has left.
pub const MAX_ATTEMPT_STORED_BYTES: u64 = 512 * 1024 * 1024;
/// How long a Runtime may take to fetch a ticket's source: MAX_SOURCE_BYTES
/// at about 40 KB/s.
const SOURCE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

// ─────────────────────────────────────────────────────────────── wire types

/// A requirement a Derivation places on a Runtime, as a typed fact.
/// `one_of: None` means the fact must merely be present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub fact: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentAdvert {
    pub environment_id: String,
    pub facts: BTreeMap<String, String>,
}

/// What a Runtime says it is. Stable: no load, no free memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDescriptorAdvert {
    pub protocol: String,
    pub agent_version: String,
    pub environments: Vec<EnvironmentAdvert>,
}

/// How a Runtime is doing right now. Mutable, separate from identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityReport {
    pub capacity: u32,
    pub current_slots: u32,
    pub health: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedDerivation {
    pub derivation_ref: String,
    /// The authored route, verbatim. The Runtime re-plans it and must reach
    /// the same `derivation_ref`.
    pub capsule_toml: String,
    /// `pure`, `idempotent`, … — decides whether a failed attempt may fall
    /// back to another Runtime.
    pub effects: String,
    pub requirements: Vec<Requirement>,
    /// Toolchain facts the route provisions when a Runtime lacks them.
    pub provisions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceTransport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_bytes: Option<u64>,
    pub archive_digest: String,
    pub closure_ref: String,
}

/// Read-only compatibility codec for already persisted v0 source tickets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacySourceTicket {
    pub attempt_id: String,
    pub fence: u32,
    pub satisfy_id: String,
    pub runtime_id: String,
    pub environment_id: String,
    pub contract_ref: String,
    pub base_contract_ref: String,
    pub derivation_ref: String,
    pub capsule_toml: String,
    #[serde(default)]
    pub browser_contract: Option<BrowserContractV0>,
    pub archive_digest: String,
    pub bindings: BTreeMap<String, String>,
    pub network: String,
    /// This attempt's caps. A ticket without them is not one this Runtime
    /// runs: it would have nothing to hold the attempt to.
    pub resource_budget: TicketResourceBudget,
}

impl AttemptTicket {
    pub fn from_wire(mut value: serde_json::Value) -> Result<Self> {
        if value.get("input").is_none() {
            let old: LegacySourceTicket = serde_json::from_value(value.clone())?;
            let map = value.as_object_mut().context("ticket object required")?;
            map.remove("capsule_toml");
            map.remove("archive_digest");
            map.insert("input".into(),serde_json::json!({"kind":"source","capsule_toml":old.capsule_toml,"archive_digest":old.archive_digest}));
        }
        Ok(serde_json::from_value(value)?)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeConstraintWire {
    Any,
    Exact {
        runtime_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatisfyPolicy {
    /// `denied` or `dependency-resolution` — what a build may reach.
    pub network: String,
    /// May `ato_managed` Runtimes be used for this request?
    pub allow_managed: bool,
}

/// The budget of the whole search the request belongs to (ADR-031). The
/// search's first request freezes it; every later request of the search must
/// state it exactly, and spends from what the earlier ones left. The
/// Coordinator owns what remains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatisfyBudget {
    /// Attempts the search may claim, over all of its requests.
    pub max_attempts: u32,
    /// `first_pass`: stop at the first verified route. `all`: attempt every
    /// admissible candidate (still within the budget). A search objective,
    /// frozen with the budget.
    pub mode: String,
    /// The search's lifetime, from its first request. Never extended.
    pub deadline_seconds: u64,
    /// Logical source bytes the search may hand to Runtimes.
    pub max_transfer_bytes: u64,
    /// Logical bytes the search may expand on Runtimes.
    pub max_expanded_bytes: u64,
    /// Logical artifact bytes the search may have Runtimes keep.
    pub max_stored_bytes: u64,
}

impl SatisfyBudget {
    /// The largest budget a search may have: every ceiling.
    pub fn ceilings(max_attempts: u32, mode: &str) -> Self {
        Self {
            max_attempts,
            mode: mode.to_owned(),
            deadline_seconds: MAX_SEARCH_DEADLINE_SECONDS,
            max_transfer_bytes: MAX_SEARCH_TRANSFER_BYTES,
            max_expanded_bytes: MAX_SEARCH_EXPANDED_BYTES,
            max_stored_bytes: MAX_SEARCH_STORED_BYTES,
        }
    }

    /// Refuse a budget the Coordinator would refuse: above a ceiling, or
    /// with no attempt or no time at all.
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_SEARCH_ATTEMPTS).contains(&self.max_attempts) {
            bail!("max_attempts must be 1..={MAX_SEARCH_ATTEMPTS}");
        }
        if !matches!(self.mode.as_str(), "first_pass" | "all") {
            bail!("mode must be first_pass or all");
        }
        if !(1..=MAX_SEARCH_DEADLINE_SECONDS).contains(&self.deadline_seconds) {
            bail!("deadline_seconds must be 1..={MAX_SEARCH_DEADLINE_SECONDS}");
        }
        for (name, value, ceiling) in [
            (
                "max_transfer_bytes",
                self.max_transfer_bytes,
                MAX_SEARCH_TRANSFER_BYTES,
            ),
            (
                "max_expanded_bytes",
                self.max_expanded_bytes,
                MAX_SEARCH_EXPANDED_BYTES,
            ),
            (
                "max_stored_bytes",
                self.max_stored_bytes,
                MAX_SEARCH_STORED_BYTES,
            ),
        ] {
            if value > ceiling {
                bail!("{name} is above the search ceiling of {ceiling} bytes");
            }
        }
        Ok(())
    }
}

/// An attempt's share of its search's budget, fixed when the ticket was
/// issued. Hard limits: this Runtime never goes past them, whatever it
/// believes the search has left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TicketResourceBudget {
    /// The source's logical bytes: the most this attempt may receive.
    pub transfer_bytes: u64,
    /// The most this attempt may expand of its source.
    pub expanded_bytes: u64,
    /// The most artifact bytes this attempt may keep.
    pub stored_bytes: u64,
}

/// What an attempt used of its caps, in the budget's logical units. Transfer
/// is not reported: the Coordinator charges it when it authorizes the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceUsage {
    pub expanded_bytes: u64,
    pub stored_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatisfyRequest {
    pub protocol: String,
    /// The Formation search this request belongs to: an opaque id, shared by
    /// every request of one search and nothing else. It carries no K, no
    /// source and nothing about the requester. While an attempt of the
    /// search is UNKNOWN, the coordinator starts nothing more for it.
    pub search_id: String,
    /// The K being satisfied: the effective Contract when a browser Contract
    /// is part of it, else the base Contract.
    pub contract_ref: String,
    pub base_contract_ref: String,
    /// Canonical K frozen before any attempt; the Coordinator checks this digest.
    pub base_contract: ato_formation::authoring::BoundContract,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_contract: Option<BrowserContractV0>,
    pub source: SourceTransport,
    pub authorized_derivations: Vec<AuthorizedDerivation>,
    pub runtime_constraint: RuntimeConstraintWire,
    pub bindings: BTreeMap<String, String>,
    pub policy: SatisfyPolicy,
    pub budget: SatisfyBudget,
}

/// One attempt, fixed: exactly this D on exactly this environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptTicket {
    pub attempt_id: String,
    pub fence: u32,
    pub satisfy_id: String,
    pub runtime_id: String,
    pub environment_id: String,
    pub contract_ref: String,
    pub base_contract_ref: String,
    pub derivation_ref: String,
    pub input: AttemptInput,
    #[serde(default)]
    pub browser_contract: Option<BrowserContractV0>,
    pub bindings: BTreeMap<String, String>,
    pub network: String,
    /// This attempt's caps. A ticket without them is not one this Runtime
    /// runs: it would have nothing to hold the attempt to.
    pub resource_budget: TicketResourceBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttemptInput {
    Source {
        capsule_toml: String,
        archive_digest: String,
    },
    Retained {
        retained_ref: String,
        descriptor_json: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptFailureWire {
    pub code: String,
    pub stage: String,
    pub message: String,
}

/// What the Runtime itself established about the ticket, from the archive
/// and the authored route — never from the request's metadata. The
/// coordinator decides fallback from this, not from what the requester said.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAttestation {
    /// The environment this Runtime executes (or refused to execute) in.
    pub environment_id: String,
    pub agent_version: String,
    /// `None` when the ticket was refused before it could be planned.
    pub derivation_ref: Option<String>,
    pub contract_ref: Option<String>,
    /// The canonical Derivation's effect class.
    pub effects: Option<String>,
    pub requirements: Vec<Requirement>,
    pub provisions: Vec<String>,
    /// Did anything of the candidate run — a build step, the application?
    /// `false` for every refusal before execution. Kept for compatibility;
    /// `attempt_record` is what UNKNOWN is decided from.
    pub execution_started: bool,
    /// What this Runtime's durable attempt record says: `not_started`,
    /// `finished`, or `started_unfinished` — the attempt ran and its end is
    /// not durable, so what it did is UNKNOWN whatever the outcome says.
    pub attempt_record: AttemptRecordState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptResultReport {
    pub fence: u32,
    /// `pass`, `fail` or `inconclusive`.
    pub outcome: String,
    pub contract_ref: Option<String>,
    pub derivation_ref: Option<String>,
    pub materialization_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_ref: Option<String>,
    pub failure: Option<AttemptFailureWire>,
    /// The Formation attempt as the Runtime recorded it: verification,
    /// realization evidence, browser receipt.
    pub formation_attempt: Option<serde_json::Value>,
    pub verifier_receipts: Vec<serde_json::Value>,
    pub attestation: RuntimeAttestation,
    /// What the attempt used of the ticket's caps.
    pub resource_usage: ResourceUsage,
}

// ────────────────────────────────────────────────────────────────── facts

/// What this host is, as open-ended typed facts. Measured, never assumed.
pub fn probe_facts(browser_verifier: Option<&BrowserVerifierCommand>) -> BTreeMap<String, String> {
    let mut facts = BTreeMap::new();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    facts.insert("platform.os".to_owned(), os.to_owned());
    facts.insert("platform.arch".to_owned(), arch.to_owned());
    facts.insert("platform".to_owned(), format!("{os}/{arch}"));
    facts.insert("cpu.arch".to_owned(), arch.to_owned());
    if let Some(vendor) = cpu_vendor() {
        facts.insert("cpu.vendor".to_owned(), vendor);
    }
    let contained = containment_available();
    facts.insert(
        "containment".to_owned(),
        if contained { "bwrap+landlock" } else { "none" }.to_owned(),
    );
    // A process candidate is realized only under containment (ADR-019).
    facts.insert("runtime.process".to_owned(), contained.to_string());
    // This worker does not realize OCI routes.
    facts.insert("runtime.oci".to_owned(), "false".to_owned());
    // A browser verifier is a capability only when it runs contained: the
    // helper, its runtimes and the sandbox all present, and the sandbox
    // actually starting here. An uncontained verifier is never advertised.
    let browser =
        browser_verifier.is_some_and(|command| command.is_contained() && command.usable());
    if browser {
        facts.insert(
            "verifier.browser.containment".to_owned(),
            "bwrap".to_owned(),
        );
    }
    facts.insert("runtime.browser".to_owned(), browser.to_string());
    let root = Path::new(TOOLCHAIN_ROOT);
    if root.is_dir() {
        facts.insert("toolchain.root".to_owned(), TOOLCHAIN_ROOT.to_owned());
        for language in ["python", "node", "pnpm", "yarn"] {
            let Ok(entries) = std::fs::read_dir(root.join(language)) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.path().join("bin").is_dir() {
                    facts.insert(
                        format!(
                            "toolchain.{language}.{}",
                            entry.file_name().to_string_lossy()
                        ),
                        "present".to_owned(),
                    );
                }
            }
        }
    }
    facts
}

fn cpu_vendor() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        let field = |name: &str| {
            info.lines()
                .find(|line| line.starts_with(name))
                .and_then(|line| line.split(':').nth(1))
                .map(|value| value.trim().to_owned())
        };
        if let Some(vendor) = field("vendor_id") {
            return Some(
                match vendor.as_str() {
                    "GenuineIntel" => "intel",
                    "AuthenticAMD" => "amd",
                    other => return Some(other.to_ascii_lowercase()),
                }
                .to_owned(),
            );
        }
        // ARM: the implementer code (0x41 Arm, 0xc0 Ampere, 0x61 Apple).
        return field("CPU implementer").map(|code| {
            match code.as_str() {
                "0x41" => "arm",
                "0xc0" => "ampere",
                "0x61" => "apple",
                other => return format!("arm-implementer-{other}"),
            }
            .to_owned()
        });
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()?;
        let brand = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
        return Some(
            if brand.contains("apple") {
                "apple"
            } else {
                "intel"
            }
            .to_owned(),
        );
    }
    #[allow(unreachable_code)]
    None
}

/// Digest of a fact set, as the coordinator records it on a route.
pub fn facts_ref(facts: &BTreeMap<String, String>) -> String {
    digest(&serde_jcs::to_vec(facts).expect("facts serialize"))
}

// ─────────────────────────────────────────────────────────── requirements

/// What a planned Derivation needs from a Runtime, read from the Derivation.
///
/// Hard requirements are what running the route at all depends on; the
/// provisions are toolchains it installs itself when they are absent (which
/// needs a build network).
pub fn derivation_requirements(planned: &PlannedCandidate) -> (Vec<Requirement>, Vec<String>) {
    let mut requirements = Vec::new();
    if !planned.derivation.platforms.is_empty() {
        requirements.push(Requirement {
            fact: "platform".to_owned(),
            one_of: Some(
                planned
                    .derivation
                    .platforms
                    .iter()
                    .map(|p| format!("{}/{}", p.os, p.arch))
                    .collect(),
            ),
        });
    }
    if planned.plan.lane.is_process() {
        requirements.push(Requirement {
            fact: "runtime.process".to_owned(),
            one_of: Some(vec!["true".to_owned()]),
        });
    }
    if !planned.plan.actions.is_empty() || planned.plan.lane.is_process() {
        requirements.push(Requirement {
            fact: "containment".to_owned(),
            one_of: Some(vec!["bwrap+landlock".to_owned()]),
        });
        requirements.push(Requirement {
            fact: "toolchain.root".to_owned(),
            one_of: None,
        });
    }
    let mut provisions: Vec<String> = planned
        .plan
        .toolchains
        .iter()
        .map(|(name, version)| format!("toolchain.{name}.{version}"))
        .chain(
            planned
                .plan
                .package_manager
                .iter()
                .map(|manager| format!("toolchain.{}.{}", manager.name, manager.version)),
        )
        .collect();
    provisions.sort();
    provisions.dedup();
    (requirements, provisions)
}

// ──────────────────────────────────────────────────────────── requesting

pub struct Submission {
    archive: File,
    pub request: SatisfyRequest,
    /// The frozen K each authorized route was planned against, by
    /// DerivationRef — what a returned receipt is checked against.
    pub contracts: BTreeMap<String, ato_formation::authoring::BoundContract>,
}

/// A new, opaque search id: 128 random bits, nothing derived from the
/// request or the requester.
pub fn new_search_id(entropy: [u8; 16]) -> String {
    let hex: String = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("search_{hex}")
}

impl Submission {
    pub fn source_file(&self) -> Result<File> {
        let mut file = self.archive.try_clone()?;
        file.rewind()?;
        Ok(file)
    }
}

pub const MAX_SOURCE_OBJECT_BYTES: u64 = 256 * 1024 * 1024;

fn file_digest(file: &mut File) -> Result<String> {
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    file.rewind()?;
    Ok(format!("sha256:{:x}", hash.finalize()))
}

/// Freeze `dir`, plan every authorized route against it, and assemble the
/// request for the search `search_id`. Every route must bind to the same
/// Contract: one K, several Ds.
#[allow(clippy::too_many_arguments)]
pub fn prepare_submission(
    dir: &Path,
    routes: &[PathBuf],
    browser_contract: Option<BrowserContractV0>,
    work_root: &Path,
    constraint: RuntimeConstraintWire,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
    search_id: &str,
) -> Result<Submission> {
    budget.validate()?;
    std::fs::create_dir_all(work_root)?;
    anyhow::ensure!(
        fs2::available_space(work_root)?
            >= MAX_SOURCE_OBJECT_BYTES + SourceLimits::default().max_total_bytes,
        "insufficient scratch capacity to freeze source"
    );
    let mut archive = local::snapshot_directory_file(dir, work_root, MAX_SOURCE_OBJECT_BYTES)?;
    let archive_bytes = archive.metadata()?.len();
    let archive_digest = file_digest(&mut archive)?;
    let verified = FileVerifiedArchive::verify(
        archive.try_clone()?,
        &archive_digest,
        archive_bytes,
        SourceLimits::default(),
    )?;
    let frozen = local::freeze_verified_file(
        verified,
        &std::path::absolute(work_root)?,
        SourceLimits::default(),
    )?;
    let evidence = detect(&frozen.root).context("detection failed")?;

    let route_files: Vec<PathBuf> = if routes.is_empty() {
        vec![frozen.root.join("capsule.toml")]
    } else {
        routes.to_vec()
    };
    let mut base_contract_ref: Option<String> = None;
    let mut authorized = Vec::new();
    let mut contracts = BTreeMap::new();
    for file in &route_files {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("cannot read the route {}", file.display()))?;
        let draft: AuthoringDraft =
            parse_capsule_toml(&text).map_err(ato_formation::failure::FormationFailure::from)?;
        let planned = plan_candidate(
            &draft,
            &frozen.closure_ref,
            &evidence,
            BTreeMap::new(),
            "/app",
            &host_triple(),
        )?;
        match &base_contract_ref {
            None => base_contract_ref = Some(planned.contract_ref.clone()),
            Some(existing) if existing != &planned.contract_ref => bail!(
                "{} binds to Contract {}, not {existing}: one request satisfies one K",
                file.display(),
                planned.contract_ref
            ),
            Some(_) => {}
        }
        let (requirements, provisions) = derivation_requirements(&planned);
        contracts.insert(planned.derivation_ref.clone(), planned.contract.clone());
        authorized.push(AuthorizedDerivation {
            derivation_ref: planned.derivation_ref.clone(),
            capsule_toml: text,
            effects: serde_json::to_value(planned.derivation.effects)?
                .as_str()
                .unwrap_or("pure")
                .to_owned(),
            requirements,
            provisions,
        });
    }
    let base_contract_ref = base_contract_ref.context("no authorized route")?;
    let contract_ref = effective_contract_ref(&base_contract_ref, browser_contract.as_ref());
    Ok(Submission {
        request: SatisfyRequest {
            protocol: PROTOCOL.to_owned(),
            search_id: search_id.to_owned(),
            contract_ref,
            base_contract_ref,
            base_contract: contracts
                .values()
                .next()
                .context("no frozen Contract")?
                .clone(),
            browser_contract,
            source: SourceTransport {
                archive_base64: None,
                content_ref: Some(archive_digest.clone()),
                archive_bytes: Some(archive_bytes),
                archive_digest,
                closure_ref: frozen.closure_ref.as_str().to_owned(),
            },
            authorized_derivations: authorized,
            runtime_constraint: constraint,
            bindings: BTreeMap::new(),
            policy,
            budget,
        },
        contracts,
        archive,
    })
}

/// Owner-session resolution wire; it does not stop a Runtime by itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UnknownResolution {
    Resolve {
        resolution: EffectResolution,
        note: String,
        execution_stop: ExecutionStop,
    },
    TerminateSearch {
        note: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectResolution {
    NoEffectConfirmed,
    EffectReconciled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionStop {
    pub kind: ExecutionStopKind,
    pub evidence: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStopKind {
    RuntimeTerminated,
    ExecutionDisabled,
}

/// Where a satisfy request stands, read from the coordinator's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// Still being worked on.
    Running,
    Satisfied,
    /// Every admissible candidate was tried, or none may follow a failure.
    Unsatisfied,
    /// The budget ran out with candidates left.
    Exhausted,
    /// An attempt may have run and what it did is not known. Not a failure
    /// and not inconclusive: nothing further runs for the search until its
    /// owner resolves the attempt.
    EffectUnknown(Vec<UnknownAttempt>),
    /// The owner stopped the search after an UNKNOWN attempt.
    Stopped,
}

/// An attempt whose result is not known, as the requester is shown it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAttempt {
    pub attempt_id: String,
    pub runtime_id: String,
    pub reason: String,
}

impl Settlement {
    /// Read the coordinator's `status`. A status this requester does not
    /// know is an error, never read as a settlement it does.
    pub fn of(status: &serde_json::Value) -> Result<Self> {
        let field =
            |value: &serde_json::Value, name: &str| value[name].as_str().unwrap_or("?").to_owned();
        Ok(match status["status"].as_str().unwrap_or("") {
            "running" => Self::Running,
            "satisfied" => Self::Satisfied,
            "unsatisfied" => Self::Unsatisfied,
            "exhausted" => Self::Exhausted,
            "unknown" => Self::EffectUnknown(
                status["unknown_attempts"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter(|attempt| attempt["resolution"].is_null())
                    .map(|attempt| UnknownAttempt {
                        attempt_id: field(attempt, "attempt_id"),
                        runtime_id: field(attempt, "runtime_id"),
                        reason: field(attempt, "unknown_reason"),
                    })
                    .collect(),
            ),
            "stopped" => Self::Stopped,
            other => {
                bail!("the coordinator reports a status this requester does not know: {other:?}")
            }
        })
    }

    /// The typed reason a request ended without a verified route.
    pub fn terminal_reason(&self) -> Option<&'static str> {
        match self {
            Self::Running | Self::Satisfied => None,
            Self::Unsatisfied => Some("unsatisfied"),
            Self::Exhausted => Some("budget_exhausted"),
            Self::EffectUnknown(_) => Some("effect_unknown"),
            Self::Stopped => Some("search_stopped"),
        }
    }
}

impl std::fmt::Display for Settlement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Satisfied => write!(f, "satisfied"),
            Self::Unsatisfied => write!(f, "unsatisfied: no admissible candidate passed"),
            Self::Exhausted => write!(f, "budget_exhausted: candidates remain"),
            Self::EffectUnknown(attempts) => {
                write!(f, "effect_unknown: ")?;
                for (index, attempt) in attempts.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(
                        f,
                        "attempt {} on Runtime {} may have run and its result is not known ({})",
                        attempt.attempt_id, attempt.runtime_id, attempt.reason
                    )?;
                }
                write!(
                    f,
                    ". Nothing further runs for this search until its owner resolves it"
                )
            }
            Self::Stopped => write!(f, "search_stopped: the owner stopped this search"),
        }
    }
}

/// The verified routes of a settled satisfy request that prove the complete
/// effective K, split from those that do not (with the reason).
///
/// The coordinator authenticates the Runtime and holds the fence; the
/// judgement of what the receipts prove is
/// [`ato_formation::receipt::accept_verified_route`], against the K this
/// requester froze.
pub fn accept_verified_routes(
    submission: &Submission,
    satisfy_id: &str,
    status: &serde_json::Value,
) -> (Vec<serde_json::Value>, Vec<String>) {
    use ato_formation::receipt::VerifiedRouteAssignment;
    let request = &submission.request;
    let assignment = VerifiedRouteAssignment {
        request_id: satisfy_id,
        effective_contract_ref: &request.contract_ref,
        base_contract_ref: &request.base_contract_ref,
        contracts: &submission.contracts,
        browser_contract: request.browser_contract.as_ref(),
    };
    accept_routes_for_assignment(&assignment, status)
}
pub fn accept_routes_for_assignment(
    assignment: &ato_formation::receipt::VerifiedRouteAssignment<'_>,
    status: &serde_json::Value,
) -> (Vec<serde_json::Value>, Vec<String>) {
    use ato_formation::receipt::accept_verified_route;
    let empty = Vec::new();
    let attempts = status["attempts"].as_array().unwrap_or(&empty);
    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for route in status["verified_routes"].as_array().unwrap_or(&empty) {
        match accept_verified_route(assignment, route, attempts) {
            Ok(()) => accepted.push(route.clone()),
            Err(rejection) => refused.push(format!(
                "{} on {}/{}: {}: {}",
                route["derivation_ref"].as_str().unwrap_or("?"),
                route["runtime_id"].as_str().unwrap_or("?"),
                route["environment_id"].as_str().unwrap_or("?"),
                rejection.code,
                rejection.detail
            )),
        }
    }
    (accepted, refused)
}

// ──────────────────────────────────────────────────────────────── client

pub struct Client {
    api: String,
    token: String,
    http: reqwest::blocking::Client,
}

impl Client {
    pub fn new(api: &str, token: &str) -> Result<Self> {
        Ok(Self {
            api: api.trim_end_matches('/').to_owned(),
            token: token.trim().to_owned(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1/runtime-network{path}", self.api)
    }

    fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<Option<T>> {
        let response = request.bearer_auth(&self.token).send()?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let body = response.text()?;
        if !status.is_success() {
            bail!(
                "coordinator answered {status}: {}",
                crate::api::bounded_reason(&body)
            );
        }
        Ok(Some(
            serde_json::from_str(&body).context("coordinator answer is not JSON")?,
        ))
    }

    pub fn advertise(&self, descriptor: &RuntimeDescriptorAdvert) -> Result<serde_json::Value> {
        self.send(self.http.put(self.url("/runtimes/self")).json(descriptor))?
            .context("empty advertise answer")
    }

    pub fn report_availability(&self, report: &AvailabilityReport) -> Result<()> {
        let _: Option<serde_json::Value> = self.send(
            self.http
                .post(self.url("/runtimes/self/availability"))
                .json(report),
        )?;
        Ok(())
    }

    pub fn claim(&self) -> Result<Option<AttemptTicket>> {
        self.send::<serde_json::Value>(self.http.post(self.url("/attempts/claim")))?
            .map(AttemptTicket::from_wire)
            .transpose()
    }

    /// The ticket's source, read up to `max_bytes` (the ticket's transfer
    /// cap) and one byte more: a longer source is returned as such, never
    /// read to its end.
    pub fn source(
        &self,
        attempt_id: &str,
        max_bytes: u64,
        fence: u32,
        work_root: &Path,
    ) -> Result<File> {
        self.download_input(attempt_id, max_bytes, fence, work_root, "source")
    }
    fn download_input(
        &self,
        attempt_id: &str,
        max_bytes: u64,
        fence: u32,
        work_root: &Path,
        kind: &str,
    ) -> Result<File> {
        anyhow::ensure!(
            max_bytes <= MAX_SOURCE_OBJECT_BYTES,
            "source exceeds object cap"
        );
        std::fs::create_dir_all(work_root)?;
        anyhow::ensure!(
            fs2::available_space(work_root)? >= max_bytes + SourceLimits::default().max_total_bytes,
            "insufficient Runtime scratch capacity"
        );
        let response = self
            .http
            .get(self.url(&format!("/attempts/{attempt_id}/{kind}")))
            .header("x-ato-attempt-fence", fence)
            .bearer_auth(&self.token)
            .timeout(SOURCE_TRANSFER_TIMEOUT)
            .send()?;
        anyhow::ensure!(
            response.status().is_success(),
            "source unavailable ({})",
            response.status()
        );
        let mut response = response.take(max_bytes.saturating_add(1));
        let mut file = tempfile::tempfile_in(work_root)?;
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = response.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            anyhow::ensure!(size <= max_bytes, "source exceeds transfer budget");
            file.write_all(&buffer[..count])?;
        }
        anyhow::ensure!(size == max_bytes, "source size mismatch");
        file.rewind()?;
        Ok(file)
    }

    pub fn submit(&self, submission: &Submission) -> Result<serde_json::Value> {
        let source = &submission.request.source;
        let pending: serde_json::Value = self.send(self.http.post(self.url("/sources")).json(&serde_json::json!({
            "archive_digest": source.archive_digest, "archive_bytes": source.archive_bytes,
        })))?.context("missing source upload response")?;
        let id = pending["upload_id"]
            .as_str()
            .context("missing source upload id")?;
        if pending["status"] != "ready" {
            let _: Option<serde_json::Value> = self.send(
                self.http
                    .put(self.url(&format!("/sources/{id}/content")))
                    .timeout(SOURCE_TRANSFER_TIMEOUT)
                    .body(submission.source_file()?),
            )?;
            let _: Option<serde_json::Value> = self.send(
                self.http
                    .post(self.url(&format!("/sources/{id}/finalize")))
                    .timeout(SOURCE_TRANSFER_TIMEOUT),
            )?;
        }
        self.satisfy(&submission.request)
    }

    pub fn report(&self, attempt_id: &str, result: &AttemptResultReport) -> Result<()> {
        let _: Option<serde_json::Value> = self.send(
            self.http
                .post(self.url(&format!("/attempts/{attempt_id}/result")))
                .json(result),
        )?;
        Ok(())
    }

    pub fn satisfy(&self, request: &SatisfyRequest) -> Result<serde_json::Value> {
        self.send(self.http.post(self.url("/satisfy")).json(request))?
            .context("empty satisfy answer")
    }

    pub fn satisfy_status(&self, id: &str) -> Result<serde_json::Value> {
        self.send(self.http.get(self.url(&format!("/satisfy/{id}"))))?
            .context("empty status answer")
    }
}

// ─────────────────────────────────────────────────────────────── serving

pub struct ServeConfig {
    pub api: String,
    pub token: String,
    pub work_root: PathBuf,
    pub out_dir: PathBuf,
    pub shim: PathBuf,
    pub browser_verifier: Option<BrowserVerifierCommand>,
    pub poll: Duration,
    /// Handle at most this many tickets, then return.
    pub max_tickets: Option<u32>,
}

/// Join the Runtime Network: advertise, report availability, and execute the
/// tickets addressed to this Runtime, one at a time.
pub fn serve(config: &ServeConfig) -> Result<()> {
    let client = Arc::new(Client::new(&config.api, &config.token)?);
    let facts = probe_facts(config.browser_verifier.as_ref());
    let advertised = client.advertise(&RuntimeDescriptorAdvert {
        protocol: PROTOCOL.to_owned(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        environments: vec![EnvironmentAdvert {
            environment_id: NATIVE_ENVIRONMENT.to_owned(),
            facts: facts.clone(),
        }],
    })?;
    eprintln!(
        "[runtime-network] joined as {} ({})",
        advertised["runtime_id"].as_str().unwrap_or("?"),
        facts.get("platform").map(String::as_str).unwrap_or("?")
    );

    // Availability is reported on its own clock, so a long attempt does not
    // make the Runtime look offline — and at once whenever a slot is taken or
    // freed, so the coordinator does not schedule against a stale load.
    let busy = Arc::new(AtomicU32::new(0));
    let report_now = |client: &Client, slots: u32| {
        if let Err(error) = client.report_availability(&AvailabilityReport {
            capacity: 1,
            current_slots: slots,
            health: "ok".to_owned(),
        }) {
            eprintln!("[runtime-network] availability report failed: {error:#}");
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat = {
        let (client, busy, stop) = (client.clone(), busy.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = client.report_availability(&AvailabilityReport {
                    capacity: 1,
                    current_slots: busy.load(Ordering::Relaxed),
                    health: "ok".to_owned(),
                });
                for _ in 0..30 {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        })
    };

    let mut handled = 0_u32;
    let outcome = (|| -> Result<()> {
        loop {
            if config.max_tickets.is_some_and(|max| handled >= max) {
                return Ok(());
            }
            let ticket = match client.claim() {
                Ok(Some(ticket)) => ticket,
                Ok(None) => {
                    std::thread::sleep(config.poll);
                    continue;
                }
                // A coordinator or link that drops a request is not a reason
                // to leave the network; the next poll tries again.
                Err(error) => {
                    eprintln!("[runtime-network] claim failed, retrying: {error:#}");
                    std::thread::sleep(config.poll * 5);
                    continue;
                }
            };
            busy.store(1, Ordering::Relaxed);
            report_now(&client, 1);
            eprintln!(
                "[runtime-network] attempt {} — {} on {}",
                ticket.attempt_id, ticket.derivation_ref, ticket.environment_id
            );
            let report = execute_ticket_with_publication(
                config,
                &ticket,
                || {
                    client.download_input(
                        &ticket.attempt_id,
                        ticket.resource_budget.transfer_bytes,
                        ticket.fence,
                        &config.work_root,
                        match ticket.input {
                            AttemptInput::Source { .. } => "source",
                            AttemptInput::Retained { .. } => "retained-content",
                        },
                    )
                },
                Some(&client),
            );
            eprintln!(
                "[runtime-network] attempt {} → {}",
                ticket.attempt_id, report.outcome
            );
            // Free before the result lands: the result advances the request,
            // and the next ticket may be this Runtime's.
            busy.store(0, Ordering::Relaxed);
            report_now(&client, 0);
            // A result that cannot be delivered is retried; if it never is,
            // the coordinator records the attempt as UNKNOWN: it was claimed,
            // and nobody can say what it did.
            for retry in 0..5 {
                match client.report(&ticket.attempt_id, &report) {
                    Ok(()) => break,
                    Err(error) => {
                        eprintln!(
                            "[runtime-network] report failed (try {}): {error:#}",
                            retry + 1
                        );
                        std::thread::sleep(Duration::from_secs(3));
                    }
                }
            }
            handled += 1;
        }
    })();
    stop.store(true, Ordering::Relaxed);
    let _ = heartbeat.join();
    outcome
}

use crate::admission::effects_name;
pub use crate::admission::is_disposable;

/// This Runtime's attestation before it has planned anything.
fn attestation() -> RuntimeAttestation {
    RuntimeAttestation {
        environment_id: NATIVE_ENVIRONMENT.to_owned(),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        derivation_ref: None,
        contract_ref: None,
        effects: None,
        requirements: Vec::new(),
        provisions: Vec::new(),
        execution_started: false,
        attempt_record: AttemptRecordState::NotStarted,
    }
}

/// A ticket this Runtime did not run: nothing of the candidate executed.
fn refused(
    ticket: &AttemptTicket,
    attestation: RuntimeAttestation,
    code: &str,
    message: &str,
) -> AttemptResultReport {
    AttemptResultReport {
        fence: ticket.fence,
        outcome: "inconclusive".to_owned(),
        contract_ref: None,
        derivation_ref: None,
        materialization_ref: None,
        retained_ref: None,
        failure: Some(AttemptFailureWire {
            code: code.to_owned(),
            stage: "admission".to_owned(),
            message: crate::api::bounded_reason(message),
        }),
        formation_attempt: None,
        verifier_receipts: Vec::new(),
        attestation,
        resource_usage: ResourceUsage::default(),
    }
}

/// Run one ticket through the common attempt entry — the ticket's frozen
/// archive, the route it names and nothing else — and report what happened.
///
/// Before anything of the candidate runs, this Runtime re-establishes what
/// the ticket is from the archive and the route alone — planned ONCE, and
/// that plan is the one executed — and refuses it when the environment is
/// not one it executes, it asks for bindings, or its own planning does not
/// reach the ticket's refs. Effects, platform, containment and verifier
/// admission are the common admission's, still before execution. Whether
/// anything ran is read from the attempt's start record, never inferred
/// from how the attempt ended.
pub fn execute_ticket(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    archive: Vec<u8>,
) -> AttemptResultReport {
    execute_ticket_with_source(config, ticket, || Ok(archive))
}

/// Source acquisition is inside the durable delivery reservation too. An old
/// ticket's historical state always takes precedence over early preflight errors.
pub fn execute_ticket_with_source(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    source: impl FnOnce() -> Result<Vec<u8>>,
) -> AttemptResultReport {
    execute_ticket_with_file(config, ticket, || {
        let bytes = source()?;
        std::fs::create_dir_all(&config.work_root)?;
        let mut file = tempfile::tempfile_in(&config.work_root)?;
        file.write_all(&bytes)?;
        Ok(file)
    })
}

pub fn execute_ticket_with_file(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    source: impl FnOnce() -> Result<File>,
) -> AttemptResultReport {
    execute_ticket_with_publication(config, ticket, source, None)
}
fn execute_ticket_with_publication(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    source: impl FnOnce() -> Result<File>,
    publisher: Option<&Client>,
) -> AttemptResultReport {
    let mut attested = attestation();
    let permit = match AttemptJournal::new(config.out_dir.join("attempt-records"))
        .acquire(&ticket.satisfy_id, &ticket.attempt_id)
    {
        Ok(permit) => permit,
        Err(refusal) => {
            attested.attempt_record = refusal.record_state();
            attested.execution_started = attested.attempt_record.execution_started();
            return refused(ticket, attested, refusal.code(), &refusal.message());
        }
    };
    let archive = match source() {
        Ok(archive) => archive,
        Err(error) => {
            return refused(
                ticket,
                attested,
                "source_unavailable",
                &format!("{error:#}"),
            );
        }
    };
    // The ticket's transfer cap is the source's own size; a longer source is
    // not the one the search paid for.
    if archive.metadata().map(|m| m.len()).unwrap_or(u64::MAX)
        > ticket.resource_budget.transfer_bytes
    {
        return refused(
            ticket,
            attested,
            "search_transfer_budget_exceeded",
            &format!(
                "the source is longer than the ticket's transfer budget of {} bytes",
                ticket.resource_budget.transfer_bytes
            ),
        );
    }
    if ticket.environment_id != NATIVE_ENVIRONMENT {
        return refused(
            ticket,
            attested,
            "environment_mismatch",
            &format!(
                "the ticket names environment {:?}; this Runtime executes only {NATIVE_ENVIRONMENT:?}",
                ticket.environment_id
            ),
        );
    }
    if !ticket.bindings.is_empty() {
        return refused(
            ticket,
            attested,
            "bindings_unsupported",
            "this Runtime binds no external state in Phase 1",
        );
    }
    let attempt_root = config.work_root.join(&ticket.attempt_id);
    let report = execute_planned_ticket(
        config,
        ticket,
        archive,
        &attempt_root,
        &mut attested,
        permit,
        publisher,
    );
    let _ = std::fs::remove_dir_all(&attempt_root);
    report
}

fn execute_planned_ticket(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    archive: File,
    attempt_root: &Path,
    attested: &mut RuntimeAttestation,
    permit: Box<dyn AttemptPermit>,
    publisher: Option<&Client>,
) -> AttemptResultReport {
    let AttemptInput::Source {
        capsule_toml,
        archive_digest,
    } = &ticket.input
    else {
        return execute_retained_ticket(config, ticket, archive, attempt_root, attested, permit);
    };
    // The ticket's expanded cap, under this Runtime's own source ceiling. The
    // tree is measured against it before anything is written, and expansion
    // enforces it again.
    let ceiling = SourceLimits::default();
    let limits = SourceLimits {
        max_total_bytes: ceiling
            .max_total_bytes
            .min(ticket.resource_budget.expanded_bytes),
        ..ceiling
    };
    let verified = match FileVerifiedArchive::verify(
        archive,
        archive_digest,
        ticket.resource_budget.transfer_bytes,
        limits,
    )
    .map_err(anyhow::Error::new)
    {
        Ok(verified) => verified,
        Err(error) => {
            let over_cap = ticket.resource_budget.expanded_bytes <= ceiling.max_total_bytes
                && error.chain().any(|cause| {
                    matches!(
                        cause.downcast_ref::<SourceError>(),
                        Some(SourceError::LimitExceeded {
                            limit: "max_total_bytes"
                        })
                    )
                });
            return refused(
                ticket,
                attested.clone(),
                if over_cap {
                    "search_expanded_budget_exceeded"
                } else {
                    "ticket_unplannable"
                },
                &format!("{error:#}"),
            );
        }
    };
    // From here the tree may be written: its measured bytes are what this
    // attempt expanded, whatever happens next.
    let mut usage = ResourceUsage {
        expanded_bytes: verified.expanded_bytes(),
        stored_bytes: 0,
    };
    let refused_after_expansion =
        |attested: RuntimeAttestation, code: &str, message: &str| AttemptResultReport {
            resource_usage: usage,
            ..refused(ticket, attested, code, message)
        };
    let planned = (|| -> Result<_> {
        std::fs::create_dir_all(attempt_root)?;
        let frozen =
            local::freeze_verified_file(verified, &std::path::absolute(attempt_root)?, limits)?;
        let evidence = detect(&frozen.root).context("detection failed")?;
        let draft: AuthoringDraft = parse_capsule_toml(capsule_toml)
            .map_err(ato_formation::failure::FormationFailure::from)?;
        let planned = plan_candidate(
            &draft,
            &frozen.closure_ref,
            &evidence,
            BTreeMap::new(),
            "/app",
            &host_triple(),
        )?;
        let contract_ref =
            effective_contract_ref(&planned.contract_ref, ticket.browser_contract.as_ref());
        Ok((frozen, planned, contract_ref))
    })();
    let (frozen, planned, contract_ref) = match planned {
        Ok(planned) => planned,
        Err(error) => {
            return refused_after_expansion(
                attested.clone(),
                "ticket_unplannable",
                &format!("{error:#}"),
            );
        }
    };
    let (requirements, provisions) = derivation_requirements(&planned);
    attested.derivation_ref = Some(planned.derivation_ref.clone());
    attested.contract_ref = Some(contract_ref.clone());
    attested.effects = Some(effects_name(planned.derivation.effects));
    attested.requirements = requirements;
    attested.provisions = provisions;

    // The Runtime does not rewrite the ticket: its own planning must reach
    // the refs the requester computed.
    if planned.derivation_ref != ticket.derivation_ref || contract_ref != ticket.contract_ref {
        return refused_after_expansion(
            attested.clone(),
            "ticket_mismatch",
            &format!(
                "planned {}/{contract_ref}, the ticket names {}/{}",
                planned.derivation_ref, ticket.derivation_ref, ticket.contract_ref
            ),
        );
    }

    let network = match ticket.network.as_str() {
        "dependency-resolution" => NetworkPolicy::DependencyResolution,
        _ => NetworkPolicy::Denied,
    };
    // A Runtime Network attempt is never verified outside the verifier
    // sandbox, whatever the worker was started with.
    let browser = ticket
        .browser_contract
        .as_ref()
        .map(|contract| BrowserVerification {
            contract: contract.clone(),
            verifier: config
                .browser_verifier
                .clone()
                .filter(BrowserVerifierCommand::is_contained),
            budget: Default::default(),
        });
    let spec = planned.attempt_spec();
    let outcome = run_reserved_attempt(
        &AttemptRequest {
            // Every attempt of one satisfy request spends from it: a
            // redelivered ticket, or another route after an UNKNOWN one, is
            // held by the same record.
            request_id: &ticket.satisfy_id,
            attempt_id: &ticket.attempt_id,
            label: "authored",
            spec: &spec,
            contract_ref: &contract_ref,
            runtime_id: &ticket.runtime_id,
            profile: &local::probe_local_runtime(),
            // A ticket runs unattended, and may be retried elsewhere.
            authorization: EffectAuthorization::Unattended,
            network,
            browser: browser.as_ref(),
            attempt_root,
            // The Runtime keeps the artifact for the coordinator, not the
            // running candidate.
            continuation: Continuation::Stop,
            receipt: ReceiptContext::formation(),
            interrupt: None,
        },
        &FormationRealizer {
            planned: &planned,
            source_root: &frozen.root,
            builder: &LocalAttemptExecutor {
                shim: config.shim.clone(),
                network,
                limits: BuildLimits::default(),
            },
            shim: &config.shim,
            network,
        },
        Ok(permit),
    );
    attested.execution_started = outcome.execution_started();
    attested.attempt_record = outcome.attempt_record;
    let mut attempt = outcome.attempt;
    // Keeping the artifact is publication, not verification: a publication
    // that fails — or that the ticket's stored cap does not allow — leaves
    // the receipt and `runtime_verification` exactly as they were.
    let publication_failed =
        |attempt: &mut ato_formation::request::FormationAttempt, code: &str, message: String| {
            attempt.status = AttemptStatus::Failed;
            attempt.outcomes.publication = ato_formation::request::Outcome::failed(code);
            attempt.failure = Some(ato_formation::request::AttemptFailure {
                code: code.to_owned(),
                stage: "publish".to_owned(),
                message: crate::api::bounded_reason(&message),
            });
            None
        };
    let materialization_ref = match &outcome.verified {
        Some(executed) => match local::prepare_artifact(executed) {
            Ok(prepared) if prepared.logical_bytes() > ticket.resource_budget.stored_bytes => {
                publication_failed(
                    &mut attempt,
                    "search_stored_budget_exceeded",
                    format!(
                        "the artifact is {} bytes; the ticket's stored budget is {} bytes",
                        prepared.logical_bytes(),
                        ticket.resource_budget.stored_bytes
                    ),
                )
            }
            Ok(prepared) => {
                // Keeping it was attempted: charged even if the write fails.
                usage.stored_bytes = prepared.logical_bytes();
                match prepared.store(&config.out_dir) {
                    Ok(reference) => {
                        attempt.outcomes.publication = ato_formation::request::Outcome::succeeded();
                        Some(reference)
                    }
                    Err(error) => publication_failed(
                        &mut attempt,
                        "artifact_store_failed",
                        format!("{error:#}"),
                    ),
                }
            }
            Err(error) => {
                publication_failed(&mut attempt, "artifact_store_failed", format!("{error:#}"))
            }
        },
        None => None,
    };

    let mut report = attempt_report(ticket, &attempt, attested, materialization_ref, None, usage);
    if let Some(publisher) = publisher
        && report.outcome == "pass"
    {
        let publication = (|| -> Result<String> {
            let executed = outcome
                .verified
                .as_ref()
                .context("verified artifact missing")?;
            let prepared = crate::retained::prepare(
                executed,
                &planned,
                ticket.browser_contract.as_ref(),
                &ticket.attempt_id,
            )?;
            anyhow::ensure!(
                prepared.descriptor.artifact.bytes <= ticket.resource_budget.stored_bytes,
                "search_stored_budget_exceeded"
            );
            report.resource_usage.stored_bytes = prepared.descriptor.artifact.bytes;
            publisher.retain(ticket, &prepared, &report)
        })();
        match publication {
            Ok(reference) => report.retained_ref = Some(reference),
            Err(error) => {
                report.outcome = "fail".into();
                report.materialization_ref = None;
                report.failure = Some(AttemptFailureWire {
                    code: "retained_publication_failed".into(),
                    stage: "publish".into(),
                    message: crate::api::bounded_reason(&format!("{error:#}")),
                });
                if let Some(attempt) = &mut report.formation_attempt {
                    attempt["status"] = serde_json::json!("failed");
                    attempt["outcomes"]["publication"] = serde_json::json!({"state":"failed","reason":"retained_publication_failed"});
                    attempt["failure"] = serde_json::to_value(&report.failure).unwrap_or_default();
                }
            }
        }
    }
    report
}

/// Explicit replay dispatch. Never call source resolution or plan_candidate.
fn execute_retained_ticket(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    mut archive: File,
    attempt_root: &Path,
    attested: &mut RuntimeAttestation,
    permit: Box<dyn AttemptPermit>,
) -> AttemptResultReport {
    use ato_formation::{execution::lower_retained, retained::RetainedCandidateV1};
    use ato_runtime_attempt::{
        plan::{BoundCandidate, PlannedCandidate},
        retained::RetainedCandidateRealizer,
    };
    let AttemptInput::Retained {
        retained_ref,
        descriptor_json,
    } = &ticket.input
    else {
        unreachable!("typed dispatch")
    };
    let prepared = (|| -> Result<_> {
        let descriptor = RetainedCandidateV1::parse(descriptor_json.as_bytes(), retained_ref)?;
        descriptor.match_assignment(&ticket.contract_ref, &ticket.derivation_ref)?;
        anyhow::ensure!(
            descriptor.base_contract_ref == ticket.base_contract_ref
                && descriptor.browser_contract == ticket.browser_contract,
            "retained frozen K mismatch"
        );
        anyhow::ensure!(
            descriptor.artifact.bytes == ticket.resource_budget.transfer_bytes,
            "retained transfer reservation mismatch"
        );
        anyhow::ensure!(
            descriptor.artifact.expanded_bytes <= ticket.resource_budget.expanded_bytes,
            "retained expanded reservation exceeded"
        );
        let planned = PlannedCandidate {
            bound: BoundCandidate {
                contract: descriptor.base_contract.clone(),
                derivation: descriptor.derivation.clone(),
                contract_ref: descriptor.base_contract_ref.clone(),
                derivation_ref: descriptor.derivation_ref.clone(),
            },
            plan: lower_retained(&descriptor)?,
        };
        std::fs::create_dir_all(attempt_root)?;
        let path = attempt_root.join("retained.tar");
        archive.rewind()?;
        std::io::copy(&mut archive, &mut File::create(&path)?)?;
        Ok((descriptor, planned, path))
    })();
    let (descriptor, planned, path) = match prepared {
        Ok(value) => value,
        Err(error) => {
            return refused(
                ticket,
                attested.clone(),
                "retained_preflight_failed",
                &format!("{error:#}"),
            );
        }
    };
    let (requirements, provisions) = derivation_requirements(&planned);
    attested.derivation_ref = Some(planned.derivation_ref.clone());
    attested.contract_ref = Some(ticket.contract_ref.clone());
    attested.effects = Some(effects_name(planned.derivation.effects));
    attested.requirements = requirements;
    attested.provisions = provisions;
    let browser = ticket
        .browser_contract
        .as_ref()
        .map(|contract| BrowserVerification {
            contract: contract.clone(),
            verifier: config
                .browser_verifier
                .clone()
                .filter(BrowserVerifierCommand::is_contained),
            budget: Default::default(),
        });
    let spec = planned.attempt_spec();
    let outcome = run_reserved_attempt(
        &AttemptRequest {
            request_id: &ticket.satisfy_id,
            attempt_id: &ticket.attempt_id,
            label: "retained",
            spec: &spec,
            contract_ref: &ticket.contract_ref,
            runtime_id: &ticket.runtime_id,
            profile: &local::probe_local_runtime(),
            authorization: EffectAuthorization::Unattended,
            network: NetworkPolicy::Denied,
            browser: browser.as_ref(),
            attempt_root,
            continuation: Continuation::Stop,
            receipt: ReceiptContext::formation(),
            interrupt: None,
        },
        &RetainedCandidateRealizer {
            descriptor: &descriptor,
            archive: &path,
            expected_contract_ref: &ticket.contract_ref,
            expected_derivation_ref: &ticket.derivation_ref,
            expanded_limit: ticket.resource_budget.expanded_bytes,
            shim: &config.shim,
        },
        Ok(permit),
    );
    attested.execution_started = outcome.execution_started();
    attested.attempt_record = outcome.attempt_record;
    let attempt = outcome.attempt;
    let pass = attempt.status == AttemptStatus::Verified;
    attempt_report(
        ticket,
        &attempt,
        attested,
        pass.then_some(descriptor.materialization_ref),
        pass.then(|| retained_ref.clone()),
        ResourceUsage {
            expanded_bytes: descriptor.artifact.expanded_bytes,
            stored_bytes: 0,
        },
    )
}

impl Client {
    fn retain(
        &self,
        ticket: &AttemptTicket,
        prepared: &crate::retained::PreparedRetained,
        report: &AttemptResultReport,
    ) -> Result<String> {
        let reference = prepared.descriptor.retained_ref()?;
        let path = format!("/attempts/{}/retained", ticket.attempt_id);
        let pending:serde_json::Value=self.send(self.http.post(self.url(&path)).json(&serde_json::json!({
            "fence":ticket.fence,"retained_ref":reference,"descriptor_json":String::from_utf8(prepared.descriptor.canonical_bytes()?)?,"verification":report
        })))?.context("missing retained reservation")?;
        let id = pending["upload_id"]
            .as_str()
            .context("missing retained upload id")?;
        if pending["status"] != "ready" {
            let _: Option<serde_json::Value> = self.send(
                self.http
                    .put(self.url(&format!("{path}/{id}/content")))
                    .header("x-ato-attempt-fence", ticket.fence)
                    .timeout(SOURCE_TRANSFER_TIMEOUT)
                    .body(prepared.bytes.clone()),
            )?;
            let ready: serde_json::Value = self
                .send(
                    self.http
                        .post(self.url(&format!("{path}/{id}/finalize")))
                        .header("x-ato-attempt-fence", ticket.fence)
                        .timeout(SOURCE_TRANSFER_TIMEOUT),
                )?
                .context("missing retained finalization")?;
            anyhow::ensure!(
                ready["status"] == "ready" && ready["retained_ref"] == reference,
                "retained object is not ready"
            );
        }
        Ok(reference)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedAuthorizedDerivation {
    pub derivation_ref: String,
    pub effects: String,
    pub requirements: Vec<Requirement>,
    pub provisions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedRequestInput {
    Retained { retained_ref: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedSatisfyRequest {
    pub protocol: String,
    pub search_id: String,
    pub contract_ref: String,
    pub base_contract_ref: String,
    pub base_contract: ato_formation::authoring::BoundContract,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_contract: Option<BrowserContractV0>,
    pub input: RetainedRequestInput,
    pub authorized_derivations: Vec<RetainedAuthorizedDerivation>,
    pub runtime_constraint: RuntimeConstraintWire,
    pub bindings: BTreeMap<String, String>,
    pub policy: SatisfyPolicy,
    pub budget: SatisfyBudget,
}
pub struct RetainedSubmission {
    pub request: RetainedSatisfyRequest,
    pub contracts: BTreeMap<String, ato_formation::authoring::BoundContract>,
}
impl RetainedSubmission {
    pub fn accept_routes(
        &self,
        satisfy_id: &str,
        status: &serde_json::Value,
    ) -> (Vec<serde_json::Value>, Vec<String>) {
        accept_routes_for_assignment(
            &ato_formation::receipt::VerifiedRouteAssignment {
                request_id: satisfy_id,
                effective_contract_ref: &self.request.contract_ref,
                base_contract_ref: &self.request.base_contract_ref,
                contracts: &self.contracts,
                browser_contract: self.request.browser_contract.as_ref(),
            },
            status,
        )
    }
}
impl Client {
    pub fn prepare_retained(
        &self,
        reference: &str,
        search_id: &str,
        constraint: RuntimeConstraintWire,
        budget: SatisfyBudget,
    ) -> Result<RetainedSubmission> {
        use ato_formation::{
            execution::lower_retained,
            retained::{MAX_DESCRIPTOR_BYTES, RetainedCandidateV1},
        };
        use ato_runtime_attempt::plan::{BoundCandidate, PlannedCandidate};
        anyhow::ensure!(
            reference.len() == 71
                && reference.starts_with("sha256:")
                && reference[7..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid retained reference"
        );
        budget.validate()?;
        let response = self
            .http
            .get(self.url(&format!("/retained/{reference}")))
            .bearer_auth(&self.token)
            .send()?;
        anyhow::ensure!(
            response.status().is_success(),
            "retained descriptor unavailable ({})",
            response.status()
        );
        let mut bytes = Vec::new();
        response
            .take((MAX_DESCRIPTOR_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        let d = RetainedCandidateV1::parse(&bytes, reference)?;
        let planned = PlannedCandidate {
            bound: BoundCandidate {
                contract: d.base_contract.clone(),
                derivation: d.derivation.clone(),
                contract_ref: d.base_contract_ref.clone(),
                derivation_ref: d.derivation_ref.clone(),
            },
            plan: lower_retained(&d)?,
        };
        let (requirements, provisions) = derivation_requirements(&planned);
        Ok(RetainedSubmission {
            contracts: BTreeMap::from([(d.derivation_ref.clone(), d.base_contract.clone())]),
            request: RetainedSatisfyRequest {
                protocol: PROTOCOL.into(),
                search_id: search_id.into(),
                contract_ref: d.contract_ref,
                base_contract_ref: d.base_contract_ref,
                base_contract: d.base_contract,
                browser_contract: d.browser_contract,
                input: RetainedRequestInput::Retained {
                    retained_ref: reference.into(),
                },
                authorized_derivations: vec![RetainedAuthorizedDerivation {
                    derivation_ref: d.derivation_ref,
                    effects: effects_name(d.derivation.effects),
                    requirements,
                    provisions,
                }],
                runtime_constraint: constraint,
                bindings: BTreeMap::new(),
                policy: SatisfyPolicy {
                    network: "denied".into(),
                    allow_managed: false,
                },
                budget,
            },
        })
    }
    pub fn submit_retained(&self, submission: &RetainedSubmission) -> Result<serde_json::Value> {
        self.send(
            self.http
                .post(self.url("/satisfy"))
                .json(&submission.request),
        )?
        .context("missing retained request response")
    }
}

fn attempt_report(
    ticket: &AttemptTicket,
    attempt: &ato_formation::request::FormationAttempt,
    attested: &RuntimeAttestation,
    materialization_ref: Option<String>,
    retained_ref: Option<String>,
    usage: ResourceUsage,
) -> AttemptResultReport {
    let failure_code = attempt.failure.as_ref().map(|f| f.code.as_str());
    let outcome = match (attempt.status, failure_code) {
        (AttemptStatus::Verified, _) => "pass",
        (_, Some("browser_contract_inconclusive")) | (AttemptStatus::Filtered, _) => "inconclusive",
        _ => "fail",
    };
    let mut receipts = Vec::new();
    if let Some(verification) = &attempt.verification {
        receipts.push(serde_json::json!({"kind":"http_contract","verification":verification}));
    }
    if let Some(receipt) = &attempt.receipt {
        receipts.push(serde_json::json!({"kind":"contract_verification","receipt":receipt}));
    }
    if let Some(browser) = &attempt.browser_verification {
        receipts.push(serde_json::json!({"kind":"browser_contract","receipt":browser}));
    }
    AttemptResultReport {
        fence: ticket.fence,
        outcome: outcome.into(),
        contract_ref: attempt.contract_ref.clone(),
        derivation_ref: attempt.derivation_ref.clone(),
        materialization_ref,
        retained_ref,
        failure: attempt.failure.as_ref().map(|f| AttemptFailureWire {
            code: f.code.clone(),
            stage: f.stage.clone(),
            message: f.message.clone(),
        }),
        formation_attempt: serde_json::to_value(attempt).ok(),
        verifier_receipts: receipts,
        attestation: attested.clone(),
        resource_usage: usage,
    }
}
