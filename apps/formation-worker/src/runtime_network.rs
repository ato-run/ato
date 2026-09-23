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
use ato_formation::authoring::{AuthoringDraft, EffectClass};
use ato_formation::browser::{BrowserContractV0, effective_contract_ref};
use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::intent::Lane;
use ato_formation::request::{
    AttemptStatus, ContractSource, FormationNetworkPolicy, FormationPolicy, FormationRequest,
    FormationResult, InitialCondition, RuntimeConstraint, SearchBudget,
};
use ato_formation::source::SourceLimits;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::browser_verify::BrowserVerifierCommand;
use crate::job::{PlannedCandidate, digest, plan_candidate};
use crate::local::{self, LocalFormation, freeze_archive, host_triple, snapshot_directory};
use crate::sandbox::{BuildLimits, TOOLCHAIN_ROOT, containment_available};

pub const PROTOCOL: &str = "ato.runtime-network/0";
/// The one execution environment a host advertises in Phase 1: itself.
pub const NATIVE_ENVIRONMENT: &str = "native";
/// Largest source archive a request carries inline.
pub const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

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
pub struct SourceInline {
    pub archive_base64: String,
    pub archive_digest: String,
    pub closure_ref: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatisfyBudget {
    pub max_attempts: u32,
    /// `first_pass`: stop at the first verified route. `all`: attempt every
    /// admissible candidate (still within `max_attempts`).
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SatisfyRequest {
    pub protocol: String,
    /// The K being satisfied: the effective Contract when a browser Contract
    /// is part of it, else the base Contract.
    pub contract_ref: String,
    pub base_contract_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_contract: Option<BrowserContractV0>,
    pub source: SourceInline,
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
    pub capsule_toml: String,
    #[serde(default)]
    pub browser_contract: Option<BrowserContractV0>,
    pub archive_digest: String,
    pub bindings: BTreeMap<String, String>,
    pub network: String,
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
    /// `false` for every refusal before execution.
    pub execution_started: bool,
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
    pub failure: Option<AttemptFailureWire>,
    /// The Formation attempt as the Runtime recorded it: verification,
    /// realization evidence, browser receipt.
    pub formation_attempt: Option<serde_json::Value>,
    pub verifier_receipts: Vec<serde_json::Value>,
    pub attestation: RuntimeAttestation,
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
        for language in ["python", "node"] {
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
    if planned.intent.lane == Lane::PythonProcess {
        requirements.push(Requirement {
            fact: "runtime.process".to_owned(),
            one_of: Some(vec!["true".to_owned()]),
        });
    }
    if !planned.plan.steps.is_empty() {
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
        .derivation
        .runtimes
        .iter()
        .chain(planned.intent.runtime.iter())
        .map(|(name, version)| format!("toolchain.{name}.{version}"))
        .collect();
    provisions.sort();
    provisions.dedup();
    (requirements, provisions)
}

// ──────────────────────────────────────────────────────────── requesting

pub struct Submission {
    pub request: SatisfyRequest,
}

/// Freeze `dir`, plan every authorized route against it, and assemble the
/// request. Every route must bind to the same Contract: one K, several Ds.
#[allow(clippy::too_many_arguments)]
pub fn prepare_submission(
    dir: &Path,
    routes: &[PathBuf],
    browser_contract: Option<BrowserContractV0>,
    work_root: &Path,
    constraint: RuntimeConstraintWire,
    policy: SatisfyPolicy,
    budget: SatisfyBudget,
) -> Result<Submission> {
    let archive = snapshot_directory(dir)?;
    if archive.len() > MAX_SOURCE_BYTES {
        bail!("the source is larger than {MAX_SOURCE_BYTES} bytes");
    }
    let archive_digest = digest(&archive);
    std::fs::create_dir_all(work_root)?;
    let frozen = freeze_archive(
        archive.clone(),
        &archive_digest,
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
            contract_ref,
            base_contract_ref,
            browser_contract,
            source: SourceInline {
                archive_base64: base64::engine::general_purpose::STANDARD.encode(&archive),
                archive_digest,
                closure_ref: frozen.closure_ref.as_str().to_owned(),
            },
            authorized_derivations: authorized,
            runtime_constraint: constraint,
            bindings: BTreeMap::new(),
            policy,
            budget,
        },
    })
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
        self.send(self.http.post(self.url("/attempts/claim")))
    }

    pub fn source(&self, attempt_id: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(self.url(&format!("/attempts/{attempt_id}/source")))
            .bearer_auth(&self.token)
            .send()?;
        if !response.status().is_success() {
            bail!(
                "the source for {attempt_id} is unavailable ({})",
                response.status()
            );
        }
        Ok(response.bytes()?.to_vec())
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
            let report = match client.source(&ticket.attempt_id) {
                Ok(archive) => execute_ticket(config, &ticket, archive),
                Err(error) => unavailable(&ticket, "source_unavailable", &format!("{error:#}")),
            };
            eprintln!(
                "[runtime-network] attempt {} → {}",
                ticket.attempt_id, report.outcome
            );
            // Free before the result lands: the result advances the request,
            // and the next ticket may be this Runtime's.
            busy.store(0, Ordering::Relaxed);
            report_now(&client, 0);
            // A result that cannot be delivered is retried; if it never is,
            // the coordinator expires the attempt as inconclusive.
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

/// Effect classes a Runtime executes unattended: an attempt of one of these
/// can fail and be retried elsewhere without anything leaking out of it.
pub fn is_disposable(effects: EffectClass) -> bool {
    matches!(
        effects,
        EffectClass::Pure | EffectClass::Idempotent | EffectClass::RecordSubstitutable
    )
}

fn effects_name(effects: EffectClass) -> String {
    serde_json::to_value(effects)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{effects:?}"))
}

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
        failure: Some(AttemptFailureWire {
            code: code.to_owned(),
            stage: "admission".to_owned(),
            message: crate::api::bounded_reason(message),
        }),
        formation_attempt: None,
        verifier_receipts: Vec::new(),
        attestation: RuntimeAttestation {
            execution_started: false,
            ..attestation
        },
    }
}

fn unavailable(ticket: &AttemptTicket, code: &str, message: &str) -> AttemptResultReport {
    let mut report = refused(ticket, attestation(), code, message);
    if let Some(failure) = report.failure.as_mut() {
        failure.stage = "runtime".to_owned();
    }
    report
}

/// The ticket's route, planned by this Runtime from the ticket's archive:
/// the canonical Derivation, its identities, effects and requirements. The
/// same planning a requester does, so the refs must agree.
fn plan_ticket(
    ticket: &AttemptTicket,
    archive: &[u8],
    work_root: &Path,
) -> Result<(PlannedCandidate, String)> {
    std::fs::create_dir_all(work_root)?;
    let frozen = freeze_archive(
        archive.to_vec(),
        &ticket.archive_digest,
        &std::path::absolute(work_root)?,
        SourceLimits::default(),
    )?;
    let evidence = detect(&frozen.root).context("detection failed")?;
    let draft: AuthoringDraft = parse_capsule_toml(&ticket.capsule_toml)
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
    Ok((planned, contract_ref))
}

/// Run one ticket through the local Formation machinery — frozen archive,
/// the authored route and nothing else, contained build, temporary
/// realization, typed and browser verification — and report what happened.
///
/// Before anything of the candidate runs, this Runtime re-establishes what
/// the ticket is from the archive and the route alone, and refuses it when:
/// the environment is not one it executes; it asks for bindings; its own
/// planning does not reach the ticket's refs; the canonical effect class is
/// not disposable (nothing here is run unattended that could leave an effect
/// behind). Platform, containment, process and browser-verifier admission
/// follow in the local driver, still before execution.
pub fn execute_ticket(
    config: &ServeConfig,
    ticket: &AttemptTicket,
    archive: Vec<u8>,
) -> AttemptResultReport {
    let mut attested = attestation();
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
    let planned = plan_ticket(ticket, &archive, &attempt_root.join("preflight"));
    let _ = std::fs::remove_dir_all(attempt_root.join("preflight"));
    let (planned, contract_ref) = match planned {
        Ok(planned) => planned,
        Err(error) => {
            return refused(
                ticket,
                attested,
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
        return refused(
            ticket,
            attested,
            "ticket_mismatch",
            &format!(
                "planned {}/{contract_ref}, the ticket names {}/{}",
                planned.derivation_ref, ticket.derivation_ref, ticket.contract_ref
            ),
        );
    }
    if !is_disposable(planned.derivation.effects) {
        return refused(
            ticket,
            attested,
            "effect_policy",
            &format!(
                "the route's effect class is {}; a Runtime Network attempt runs unattended and \
                 may be retried elsewhere, so only pure, idempotent or record-substitutable \
                 routes are executed",
                effects_name(planned.derivation.effects)
            ),
        );
    }

    let network = match ticket.network.as_str() {
        "dependency-resolution" => FormationNetworkPolicy::DependencyResolution,
        _ => FormationNetworkPolicy::Denied,
    };
    let request = FormationRequest {
        initial_condition: InitialCondition::Archive {
            bytes: archive,
            expected_digest: ticket.archive_digest.clone(),
        },
        // The route the ticket names, verbatim. No preset, no other route.
        contract: ContractSource::Authored {
            toml: ticket.capsule_toml.clone(),
        },
        runtime: RuntimeConstraint::Exact {
            runtime_id: "local".to_owned(),
        },
        policy: FormationPolicy { network },
        budget: SearchBudget { max_attempts: 1 },
        browser_contract: ticket.browser_contract.clone(),
    };
    let env = LocalFormation {
        work_root: attempt_root.clone(),
        out_dir: config.out_dir.clone(),
        shim: config.shim.clone(),
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
        // A Runtime Network attempt is never verified outside the verifier
        // sandbox, whatever the worker was started with.
        browser_verifier: config
            .browser_verifier
            .clone()
            .filter(BrowserVerifierCommand::is_contained),
        browser_budget: Default::default(),
    };
    // Evidence names this Runtime as the ticket does, not as `local`.
    let result = local::run_as(
        &request,
        &env,
        &local::local_executor(&request, &env),
        &ticket.runtime_id,
    );
    let _ = std::fs::remove_dir_all(&attempt_root);
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            return AttemptResultReport {
                attestation: attested,
                ..unavailable(ticket, "formation_error", &format!("{error:#}"))
            };
        }
    };
    let (attempts, materialization_ref) = match &result {
        FormationResult::Formed {
            attempts,
            verified_routes,
            ..
        } => (
            attempts,
            verified_routes
                .first()
                .map(|r| r.materialization_ref.clone()),
        ),
        FormationResult::NoVerifiedRoute { attempts, .. } => (attempts, None),
    };
    let Some(attempt) = attempts.first() else {
        return AttemptResultReport {
            attestation: attested,
            ..unavailable(
                ticket,
                "formation_empty",
                "the Formation produced no attempt",
            )
        };
    };
    // Admission in the local driver (platform, containment, process,
    // browser verifier) refuses before anything runs.
    attested.execution_started = attempt.status != AttemptStatus::Filtered;

    // Planned twice from the same bytes; still, never report a pass under
    // refs other than the ticket's.
    if attempt.derivation_ref.as_deref() != Some(ticket.derivation_ref.as_str())
        || attempt.contract_ref.as_deref() != Some(ticket.contract_ref.as_str())
    {
        let mut report = refused(
            ticket,
            attested.clone(),
            "ticket_mismatch",
            &format!(
                "planned {:?}/{:?}, the ticket names {}/{}",
                attempt.derivation_ref,
                attempt.contract_ref,
                ticket.derivation_ref,
                ticket.contract_ref
            ),
        );
        report.attestation.execution_started = attested.execution_started;
        report.formation_attempt = serde_json::to_value(attempt).ok();
        return report;
    }

    let failure_code = attempt.failure.as_ref().map(|f| f.code.as_str());
    let outcome = match (attempt.status, failure_code) {
        (AttemptStatus::Verified, _) => "pass",
        // Not decided: the browser verification had no verdict, or this
        // Runtime refused the route after all.
        (_, Some("browser_contract_inconclusive")) | (AttemptStatus::Filtered, _) => "inconclusive",
        _ => "fail",
    };
    let mut receipts = Vec::new();
    if let Some(verification) = &attempt.verification {
        receipts.push(serde_json::json!({ "kind": "http_contract", "verification": verification }));
    }
    if let Some(browser) = &attempt.browser_verification {
        receipts.push(serde_json::json!({ "kind": "browser_contract", "receipt": browser }));
    }
    AttemptResultReport {
        fence: ticket.fence,
        outcome: outcome.to_owned(),
        contract_ref: attempt.contract_ref.clone(),
        derivation_ref: attempt.derivation_ref.clone(),
        materialization_ref,
        failure: attempt.failure.as_ref().map(|f| AttemptFailureWire {
            code: f.code.clone(),
            stage: f.stage.clone(),
            message: f.message.clone(),
        }),
        formation_attempt: serde_json::to_value(attempt).ok(),
        verifier_receipts: receipts,
        attestation: attested,
    }
}
