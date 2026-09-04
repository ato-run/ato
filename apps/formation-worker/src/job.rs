//! One Formation job, end to end.
//!
//! ```text
//! claim (takes the fence)
//!   -> acquire the pinned source, verify its bytes AND its tree
//!   -> detect -> Program Intent -> Effective Build Plan
//!   -> build, contained
//!   -> materialize (workspace for a process lane, bundle for a static one)
//!   -> publish the artifact
//!   -> offer the result; the control plane decides whether it counts
//! ```
//!
//! The worker owns nothing a tenant executes. No ComputeInstance, no Run, no
//! lease, no state revision — a build produces an artifact, and what happens to
//! that artifact afterwards is somebody else's decision.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ato_formation::detect::{FieldOrigins, detect};
use ato_formation::intent::{
    AuthoredOverrides, EffectiveBuildPlanV1, Lane, ProgramIntentV1, compile_build_plan,
    compile_intent,
};
use ato_formation::source::{DownloadedArchive, SourceClosureRef, SourceLimits};

use crate::api::{FormationApi, PublishOutcome};
use crate::build::{BuildAttempt, run_build};
use crate::sandbox::{BuildLimits, BuildSandbox, NetworkPolicy};

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
    pub intent_digest: String,
    pub plan_digest: String,
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

/// Run one job.
pub fn run_job(
    context: &JobContext<'_>,
    job_id: &str,
    compute_id: &str,
    capsule_revision_id: &str,
) -> Result<JobOutcome> {
    let claimed = context.api.claim(job_id, context.worker_id)?;
    let attempt = BuildAttempt {
        job_id: job_id.to_owned(),
        attempt_id: claimed.attempt_id.clone(),
        attempt_fence: claimed.attempt_fence,
    };
    let job = &claimed.job;

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

    let attempt_root = context.work_root.join(&claimed.attempt_id);
    let source_root = verified.materialize(
        &attempt_root.join("source"),
        subdirectory,
        context.source_limits,
    )?;

    // ── intent ──────────────────────────────────────────────────────────────
    let evidence = detect(&source_root).context("detection failed")?;
    let overrides = AuthoredOverrides(
        job["authoring"]["overrides"]
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|text| (key.clone(), text.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    );
    let guest_root = job["target"]["workspace_guest_root"]
        .as_str()
        .unwrap_or("/app");
    let triple = job["target"]["triple"]
        .as_str()
        .unwrap_or("x86_64-linux-gnu");

    let mut origins = FieldOrigins::new();
    let intent = compile_intent(&evidence, &overrides, guest_root, &mut origins)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let plan = compile_build_plan(&intent, guest_root, triple)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let intent_digest = intent
        .canonical_digest()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let plan_digest = plan
        .canonical_digest()
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    // ── build ───────────────────────────────────────────────────────────────
    let workspace_root = attempt_root.join("workspace");
    stage_workspace(&source_root, &workspace_root)?;

    let network = match job["policy"]["network"].as_str() {
        Some("dependency_resolution") => NetworkPolicy::DependencyResolution,
        _ => NetworkPolicy::Denied,
    };
    let cache_root = attempt_root.join("cache");
    std::fs::create_dir_all(&cache_root).context("cannot create the build cache")?;

    let built = run_build(
        &plan,
        attempt.clone(),
        &BuildSandbox {
            source_root: &source_root,
            workspace_root: &workspace_root,
            cache_root: Some(&cache_root),
            shim: context.shim,
            policy_host_path: &workspace_root.join(".ato-build-policy.json"),
            network,
            limits: context.limits,
        },
    )?;

    // ── materialize and publish ─────────────────────────────────────────────
    //
    // The two lanes produce different KINDS of artifact, and the difference is
    // not cosmetic. A process workspace is a tree the Runner mounts; a Static
    // Web materialization is a manifest, a receipt and content-addressed blobs
    // the browser evaluator already knows how to read. Packing a static site as
    // a workspace would publish an artifact nothing can serve.
    let output_root = crate::build::output_root(&built, &plan)?;
    let mut static_bundle: Option<crate::static_lane::StaticFormationOutput> = None;
    let packed = match intent.lane {
        Lane::PythonProcess => context.packer.pack(&output_root)?,
        Lane::StaticWeb => {
            let produced = crate::static_lane::materialize_static(
                &intent,
                &plan,
                // The WORKSPACE root, not the already-resolved output root:
                // the lane resolves `static.output_root` itself, and handing it
                // a resolved path made it look for `site/site`.
                &built.workspace_root,
                &attempt_root.join("bundle"),
                &format!("swm_{}", &claimed.attempt_id),
                // No canaries: this build redeems no secrets, so there is
                // nothing to scan for — and an empty list is NOT a claim that
                // the output was scanned.
                &[],
            )?;
            static_bundle = Some(produced);
            Vec::new()
        }
    };

    // A Static artifact's identity is its MANIFEST digest, and a process
    // artifact's is the digest of its packed workspace. They are published to
    // different stores for the same reason: the edge serves a static bundle by
    // reading its objects, while a Runner unpacks a workspace tar.
    let materialization_ref = match &static_bundle {
        Some(produced) => {
            let blob_digests: Vec<String> = produced
                .bundle
                .receipt
                .blobs
                .iter()
                .map(|blob| blob.digest.clone())
                .collect();
            context.api.publish_static_bundle(
                &claimed.attempt_id,
                &produced.bundle.bundle_root,
                &produced.bundle.receipt.manifest_digest,
                &blob_digests,
            )?;
            produced.bundle.receipt.manifest_digest.clone()
        }
        None => context.api.publish_artifact(&packed)?,
    };
    let artifact_bytes = match &static_bundle {
        Some(produced) => produced.bundle.receipt.total_size,
        None => packed.len() as u64,
    };

    let result = compose_result(
        &attempt,
        &closure_ref,
        &intent,
        &plan,
        &intent_digest,
        &plan_digest,
        &materialization_ref,
        // The artifact's real size, whichever store it went to. Reporting the
        // packed length for a static bundle would say zero — the bundle is
        // never packed — and a receipt that under-reported size would be
        // describing something that does not exist.
        artifact_bytes,
        triple,
        guest_root,
        network,
    )?;
    let outcome = context
        .api
        .publish_result(&result, compute_id, capsule_revision_id)?;

    Ok(JobOutcome {
        attempt,
        closure_ref,
        intent_digest,
        plan_digest,
        materialization_ref,
        outcome,
    })
}

/// Copy the source into the workspace the build writes to.
///
/// A copy rather than a bind: the source is read-only inside the sandbox on
/// purpose, and a build that edited it would produce an artifact whose closure
/// ref no longer describes it.
fn stage_workspace(source_root: &Path, workspace_root: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace_root)
        .with_context(|| format!("cannot create {}", workspace_root.display()))?;
    copy_tree(source_root, workspace_root)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        // A link in the source is not followed here either; the source module
        // already refused them, and this keeps the property local.
        if metadata.is_symlink() {
            continue;
        }
        let target = to.join(entry.file_name());
        if metadata.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if metadata.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compose_result(
    attempt: &BuildAttempt,
    closure_ref: &SourceClosureRef,
    intent: &ProgramIntentV1,
    plan: &EffectiveBuildPlanV1,
    intent_digest: &str,
    plan_digest: &str,
    materialization_ref: &str,
    size_bytes: u64,
    triple: &str,
    guest_root: &str,
    network: NetworkPolicy,
) -> Result<serde_json::Value> {
    let (kind, candidate) = match intent.lane {
        Lane::PythonProcess => (
            "process_workspace",
            serde_json::json!({
                "kind": "process",
                "argv": intent.launch_argv,
                "cwd_relative": intent.cwd_relative,
                "public_env": intent.public_env,
                "workspace_materialization_ref": materialization_ref,
            }),
        ),
        Lane::StaticWeb => (
            "static_web",
            serde_json::json!({
                "kind": "static_browser",
                "materialization_ref": materialization_ref,
                "entry_path": intent.static_entry_path.clone().unwrap_or_else(|| "index.html".to_owned()),
                "spa_fallback": intent.static_spa_fallback,
            }),
        ),
    };

    // The formation key: the digest of everything that decided this build.
    // Equal keys must mean equal outputs, or coalescing would serve one
    // requester another's answer.
    let formation_key = digest(
        format!(
            "{}|{}|{}|{}|{}",
            closure_ref.as_str(),
            intent_digest,
            plan_digest,
            triple,
            plan.workspace_guest_root
        )
        .as_bytes(),
    );

    Ok(serde_json::json!({
        "protocol": "ato.formation-result.v1",
        "job_id": attempt.job_id,
        "attempt_id": attempt.attempt_id,
        "attempt_fence": attempt.attempt_fence,
        "status": "succeeded",
        "formation_key": formation_key,
        "source_revision_ref": format!("srev_{}", &closure_ref.as_str()[7..23]),
        "source_closure_ref": closure_ref.as_str(),
        "program_intent_ref": intent_digest,
        "effective_build_plan_ref": plan_digest,
        "compute_schema_ref": digest(
            format!("{intent_digest}|{materialization_ref}").as_bytes(),
        ),
        "materializations": [{
            "kind": kind,
            "content_ref": materialization_ref,
            // Per lane: a consumer that reached for the wrong reader would
            // find a tar where it expected a bundle, and say so unhelpfully.
            "media_type": match intent.lane {
                Lane::PythonProcess => "application/vnd.ato.process-workspace.v1+tar",
                Lane::StaticWeb => "application/vnd.ato.static-web-bundle.v1+tar",
            },
            "digest": materialization_ref,
            "size_bytes": size_bytes,
            "target": { "triple": triple, "workspace_guest_root": guest_root },
            "compatibility": { "os": "linux" },
            "producer": concat!("ato-formation-worker/", env!("CARGO_PKG_VERSION")),
        }],
        "runtime_requirements": intent.runtime.iter().map(|(name, version)| serde_json::json!({
            "name": name, "version": version, "resolution": "authored",
        })).collect::<Vec<_>>(),
        "realization_candidates": [candidate],
        "exported_ports": intent.exported_ports.iter().map(|(name, port)| serde_json::json!({
            "name": name, "protocol": "http", "guest_port": port,
        })).collect::<Vec<_>>(),
        "readiness_contracts": intent.readiness_http_path.as_ref().map(|path| vec![serde_json::json!({
            "kind": "http", "port_name": "http", "path": path,
        })]).unwrap_or_default(),
        "state_slot_declarations": intent.state_slots.iter().map(|(key, mount)| serde_json::json!({
            "state_key": key, "mount_target": mount,
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
            },
            // What was ACTUALLY in force, so a later reader can tell whether
            // this artifact was built under isolation.
            "isolation": network.provenance(),
            "field_origins": {},
        },
        "diagnostics": [],
        "deterministic_inputs_digest": formation_key,
    }))
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
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
