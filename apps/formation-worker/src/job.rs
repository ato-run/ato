//! One Formation job, end to end.
//!
//! ```text
//! claim (takes the fence)
//!   -> acquire the pinned source, verify its bytes AND its tree
//!   -> detect -> Program Intent -> Effective Build Plan
//!   -> common attempt: admission, durable start, build, realize
//!   -> actual HTTP observation, Contract verification, receipt, stop
//!   -> publish the artifact
//!   -> offer the result; the control plane decides whether it counts
//! ```
//!
//! The worker owns a temporary candidate until verification and cleanup.
//! Publication and durable tenant Runs remain the caller's responsibility.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_formation::authoring::ContractIdentity;
use ato_formation::capsule_toml::{parse_capsule_toml, read_capsule_toml};
use ato_formation::detect::detect;
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::intent::Lane;
use ato_formation::preset::{
    apply_contract_identity, candidate_authoring, select_preset, synthesize_authoring_with_identity,
};
use ato_formation::request::{AttemptOutcomes, Outcome};
use ato_formation::source::{DownloadedArchive, SourceClosureRef, SourceLimits};
use ato_formation::verify::{ContractVerification, ContractVerificationReceipt};
use ato_runtime_attempt::{
    admission::EffectAuthorization,
    attempt::{AttemptRequest, Continuation, ReceiptContext, run_reserved_attempt},
    executor::{AttemptExecution, AttemptExecutor, ExecutedCandidate, LocalAttemptExecutor},
    formation_realizer::FormationRealizer,
    journal::{AttemptJournal, AttemptLedger, AttemptRecordState},
};

pub use ato_runtime_attempt::plan::{
    PlannedCandidate, copy_tree, digest, observe_candidate, plan_candidate, stage_workspace,
};

use crate::api::{FailureReport, FormationApi, PublishOutcome};
use crate::build::BuildAttempt;
use crate::sandbox::{BuildLimits, NetworkPolicy};

/// Fetching a pinned source. A trait so the whole job is testable offline.
pub trait SourceFetcher {
    /// Bytes of the archive this job's source names.
    ///
    /// The job id is a parameter because not every source can be fetched from
    /// the source alone: a GitHub commit has a URL anybody can derive, and an
    /// uploaded archive does not — those bytes are the control plane's, and the
    /// worker asks for them by naming the job it is running.
    fn fetch(&self, job_id: &str, source: &serde_json::Value) -> Result<Vec<u8>>;
}

/// The real fetcher, for every source kind the contract admits.
pub struct PinnedSourceFetcher {
    client: reqwest::blocking::Client,
    api_base: String,
    token: String,
}

impl PinnedSourceFetcher {
    pub fn new(client: reqwest::blocking::Client, api_base: String, token: String) -> Self {
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_owned(),
            token,
        }
    }

    fn github(&self, source: &serde_json::Value) -> Result<Vec<u8>> {
        let owner = source["owner"].as_str().context("source has no owner")?;
        let repository = source["repository"]
            .as_str()
            .context("source has no repository")?;
        let commit = source["resolved_commit_sha"]
            .as_str()
            .context("source is not pinned")?;
        // The PINNED commit, never the requested ref. A branch moves, and a
        // retry that followed it would build something else.
        let url = format!("https://codeload.github.com/{owner}/{repository}/tar.gz/{commit}");
        let response = self
            .client
            .get(&url)
            .send()?
            .error_for_status()
            .with_context(|| {
                format!(
                    "failed to fetch {}",
                    ato_formation::source::redact_url(&url)
                )
            })?;
        Ok(response.bytes()?.to_vec())
    }

    /// Bytes the requester already uploaded.
    ///
    /// Asked for BY JOB. The worker does not name the upload: the control
    /// plane reads the job's own source, checks the upload belongs to whoever
    /// submitted it, and serves the digest the job names. A worker that could
    /// choose the object would be choosing what it builds.
    fn uploaded(&self, job_id: &str) -> Result<Vec<u8>> {
        let url = format!(
            "{}/v1/internal/formation/jobs/{job_id}/source-archive",
            self.api_base
        );
        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()?
            .error_for_status()
            .with_context(|| {
                format!(
                    "failed to fetch the uploaded source for {job_id} from {}",
                    ato_formation::source::redact_url(&url)
                )
            })?;
        Ok(response.bytes()?.to_vec())
    }
}

impl SourceFetcher for PinnedSourceFetcher {
    fn fetch(&self, job_id: &str, source: &serde_json::Value) -> Result<Vec<u8>> {
        match source["kind"].as_str() {
            Some("git_hub") => self.github(source),
            Some("uploaded_archive") => self.uploaded(job_id),
            // `existing_source_closure` names a closure that is already
            // materialized; nothing needs fetching, and reaching here means a
            // caller asked for bytes that were never going to arrive.
            Some(other) => anyhow::bail!("source kind {other:?} has no fetcher"),
            None => anyhow::bail!("source names no kind"),
        }
    }
}

/// Packing a materialized tree into an artifact.
pub trait TreePacker {
    fn pack(&self, root: &Path) -> Result<Vec<u8>>;
}

/// What a finished job produced.
#[derive(Debug)]
pub struct JobOutcome {
    pub attempt: BuildAttempt,
    pub closure_ref: SourceClosureRef,
    pub materialization_ref: String,
    pub outcome: PublishOutcome,
}

pub struct JobContext<'a> {
    pub api: &'a FormationApi,
    pub fetcher: &'a dyn SourceFetcher,
    pub packer: &'a dyn TreePacker,
    pub work_root: &'a Path,
    pub shim: &'a Path,
    pub worker_id: &'a str,
    pub limits: BuildLimits,
    pub source_limits: SourceLimits,
}

/// Claim one named job, then run it.
pub fn run_job(
    context: &JobContext<'_>,
    job_id: &str,
    compute_id: &str,
    capsule_revision_id: &str,
) -> Result<JobOutcome> {
    let claimed = context.api.claim(job_id, context.worker_id)?;
    run_claimed_job(
        context,
        &BuildAttempt {
            job_id: job_id.to_owned(),
            attempt_id: claimed.attempt_id,
            attempt_fence: claimed.attempt_fence,
        },
        &claimed.job,
        compute_id,
        capsule_revision_id,
        claimed.operation_catalog_required,
    )
}

/// Run a job whose attempt is already claimed.
///
/// Split out from `run_job` because a queue-polling worker learns which job it
/// has BY claiming — there is no name to pass in beforehand. The attempt fence
/// still comes from the control plane either way; nothing here decides which
/// attempt is current.
pub fn run_claimed_job(
    context: &JobContext<'_>,
    attempt: &BuildAttempt,
    job: &serde_json::Value,
    compute_id: &str,
    capsule_revision_id: &str,
    operation_catalog_required: bool,
) -> Result<JobOutcome> {
    let journal = AttemptJournal::new(context.work_root.join("attempt-records"));
    run_claimed_job_with_ledger(
        context,
        attempt,
        job,
        compute_id,
        capsule_revision_id,
        operation_catalog_required,
        &journal,
    )
}

/// Injectable durable ledger, shared by production and failure-injection tests.
pub fn run_claimed_job_with_ledger(
    context: &JobContext<'_>,
    attempt: &BuildAttempt,
    job: &serde_json::Value,
    compute_id: &str,
    capsule_revision_id: &str,
    operation_catalog_required: bool,
    journal: &dyn AttemptLedger,
) -> Result<JobOutcome> {
    let attempt = attempt.clone();
    // Reserve historical execution before fetching source or refusing planning.
    // A redelivery must never overwrite prior uncertainty with a fresh refusal.
    let permit = journal
        .acquire(&attempt.job_id, &attempt.attempt_id)
        .map_err(|refusal| {
            HostedAttemptFailure::new(
                refusal.code(),
                "record",
                refusal.record_state(),
                None,
                None,
                Some(anyhow::anyhow!(refusal.message())),
            )
        })?;

    // ── source ──────────────────────────────────────────────────────────────
    let source = &job["source"];
    let subdirectory = source["subdirectory"].as_str().unwrap_or("");
    let job_id = job["job_id"].as_str().context("job names no id")?;
    let archive = context.fetcher.fetch(job_id, source)?;
    let archive_digest = digest(&archive);
    // Both checks, in order. Intact bytes of a DIFFERENT tree pass the first
    // and must not pass the second.
    let verified = DownloadedArchive::new(archive)
        .verify_archive_digest(&archive_digest)?
        .verify_tree_digest(
            source["expected_source_tree_digest"].as_str(),
            context.source_limits,
        )?;
    let closure_ref = verified.closure_ref(subdirectory)?;

    let attempt_root = context.work_root.join(&attempt.attempt_id);
    let source_root = verified.materialize(
        &attempt_root.join("source"),
        subdirectory,
        context.source_limits,
    )?;

    if operation_catalog_required {
        let operations = crate::operations::collect(&source_root)?;
        context
            .api
            .register_operation_source(&attempt.attempt_id, &operations)?;
    }

    // ── authoring ───────────────────────────────────────────────────────────
    //
    // Two frontends, one compiler. `capsule.toml` and an App Preset each
    // produce the same pair — a Contract draft and a Derivation draft — and
    // from here nothing can tell which door a build came through.
    //
    // The strict rule: a `capsule.toml` that is present is parsed strictly and
    // NEVER falls back to a Preset. An author who supplied a route and a
    // contract has said what they want; replacing that with a guess because
    // their document did not parse is the one failure mode this whole split
    // exists to prevent.
    let evidence = detect(&source_root).context("detection failed")?;
    // An explicit identity request is only a statement about how to bind. It is
    // never a requirement to change K: bind refuses when the statement and the
    // requirements disagree.
    let requested_identity = requested_contract_identity(job)?;
    let authored_toml = match job["authoring"]["manifest_toml"].as_str() {
        // The control plane's copy of the author's document wins over the one
        // in the tree: it is the one it accepted and recorded. Both go through
        // the same parser, so the two cannot come to mean different things.
        Some(text) => Some(text.to_owned()),
        None => read_capsule_toml(&source_root).map_err(FormationFailure::from)?,
    };
    let draft = match authored_toml {
        Some(text) => {
            let mut draft = parse_capsule_toml(&text).map_err(FormationFailure::from)?;
            draft.contract.identity =
                resolve_authored_identity(draft.contract.identity, requested_identity)?;
            draft
        }
        None => {
            let identity = requested_identity.unwrap_or(ContractIdentity::SourceBound);
            // Written for the person who uploaded the source. "No lane
            // matched" would name our dispatch instead of their problem — and
            // carrying the mismatch as a TYPE is what gets those words all the
            // way out, instead of them dying in a log nobody reads.
            let drafts = candidate_authoring(&evidence).map_err(FormationFailure::from)?;
            if matches!(drafts.first().map(|d| &d.provenance),
                Some(ato_formation::authoring::AuthoringProvenance::PresetSynthesized { preset })
                    if *preset == ato_formation::preset::NODE_STATIC_V2)
            {
                // The common plan admission enforces the declared network requirement.
                let draft = drafts
                    .into_iter()
                    .next()
                    .context("known-D frontend returned no draft")?;
                apply_contract_identity(draft, identity)
            } else {
                let preset = select_preset(&evidence).map_err(FormationFailure::from)?;
                // A Preset that installs from a registry cannot run under a job
                // whose policy denies the network. Saying so by name beats letting
                // `npm ci` fail three steps later with a DNS error the person who
                // uploaded a folder has no way to interpret.
                if preset.resolves_dependencies()
                    && job["policy"]["network"].as_str() != Some("dependency_resolution")
                {
                    bail!(
                        "{} needs to install its dependencies, which this lane does not allow",
                        preset.label()
                    );
                }
                synthesize_authoring_with_identity(preset, identity)
            }
        }
    };

    let authored_overrides: BTreeMap<String, String> = job["authoring"]["overrides"]
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|text| (key.clone(), text.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let guest_root = job["target"]["workspace_guest_root"]
        .as_str()
        .unwrap_or("/app");
    let triple = job["target"]["triple"]
        .as_str()
        .unwrap_or("x86_64-linux-gnu");

    let planned = plan_candidate(
        &draft,
        &closure_ref,
        &evidence,
        authored_overrides,
        guest_root,
        triple,
    )?;

    let network = match job["policy"]["network"].as_str() {
        Some("dependency_resolution") => NetworkPolicy::DependencyResolution,
        _ => NetworkPolicy::Denied,
    };
    let builder = HostedBuildExecutor {
        executor: LocalAttemptExecutor {
            shim: context.shim.to_owned(),
            network,
            limits: context.limits,
        },
        attempt: attempt.clone(),
    };
    let profile = crate::local::probe_local_runtime();
    let spec = planned.attempt_spec();
    let mut outcome = run_reserved_attempt(
        &AttemptRequest {
            request_id: &attempt.job_id,
            attempt_id: &attempt.attempt_id,
            label: "hosted",
            spec: &spec,
            contract_ref: &planned.contract_ref,
            runtime_id: context.worker_id,
            profile: &profile,
            authorization: EffectAuthorization::Unattended,
            network,
            browser: None,
            attempt_root: &attempt_root,
            continuation: Continuation::Stop,
            receipt: ReceiptContext::formation(),
            interrupt: None,
            control: None,
        },
        &FormationRealizer {
            planned: &planned,
            source_root: &source_root,
            builder: &builder,
            shim: context.shim,
            network,
        },
        Ok(permit),
    );
    let mut outcomes = outcome.attempt.outcomes.clone();
    // Preserve the original outcome even if writing the diagnostic sidecar fails.
    let persisted = persist_hosted_outcome(&attempt_root, &outcome.attempt, &outcomes);
    let executed = match outcome.verified.take() {
        Some(executed) => executed,
        None => {
            let (code, stage) = outcome
                .attempt
                .failure
                .as_ref()
                .map(|failure| (failure.code.as_str(), failure.stage.as_str()))
                .unwrap_or(("attempt_outcome_missing", "record"));
            return Err(HostedAttemptFailure::new(
                code,
                stage,
                outcome.attempt_record,
                outcome.attempt.receipt.as_ref(),
                Some(&outcomes),
                outcome.error.or_else(|| persisted.err()),
            )
            .with_failure(outcome.attempt.failure.clone())
            .into());
        }
    };
    persisted.map_err(|error| {
        HostedAttemptFailure::new(
            "outcome_record_failed",
            "record",
            outcome.attempt_record.clone(),
            outcome.attempt.receipt.as_ref(),
            Some(&outcomes),
            Some(error),
        )
    })?;
    let verification = outcome
        .attempt
        .verification
        .as_ref()
        .context("verified attempt has no verification")?;
    let receipt = outcome
        .attempt
        .receipt
        .as_ref()
        .context("verified attempt has no receipt")?;

    // Publishing owns no verification. Even a failed upload preserves the
    // satisfied receipt and all four independent outcomes in durable storage.
    let publication = (|| -> Result<(String, u64)> {
        match executed {
            ExecutedCandidate::Process { workspace_root } => {
                let packed = context.packer.pack(&workspace_root)?;
                Ok((context.api.publish_artifact(&packed)?, packed.len() as u64))
            }
            ExecutedCandidate::StaticWeb { output } => {
                let digests = output
                    .bundle
                    .receipt
                    .blobs
                    .iter()
                    .map(|b| b.digest.clone())
                    .collect::<Vec<_>>();
                context.api.publish_static_bundle(
                    &attempt.attempt_id,
                    &output.bundle.bundle_root,
                    &output.bundle.receipt.manifest_digest,
                    &digests,
                )?;
                Ok((
                    output.bundle.receipt.manifest_digest.clone(),
                    output.bundle.receipt.total_size,
                ))
            }
        }
    })();
    let (materialization_ref, artifact_bytes) = match publication {
        Ok(value) => {
            outcomes.publication = Outcome::succeeded();
            value
        }
        Err(error) => {
            outcomes.publication = Outcome::failed("artifact_store_failed");
            let _ = persist_hosted_outcome(&attempt_root, &outcome.attempt, &outcomes);
            return Err(HostedAttemptFailure::new(
                "artifact_store_failed",
                "publish",
                outcome.attempt_record.clone(),
                Some(receipt),
                Some(&outcomes),
                Some(error),
            )
            .into());
        }
    };
    persist_hosted_outcome(&attempt_root, &outcome.attempt, &outcomes)?;
    let contract_ref = &planned.contract_ref;
    let derivation_ref = &planned.derivation_ref;
    let result = compose_result(
        &attempt,
        &closure_ref,
        &planned,
        &materialization_ref,
        // The artifact's real size, whichever store it went to. Reporting the
        // packed length for a static bundle would say zero — the bundle is
        // never packed — and a receipt that under-reported size would be
        // describing something that does not exist.
        artifact_bytes,
        triple,
        guest_root,
        network,
        contract_ref,
        derivation_ref,
        verification,
        receipt,
        &outcomes,
    )?;
    std::fs::write(
        attempt_root.join("formation-result-v2.json"),
        serde_jcs::to_vec(&result)?,
    )?;
    let published = match context
        .api
        .publish_result(&result, compute_id, capsule_revision_id)
    {
        Ok(published) => published,
        Err(error) => {
            outcomes.publication = Outcome::failed("result_publication_unconfirmed");
            let _ = persist_hosted_outcome(&attempt_root, &outcome.attempt, &outcomes);
            return Err(HostedAttemptFailure::new(
                "result_publication_unconfirmed",
                "publish",
                outcome.attempt_record.clone(),
                Some(receipt),
                Some(&outcomes),
                Some(error),
            )
            .into());
        }
    };

    Ok(JobOutcome {
        attempt,
        closure_ref,
        materialization_ref,
        outcome: published,
    })
}

/// The Contract identity a job asks for, if any.
///
/// Parsed through the shared DTO so an unknown value is refused by name rather
/// than ignored: silently falling back to the default identity would bind a
/// different K than the caller asked for. Absent leaves the bytes of older
/// jobs exactly as they were.
fn requested_contract_identity(job: &serde_json::Value) -> Result<Option<ContractIdentity>> {
    let Some(value) = job
        .get("authoring")
        .and_then(|a| a.get("contract_identity"))
    else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let requested: ato_ipc::formation::FormationContractIdentityV1 =
        serde_json::from_value(value.clone()).map_err(|error| {
            FormationFailure::new(
                "contract_identity_invalid",
                FailureStage::Authoring,
                format!("authoring.contract_identity {value} is not a Contract identity: {error}"),
            )
        })?;
    Ok(Some(match requested {
        ato_ipc::formation::FormationContractIdentityV1::Behavioral => ContractIdentity::Behavioral,
        ato_ipc::formation::FormationContractIdentityV1::SourceBound => {
            ContractIdentity::SourceBound
        }
    }))
}

/// An authored manifest's stated identity outranks a job-level request.
///
/// A request may fill in an identity the manifest never stated, but it may not
/// contradict one that is written down: both are authorities, and silently
/// choosing one would bind a `K` that neither asked for.
fn resolve_authored_identity(
    stated: ContractIdentity,
    requested: Option<ContractIdentity>,
) -> Result<ContractIdentity> {
    let Some(requested) = requested else {
        return Ok(stated);
    };
    match stated {
        ContractIdentity::Unspecified => Ok(requested),
        stated if stated == requested => Ok(stated),
        stated => Err(FormationFailure::new(
            "contract_identity_conflict",
            FailureStage::Authoring,
            format!(
                "the authored manifest declares Contract identity {stated}, but the job requests \
                 {requested}; refusing to let one silently override the other"
            ),
        )
        .into()),
    }
}

/// Carries common attempt facts through anyhow without classifying error prose.
#[derive(Debug)]
pub struct HostedAttemptFailure {
    pub report: FailureReport,
    pub attempt_failure: Option<ato_formation::request::AttemptFailure>,
    cause: Option<anyhow::Error>,
}
impl HostedAttemptFailure {
    fn new(
        code: &str,
        stage: &str,
        record: AttemptRecordState,
        receipt: Option<&ContractVerificationReceipt>,
        outcomes: Option<&AttemptOutcomes>,
        cause: Option<anyhow::Error>,
    ) -> Self {
        Self {
            report: FailureReport {
                code: code.into(),
                stage: stage.into(),
                // Detailed causes stay in operator logs; no raw build output on wire.
                message: format!("Hosted attempt stopped at {stage} ({code})"),
                attempt_record: Some(record),
                receipt: receipt.map(|r| serde_json::json!(r)),
                outcomes: outcomes.map(|o| serde_json::json!(o)),
            },
            cause,
            attempt_failure: None,
        }
    }
    fn with_failure(mut self, failure: Option<ato_formation::request::AttemptFailure>) -> Self {
        self.attempt_failure = failure;
        self
    }
}
impl std::fmt::Display for HostedAttemptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.report.message)
    }
}
impl std::error::Error for HostedAttemptFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_ref().map(|e| e.as_ref())
    }
}

struct HostedBuildExecutor {
    executor: LocalAttemptExecutor,
    attempt: BuildAttempt,
}
impl AttemptExecutor for HostedBuildExecutor {
    fn execute(&self, execution: &AttemptExecution<'_>) -> Result<ExecutedCandidate> {
        self.executor
            .execute_with_identity(execution, self.attempt.clone())
    }
}

fn persist_hosted_outcome(
    root: &Path,
    attempt: &ato_formation::request::FormationAttempt,
    outcomes: &AttemptOutcomes,
) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(root)?;
    let bytes = serde_json::to_vec_pretty(
        &serde_json::json!({ "schema":"ato.hosted-formation-outcome/1", "attempt":attempt, "outcomes":outcomes }),
    )?;
    let mut pending = tempfile::NamedTempFile::new_in(root)?;
    pending.write_all(&bytes)?;
    pending.as_file().sync_all()?;
    pending.persist(root.join("hosted-outcome.json"))?;
    #[cfg(unix)]
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compose_result(
    attempt: &BuildAttempt,
    closure_ref: &SourceClosureRef,
    planned: &PlannedCandidate,

    materialization_ref: &str,
    size_bytes: u64,
    triple: &str,
    guest_root: &str,
    network: NetworkPolicy,
    contract_ref: &str,
    derivation_ref: &str,
    verification: &ContractVerification,
    receipt: &ContractVerificationReceipt,
    outcomes: &AttemptOutcomes,
) -> Result<serde_json::Value> {
    let plan = &planned.plan;
    let serve = plan.serving(&planned.derivation);
    let public_env = plan.process_environment(&planned.derivation);
    let readiness_path = planned
        .contract
        .requirements
        .iter()
        .find(|r| r.verifier == ato_formation::authoring::HTTP_CONTRACT_VERIFIER)
        .and_then(|r| r.path.as_ref());
    // What the result actually exports is decided here, once, and the readiness
    // contract is derived from it. A Static Web candidate runs in the browser
    // and exports nothing, so naming the Contract's HTTP port there is a claim
    // about a port that does not exist — the control plane refuses it with
    // `readiness names port "http", which is not exported`. The HTTP Contract
    // observation itself is unaffected: it is still verified for every lane.
    let exported_ports = planned
        .derivation
        .ports
        .iter()
        .filter(|port| plan.lane.is_process() && port.from == serve.id)
        .map(|port| {
            serde_json::json!({
                "name": "http", "protocol": "http", "guest_port": port.guest_port,
            })
        })
        .collect::<Vec<_>>();
    let readiness_contracts = if exported_ports
        .iter()
        .any(|port| port.get("name").and_then(serde_json::Value::as_str) == Some("http"))
    {
        readiness_path
            .map(|path| {
                vec![serde_json::json!({
                    "kind": "http", "port_name": "http", "path": path,
                })]
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let (kind, candidate) = match plan.lane {
        Lane::PythonProcess | Lane::Process => (
            "process_workspace",
            serde_json::json!({
                "kind": "process",
                "argv": serve.argv,
                "cwd_relative": serve.cwd,
                "public_env": public_env,
                "workspace_materialization_ref": materialization_ref,
            }),
        ),
        Lane::StaticWeb => (
            "static_web",
            serde_json::json!({
                "kind": "static_browser",
                "materialization_ref": materialization_ref,
                "entry_path": serve.entry.clone().unwrap_or_else(|| "index.html".to_owned()),
                "spa_fallback": serve.spa_fallback.unwrap_or(false),
            }),
        ),
    };

    // Artifact lookup only. This key never transfers verification to another
    // attempt, and v2 cannot collide with v1's differently-defined namespace.
    let inputs_digest = digest(&serde_jcs::to_vec(&serde_json::json!({
        "source_closure_ref": closure_ref.as_str(), "derivation_ref": derivation_ref,
        "target": { "triple": triple, "workspace_guest_root": plan.workspace_guest_root },
    }))?);
    let formation_key = format!("v2:{inputs_digest}");

    Ok(serde_json::json!({
        "protocol": "ato.formation-result.v2",
        "job_id": attempt.job_id,
        "attempt_id": attempt.attempt_id,
        "attempt_fence": attempt.attempt_fence,
        "status": "succeeded",
        "formation_key": formation_key,
        "source_revision_ref": format!("srev_{}", &closure_ref.as_str()[7..23]),
        "source_closure_ref": closure_ref.as_str(),
        "compute_schema_ref": digest(
            format!("{derivation_ref}|{materialization_ref}").as_bytes(),
        ),
        "materializations": [{
            "kind": kind,
            "content_ref": materialization_ref,
            // Per lane: a consumer that reached for the wrong reader would
            // find a tar where it expected a bundle, and say so unhelpfully.
            "media_type": match plan.lane {
                Lane::PythonProcess | Lane::Process => "application/vnd.ato.process-workspace.v1+tar",
                Lane::StaticWeb => "application/vnd.ato.static-web-bundle.v1",
            },
            "digest": materialization_ref,
            "size_bytes": size_bytes,
            "target": { "triple": triple, "workspace_guest_root": guest_root },
            "compatibility": if plan.lane == Lane::StaticWeb { serde_json::json!({"evaluator":"browser"}) } else { serde_json::json!({"os":"linux"}) },
            "producer": concat!("ato-formation-worker/", env!("CARGO_PKG_VERSION")),
        }],
        "runtime_requirements": plan.toolchains.iter().map(|(name, version)| serde_json::json!({
            "name": name, "version": version, "resolution": "authored",
        })).collect::<Vec<_>>(),
        "realization_candidates": [candidate],
        "exported_ports": exported_ports,
        "readiness_contracts": readiness_contracts,
        "state_slot_declarations": planned.derivation.state.iter().filter(|_| plan.lane.is_process()).map(|slot| serde_json::json!({
            "state_key": slot.id, "mount_target": slot.mount,
            "access": "read_write", "protocol": "ato.state.filesystem@1",
        })).collect::<Vec<_>>(),
        "binding_requirements": [],
        "provenance": {
            "formation_service_version": env!("CARGO_PKG_VERSION"),
            "builder_catalog_version": "catalog-2026-09",
            "policy_version": "formation-policy-v1",
            "network_policy": match network {
                NetworkPolicy::Denied => "denied",
                NetworkPolicy::DependencyResolution => "dependency_resolution",
                NetworkPolicy::Scoped => "phase_scoped_connect",
            },
            // What was ACTUALLY in force, so a later reader can tell whether
            // this artifact was built under isolation.
            "isolation": if plan.lane == Lane::StaticWeb && plan.actions.is_empty() { "static-loopback;no-build" } else { network.provenance() },
            "field_origins": {},
        },
        "diagnostics": [],
        // The Capsule this build formed, the route that formed it, and what
        // was actually decided about the Contract. Carried on the existing
        // result rather than in a new store: a receipt nobody can read beside
        // the thing it describes is evidence in name only.
        "contract_ref": contract_ref,
        "derivation_ref": derivation_ref,
        "contract_verification": {
            "summary": verification.summary(),
            "observations": verification
                .verdicts
                .iter()
                .map(|verdict| {
                    let (result, detail) = match &verdict.outcome {
                        ato_formation::verify::ObservationOutcome::Satisfied => {
                            ("satisfied", String::new())
                        }
                        ato_formation::verify::ObservationOutcome::Deferred { by } => {
                            ("deferred", by.clone())
                        }
                        ato_formation::verify::ObservationOutcome::Failed { code, detail } => {
                            (*code, detail.clone())
                        }
                    };
                    serde_json::json!({ "id": verdict.id, "result": result, "detail": detail })
                })
                .collect::<Vec<_>>(),
        },
        "deterministic_inputs_digest": inputs_digest,
        "verification_receipt": receipt,
        "outcomes": outcomes,
    }))
}

/// Where a job's scratch lives, so a caller can clean it up.
pub fn attempt_root(work_root: &Path, attempt_id: &str) -> PathBuf {
    work_root.join(attempt_id)
}

/// Refuse a job this worker cannot honour before claiming it.
pub fn preflight(job: &serde_json::Value) -> Result<()> {
    if job["policy"]["publish_enabled"].as_bool() == Some(true)
        && job["policy"]["network"].as_str() == Some("dependency_resolution")
    {
        // ADR-018. The contract refuses this too; refusing here as well means
        // a worker running against an older control plane still will not do it.
        bail!(
            "refusing a publish-enabled job that needs the network: this worker cannot confine a \
             networked untrusted source (ADR-018)"
        );
    }
    Ok(())
}

#[cfg(test)]
mod result_v2_readiness_tests {
    //! A readiness contract names a port the result exports.
    //!
    //! The control plane refuses a result whose readiness names a port missing
    //! from `exported_ports`, and a Static Web result exports none. These tests
    //! hold the builder to that: readiness is derived from the exported ports
    //! rather than from the Contract's HTTP requirement alone, while the HTTP
    //! observation itself stays verified for every lane.
    use std::collections::BTreeMap;

    use ato_formation::authoring::HTTP_CONTRACT_VERIFIER;
    use ato_formation::capsule_toml::parse_capsule_toml;
    use ato_formation::detect::detect;
    use ato_formation::intent::Lane;
    use ato_formation::preset::{AppPreset, synthesize_authoring};
    use ato_formation::request::AttemptOutcomes;
    use ato_formation::source::{RESOLVER_CONTRACT_V1, SourceClosureRef};
    use ato_formation::verify::{
        ContractVerification, ContractVerificationReceipt, ObservationOutcome, ObservationVerdict,
        VerificationTargetKind,
    };
    use serde_json::{Value, json};

    use super::*;

    const PROCESS_ROUTE: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/app/server.py"]

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[contract.require]]
id = "health"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/health"

[contract.require.expect]
status = 200
"#;

    fn closure() -> SourceClosureRef {
        SourceClosureRef::derive(
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "",
            RESOLVER_CONTRACT_V1,
        )
        .expect("closure")
    }

    fn static_plan() -> PlannedCandidate {
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("index.html"), "<h1>static</h1>").expect("write");
        let evidence = detect(source.path()).expect("detects");
        plan_candidate(
            &synthesize_authoring(AppPreset::StaticFiles),
            &closure(),
            &evidence,
            BTreeMap::new(),
            "/app",
            "wasm32-browser",
        )
        .expect("static plan")
    }

    fn process_plan() -> PlannedCandidate {
        let source = tempfile::tempdir().expect("tempdir");
        std::fs::write(source.path().join("server.py"), "print('serve')").expect("write");
        let draft = parse_capsule_toml(PROCESS_ROUTE).expect("process route parses");
        let evidence = detect(source.path()).expect("detects");
        plan_candidate(
            &draft,
            &closure(),
            &evidence,
            BTreeMap::new(),
            "/app",
            "x86_64-linux-gnu",
        )
        .expect("process plan")
    }

    fn compose(planned: &PlannedCandidate) -> Value {
        let attempt = BuildAttempt {
            job_id: "job".to_owned(),
            attempt_id: "attempt".to_owned(),
            attempt_fence: 1,
        };
        let verification = ContractVerification {
            verdicts: vec![ObservationVerdict {
                id: "root".to_owned(),
                outcome: ObservationOutcome::Satisfied,
            }],
        };
        let receipt = ContractVerificationReceipt::new(
            "sha256:2222222222222222222222222222222222222222222222222222222222222222",
            &planned.contract_ref,
            &planned.derivation_ref,
            VerificationTargetKind::FormationRuntime,
            verification.clone(),
        );
        compose_result(
            &attempt,
            &closure(),
            planned,
            "sha256:3333333333333333333333333333333333333333333333333333333333333333",
            1024,
            "x86_64-linux-gnu",
            "/app",
            NetworkPolicy::Denied,
            &planned.contract_ref,
            &planned.derivation_ref,
            &verification,
            &receipt,
            &AttemptOutcomes::not_run("test"),
        )
        .expect("composes")
    }

    #[test]
    fn a_static_result_exports_no_port_and_claims_no_readiness() {
        let planned = static_plan();
        assert_eq!(planned.plan.lane, Lane::StaticWeb);
        // The Contract still requires an HTTP observation; the point is that
        // the RESULT no longer claims a port that it does not export.
        assert!(
            planned
                .contract
                .requirements
                .iter()
                .any(|r| r.verifier == HTTP_CONTRACT_VERIFIER),
            "the static preset must keep its HTTP Contract observation"
        );

        let result = compose(&planned);
        assert_eq!(result["exported_ports"], json!([]));
        assert_eq!(result["readiness_contracts"], json!([]));
        assert_eq!(result["materializations"][0]["kind"], json!("static_web"));
        // Verification evidence is carried unchanged: this fix does not touch
        // what the Contract observes, only what the result says it exports.
        assert_eq!(
            result["contract_verification"]["observations"][0]["result"],
            json!("satisfied")
        );
    }

    #[test]
    fn a_process_result_readiness_names_an_exported_port() {
        let planned = process_plan();
        assert!(planned.plan.lane.is_process());

        let result = compose(&planned);
        let exported = result["exported_ports"]
            .as_array()
            .expect("exported_ports is a list");
        assert!(!exported.is_empty(), "a serving process exports its port");

        let readiness = result["readiness_contracts"]
            .as_array()
            .expect("readiness_contracts is a list");
        assert_eq!(readiness.len(), 1);
        assert_eq!(readiness[0]["kind"], json!("http"));
        let port_name = readiness[0]["port_name"]
            .as_str()
            .expect("readiness names a port");
        assert!(
            exported
                .iter()
                .any(|port| port["name"].as_str() == Some(port_name)),
            "readiness names {port_name:?}, which is not exported: {exported:?}"
        );
        // The readiness path is the serving HTTP path from the Contract.
        assert_eq!(readiness[0]["path"], json!("/health"));
    }
}

#[cfg(test)]
mod contract_identity_tests {
    //! The job-level identity request is metadata, and the authored manifest
    //! keeps its authority. These pin the two rules the worker applies before
    //! binding: absent stays byte-compatible, and a contradiction is a typed
    //! failure rather than a silent override.

    use super::*;
    use serde_json::json;

    #[test]
    fn a_job_without_an_identity_request_is_unchanged() {
        for job in [
            json!({}),
            json!({ "authoring": {} }),
            json!({ "authoring": { "contract_identity": null } }),
        ] {
            assert_eq!(
                requested_contract_identity(&job).expect("absent is not an error"),
                None,
                "{job}"
            );
        }
    }

    #[test]
    fn a_job_identity_is_read_in_the_wire_spelling() {
        for (wire, expected) in [
            ("behavioral", ContractIdentity::Behavioral),
            ("source-bound", ContractIdentity::SourceBound),
        ] {
            let job = json!({ "authoring": { "contract_identity": wire } });
            assert_eq!(
                requested_contract_identity(&job).expect("known identity"),
                Some(expected),
                "{wire}"
            );
        }
    }

    #[test]
    fn an_unknown_job_identity_is_refused_typed() {
        for wire in [json!("bogus"), json!("unspecified"), json!(7)] {
            let job = json!({ "authoring": { "contract_identity": wire } });
            let error = requested_contract_identity(&job).expect_err("unknown must fail");
            let failure = error
                .downcast_ref::<FormationFailure>()
                .expect("a typed formation failure");
            assert_eq!(failure.code, "contract_identity_invalid", "{wire}");
            assert_eq!(failure.stage, FailureStage::Authoring);
        }
    }

    #[test]
    fn an_authored_identity_may_be_filled_in_but_not_contradicted() {
        assert_eq!(
            resolve_authored_identity(ContractIdentity::Unspecified, None).unwrap(),
            ContractIdentity::Unspecified
        );
        assert_eq!(
            resolve_authored_identity(
                ContractIdentity::Unspecified,
                Some(ContractIdentity::Behavioral)
            )
            .unwrap(),
            ContractIdentity::Behavioral
        );
        assert_eq!(
            resolve_authored_identity(ContractIdentity::SourceBound, None).unwrap(),
            ContractIdentity::SourceBound
        );
        assert_eq!(
            resolve_authored_identity(
                ContractIdentity::SourceBound,
                Some(ContractIdentity::SourceBound)
            )
            .unwrap(),
            ContractIdentity::SourceBound
        );

        let error = resolve_authored_identity(
            ContractIdentity::SourceBound,
            Some(ContractIdentity::Behavioral),
        )
        .expect_err("contradiction must fail");
        let failure = error
            .downcast_ref::<FormationFailure>()
            .expect("a typed formation failure");
        assert_eq!(failure.code, "contract_identity_conflict");
        assert!(failure.message.contains("source-bound"), "{failure:?}");
        assert!(failure.message.contains("behavioral"), "{failure:?}");
    }
}
