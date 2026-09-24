//! Local Formation — Phase 1.
//!
//! `I -- D on this Runtime --> C`, then `C |= K` observed for real
//! (ADR-019): candidates are built and, for a process lane, realized
//! temporarily on the local Runtime so every Contract observation is decided
//! before `Formed` is reported.
//!
//! The Initial Condition is frozen once. The directory is snapshotted into
//! one archive, that archive goes through the same proof-state chain an
//! uploaded source does, and the tree it materializes is the only thing
//! detection, planning and building ever read — the measured `I` is the built
//! `I`, whatever happens to the directory afterwards.
//!
//! The driver holds no client for anywhere else. `--runtime local` is the
//! whole candidate space, so a failed attempt produces evidence and never a
//! fallback — the property is structural, not a policy the code remembers to
//! follow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use ato_formation::authoring::{AuthoringDraft, AuthoringProvenance};
use ato_formation::browser::{BrowserBudget, effective_contract_ref};
use ato_formation::capsule_toml::{parse_capsule_toml, read_capsule_toml};
use ato_formation::detect::{DetectorEvidence, detect};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::preset::candidates;
use ato_formation::request::{
    AttemptFailure, AttemptStatus, ContractSource, FormationAttempt, FormationNetworkPolicy,
    FormationRequest, FormationResult, InitialCondition, RuntimeConstraint, RuntimeProfile,
    VerifiedRoute,
};
use ato_formation::source::{DownloadedArchive, SourceClosureRef, SourceLimits};

use crate::attempt::{AttemptRequest, bounded, failure_of, run_attempt};
use crate::browser_verify::{BrowserVerification, BrowserVerifierCommand};
use crate::executor::{AttemptExecutor, ExecutedCandidate, LocalAttemptExecutor};
use crate::job::{copy_tree, digest, plan_candidate};
use crate::journal::AttemptJournal;
use crate::pack::pack_tree;
use crate::sandbox::{BuildLimits, NetworkPolicy, TOOLCHAIN_ROOT, containment_available};

/// What a local Formation needs beyond the request itself.
pub struct LocalFormation {
    /// Per-attempt scratch: staged workspace, build cache, bundle output.
    pub work_root: PathBuf,
    /// Verified artifacts are written here, content-addressed.
    pub out_dir: PathBuf,
    /// The calling binary, re-exec'd inside the build sandbox.
    pub shim: PathBuf,
    pub limits: BuildLimits,
    pub source_limits: SourceLimits,
    /// The browser verifier this Runtime has, if any. Used only when a
    /// request carries a browser Contract.
    pub browser_verifier: Option<BrowserVerifierCommand>,
    pub browser_budget: BrowserBudget,
}

static ATTEMPT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Run a Formation request against the local Runtime, end to end.
///
/// Hard errors — an unreadable directory, an authored `capsule.toml` that
/// does not parse — come back as `Err`. Everything a candidate did or did
/// not do comes back inside the result's attempts.
pub fn run(request: &FormationRequest, env: &LocalFormation) -> Result<FormationResult> {
    run_with_executor(request, env, &local_executor(request, env))
}

fn local_executor(request: &FormationRequest, env: &LocalFormation) -> LocalAttemptExecutor {
    LocalAttemptExecutor {
        shim: env.shim.clone(),
        network: network_policy(request.policy.network),
        limits: env.limits,
    }
}

/// [`run`] with the attempt executor supplied by the caller.
///
/// The seam a different execution boundary plugs into; the search loop, the
/// frozen Initial Condition and the verification rules stay the same.
pub fn run_with_executor(
    request: &FormationRequest,
    env: &LocalFormation,
    executor: &dyn AttemptExecutor,
) -> Result<FormationResult> {
    let RuntimeConstraint::Exact { runtime_id } = &request.runtime;
    if runtime_id != "local" {
        bail!("Phase 1 admits exactly one Runtime: --runtime local (got {runtime_id:?})");
    }
    run_as(request, env, executor, runtime_id)
}

/// [`run_with_executor`] on this machine, recording it in the attempt
/// evidence under `runtime_id`: the identity a Runtime Network ticket names
/// this Runtime by, rather than the anonymous `local`.
fn run_as(
    request: &FormationRequest,
    env: &LocalFormation,
    executor: &dyn AttemptExecutor,
    runtime_id: &str,
) -> Result<FormationResult> {
    let profile = probe_local_runtime();
    // Absolute from here on: these paths are bound into sandboxes whose
    // working directory is not this process's.
    let env = &LocalFormation {
        work_root: std::path::absolute(&env.work_root).context("cannot resolve the work root")?,
        out_dir: std::path::absolute(&env.out_dir).context("cannot resolve the out dir")?,
        shim: env.shim.clone(),
        limits: env.limits,
        source_limits: env.source_limits,
        browser_verifier: env.browser_verifier.clone(),
        browser_budget: env.browser_budget,
    };
    let browser = request
        .browser_contract
        .as_ref()
        .map(|contract| BrowserVerification {
            contract: contract.clone(),
            verifier: env.browser_verifier.clone(),
            budget: env.browser_budget,
        });

    let frozen = match &request.initial_condition {
        InitialCondition::LocalDirectory { path } => {
            freeze_local_source(path, &env.work_root, env.source_limits)?
        }
        InitialCondition::Archive {
            bytes,
            expected_digest,
        } => freeze_archive(
            bytes.clone(),
            expected_digest,
            &env.work_root,
            env.source_limits,
        )?,
    };
    let (closure_ref, source_root) = (frozen.closure_ref.clone(), frozen.root.clone());
    let evidence = detect(&source_root).context("detection failed")?;

    // Candidate Derivations: one authored route, or every preset the source
    // honestly fits. An authored document that fails to parse stops the whole
    // Formation — substituting a guess for a route somebody wrote is the one
    // failure mode the strict rule exists to prevent.
    let drafts: Vec<AuthoringDraft> = match &request.contract {
        ContractSource::Authored { toml } => {
            vec![parse_capsule_toml(toml).map_err(FormationFailure::from)?]
        }
        ContractSource::Infer => {
            match read_capsule_toml(&source_root).map_err(FormationFailure::from)? {
                Some(text) => vec![parse_capsule_toml(&text).map_err(FormationFailure::from)?],
                None => match candidates(&evidence) {
                    Ok(list) => list.into_iter().map(synthesize).collect(),
                    Err(mismatch) => {
                        return Ok(FormationResult::NoVerifiedRoute {
                            attempted_contract_refs: Vec::new(),
                            attempts: vec![FormationAttempt {
                                candidate: "detect".to_owned(),
                                attempt_id: None,
                                derivation_ref: None,
                                contract_ref: None,
                                runtime_id: runtime_id.to_owned(),
                                status: AttemptStatus::Filtered,
                                verification: None,
                                base_contract_ref: None,
                                realization: None,
                                browser_verification: None,
                                receipt: None,
                                failure: Some(AttemptFailure {
                                    code: mismatch.code.to_owned(),
                                    stage: FailureStage::Preset.as_str().to_owned(),
                                    message: mismatch.message,
                                }),
                            }],
                        });
                    }
                },
            }
        }
    };

    let network = network_policy(request.policy.network);
    // One request: every candidate of this run spends from it, and an
    // attempt whose end was never recorded stops the rest.
    let request_id = request_id();
    let journal = AttemptJournal::new(env.out_dir.join("attempt-records"));

    let mut attempts = Vec::new();
    for draft in drafts.iter().take(request.budget.max_attempts.max(1)) {
        let (attempt, formed) = attempt_one(
            draft,
            &closure_ref,
            &source_root,
            &evidence,
            &request_id,
            runtime_id,
            &profile,
            executor,
            &journal,
            network,
            browser.as_ref(),
            env,
        );
        let mut attempt = attempt;
        scrub_host_paths(&mut attempt, env);
        attempts.push(attempt);
        if let Some((contract_ref, route)) = formed {
            return Ok(FormationResult::Formed {
                contract_ref,
                verified_routes: vec![route],
                attempts,
            });
        }
    }

    Ok(FormationResult::NoVerifiedRoute {
        attempted_contract_refs: attempts
            .iter()
            .filter_map(|attempt| attempt.contract_ref.clone())
            .collect(),
        attempts,
    })
}

/// A new request id: each invocation of a local Formation is its own
/// request, explicitly asked for.
fn request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!(
        "local-{}-{}-{nanos}",
        std::process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Replace this worker's own paths in an attempt's failure message: the
/// attempt is evidence handed to whoever requested the Formation, and the
/// worker's scratch layout, home and temp directories are not theirs. The
/// guest paths a candidate sees (`/app`, `/src`) are kept.
fn scrub_host_paths(attempt: &mut FormationAttempt, env: &LocalFormation) {
    let Some(failure) = attempt.failure.as_mut() else {
        return;
    };
    let mut prefixes: Vec<(String, &str)> = vec![
        (env.work_root.display().to_string(), "<work>"),
        (env.out_dir.display().to_string(), "<out>"),
        (std::env::temp_dir().display().to_string(), "<tmp>"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        prefixes.push((
            std::path::PathBuf::from(home).display().to_string(),
            "<home>",
        ));
    }
    // Longest first: the work root usually lives inside the home.
    prefixes.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
    for (prefix, placeholder) in prefixes {
        let prefix = prefix.trim_end_matches('/');
        if prefix.len() > 1 {
            failure.message = failure.message.replace(prefix, placeholder);
        }
    }
}

/// One candidate end to end: plan it, then hand it to the common attempt
/// entry, and keep what that entry verified.
#[allow(clippy::too_many_arguments)]
fn attempt_one(
    draft: &AuthoringDraft,
    closure_ref: &SourceClosureRef,
    source_root: &Path,
    evidence: &DetectorEvidence,
    request_id: &str,
    runtime_id: &str,
    profile: &RuntimeProfile,
    executor: &dyn AttemptExecutor,
    journal: &AttemptJournal,
    network: NetworkPolicy,
    browser: Option<&BrowserVerification>,
    env: &LocalFormation,
) -> (FormationAttempt, Option<(String, VerifiedRoute)>) {
    let label = match &draft.provenance {
        AuthoringProvenance::Authored => "authored".to_owned(),
        AuthoringProvenance::PresetSynthesized { preset } => preset.to_string(),
    };
    let attempt_id = format!(
        "local-{}-{}",
        std::process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    let planned = match plan_candidate(
        draft,
        closure_ref,
        evidence,
        // No caller overrides locally: the request is the whole statement.
        BTreeMap::new(),
        "/app",
        &host_triple(),
    ) {
        Ok(planned) => planned,
        Err(error) => {
            return (
                FormationAttempt {
                    candidate: label,
                    attempt_id: Some(attempt_id),
                    derivation_ref: None,
                    contract_ref: None,
                    base_contract_ref: None,
                    runtime_id: runtime_id.to_owned(),
                    status: AttemptStatus::Failed,
                    verification: None,
                    realization: None,
                    browser_verification: None,
                    receipt: None,
                    failure: Some(failure_of(&error)),
                },
                None,
            );
        }
    };
    // The K this attempt verifies. A browser Contract is a condition of
    // success, so it is part of the identity; without one nothing changes.
    let contract_ref = effective_contract_ref(
        &planned.contract_ref,
        browser.map(|browser| &browser.contract),
    );
    let attempt_root = env.work_root.join(&attempt_id);
    let outcome = run_attempt(
        &AttemptRequest {
            request_id,
            attempt_id: &attempt_id,
            label: &label,
            candidate: &planned,
            contract_ref: &contract_ref,
            source_root,
            runtime_id,
            profile,
            network,
            browser,
            attempt_root: &attempt_root,
            shim: &env.shim,
        },
        executor,
        journal,
    );
    let mut attempt = outcome.attempt;
    let Some(executed) = outcome.verified else {
        return (attempt, None);
    };

    // Verified — only now does the artifact become worth keeping. Keeping it
    // is publication, not verification: a failure here leaves the receipt
    // and its verdicts exactly as they were.
    match store_candidate(&executed, &env.out_dir) {
        Ok(materialization_ref) => (
            attempt,
            Some((
                contract_ref,
                VerifiedRoute {
                    attempt_id,
                    derivation_ref: planned.derivation_ref.clone(),
                    runtime_id: runtime_id.to_owned(),
                    materialization_ref,
                },
            )),
        ),
        Err(error) => {
            attempt.status = AttemptStatus::Failed;
            attempt.failure = Some(AttemptFailure {
                code: "artifact_store_failed".to_owned(),
                stage: "publish".to_owned(),
                message: bounded(&format!("{error:#}")),
            });
            (attempt, None)
        }
    }
}

/// Keep the artifact of a verified candidate, content-addressed.
pub(crate) fn store_candidate(executed: &ExecutedCandidate, out_dir: &Path) -> Result<String> {
    match executed {
        ExecutedCandidate::Process { workspace_root } => {
            let packed = pack_tree(workspace_root)?;
            let reference = digest(&packed);
            let dir = out_dir.join("artifacts");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(format!("{}.tar", &reference["sha256:".len()..])),
                &packed,
            )?;
            Ok(reference)
        }
        ExecutedCandidate::StaticWeb { output } => {
            let dir = out_dir.join("bundles");
            std::fs::create_dir_all(&dir)?;
            let destination = dir.join(&output.manifest_digest["sha256:".len()..]);
            if !destination.exists() {
                // copy_tree fills an existing directory; it does not create
                // the root, and whether a file or a directory is listed first
                // is up to the filesystem.
                std::fs::create_dir_all(&destination)?;
                copy_tree(&output.bundle.bundle_root, &destination)?;
            }
            Ok(output.manifest_digest.clone())
        }
    }
}

/// The Initial Condition, frozen: one snapshot of the directory, verified
/// and materialized. Removed when the Formation ends.
pub(crate) struct FrozenSource {
    pub(crate) closure_ref: SourceClosureRef,
    pub(crate) root: PathBuf,
    scratch: PathBuf,
}

impl Drop for FrozenSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Snapshot a local directory once and turn it into the tree a Formation
/// builds from.
///
/// ```text
/// directory --tar once--> archive --digest--> DigestVerifiedArchive
///           --measure--> TreeVerifiedArchive --materialize--> frozen tree
/// ```
///
/// The archive is the codeload shape (a `source/` wrapper, no `.git`), so
/// the closure ref is the one an upload of the same tree would get and the
/// source module's rules — refused symlinks, path limits — apply unchanged.
/// After this returns the directory is never read again.
fn freeze_local_source(dir: &Path, work_root: &Path, limits: SourceLimits) -> Result<FrozenSource> {
    let archive = snapshot_directory(dir)?;
    let archive_digest = digest(&archive);
    freeze_archive(archive, &archive_digest, work_root, limits)
}

/// Snapshot a directory into the archive a Formation measures: the codeload
/// shape, no `.git`, symlinks kept for the source rules to decide.
pub fn snapshot_directory(dir: &Path) -> Result<Vec<u8>> {
    let directory = dir
        .canonicalize()
        .with_context(|| format!("cannot read {}", dir.display()))?;
    if !directory.is_dir() {
        bail!("{} is not a directory", directory.display());
    }
    tar_directory(&directory)
}

/// Verify an archive against the digest it was named by, measure its tree and
/// materialize it: the frozen Initial Condition.
pub(crate) fn freeze_archive(
    archive: Vec<u8>,
    archive_digest: &str,
    work_root: &Path,
    limits: SourceLimits,
) -> Result<FrozenSource> {
    let verified = DownloadedArchive::new(archive)
        .verify_archive_digest(archive_digest)
        .and_then(|archive| archive.verify_tree_digest(None, limits))
        .context("the directory is not a usable source")?;
    let closure_ref = verified
        .closure_ref("")
        .context("the directory is not a usable source")?;
    let scratch = work_root.join(format!(
        "source-{}-{}",
        std::process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("cannot create {}", scratch.display()))?;
    // Owned before materializing, so a failure part-way still removes it.
    let mut frozen = FrozenSource {
        closure_ref,
        root: PathBuf::new(),
        scratch: scratch.clone(),
    };
    frozen.root = verified
        .materialize(&scratch.join("tree"), "", limits)
        .context("the directory is not a usable source")?;
    Ok(frozen)
}

/// Pack a directory into an in-memory tar under a `source/` wrapper,
/// deterministically: sorted paths, zeroed metadata, no `.git`.
///
/// Symlinks are archived AS symlinks, not skipped: whether a link is
/// acceptable is the source module's decision, and it refuses them for every
/// source the same way.
fn tar_directory(root: &Path) -> Result<Vec<u8>> {
    let mut entries: Vec<(PathBuf, PathBuf, EntryKind)> = Vec::new();
    collect_tree(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut builder = tar::Builder::new(Vec::new());
    append_dir(&mut builder, "source")?;
    for (relative, absolute, kind) in entries {
        let path = format!("source/{}", relative.to_string_lossy());
        if kind == EntryKind::Dir {
            append_dir(&mut builder, &path)?;
        } else if kind == EntryKind::Symlink {
            let target = std::fs::read_link(&absolute)
                .with_context(|| format!("cannot read link {}", absolute.display()))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o777);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Symlink);
            builder
                .append_link(&mut header, &path, &target)
                .with_context(|| format!("cannot add {path} to the source archive"))?;
        } else {
            let bytes = std::fs::read(&absolute)
                .with_context(|| format!("cannot read {}", absolute.display()))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, &path, bytes.as_slice())
                .with_context(|| format!("cannot add {path} to the source archive"))?;
        }
    }
    Ok(builder.into_inner()?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Symlink,
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(PathBuf, PathBuf, EntryKind)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        // VCS metadata is never app content — the same reason a codeload
        // tarball does not carry it.
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        // A link is recorded, never followed: the source module decides.
        if metadata.is_symlink() {
            entries.push((relative, path, EntryKind::Symlink));
        } else if metadata.is_dir() {
            entries.push((relative, path.clone(), EntryKind::Dir));
            collect_tree(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.push((relative, path, EntryKind::File));
        }
    }
    Ok(())
}

fn append_dir(builder: &mut tar::Builder<Vec<u8>>, path: &str) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o755);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Directory);
    header.set_cksum();
    builder.append_data(&mut header, path, std::io::empty())?;
    Ok(())
}

fn network_policy(policy: FormationNetworkPolicy) -> NetworkPolicy {
    match policy {
        FormationNetworkPolicy::Denied => NetworkPolicy::Denied,
        FormationNetworkPolicy::DependencyResolution => NetworkPolicy::DependencyResolution,
    }
}

/// This machine as a Runtime: the facts a Formation filter can need.
///
/// Only what admission reads: platform, containment, and whether the
/// toolchain root a build provisions into exists. Measured, never assumed.
pub fn probe_local_runtime() -> RuntimeProfile {
    let mut capabilities = BTreeMap::new();
    capabilities.insert("platform.os".to_owned(), std::env::consts::OS.to_owned());
    capabilities.insert(
        "platform.arch".to_owned(),
        std::env::consts::ARCH.to_owned(),
    );
    capabilities.insert(
        "formation.containment".to_owned(),
        if containment_available() {
            "bwrap+landlock".to_owned()
        } else {
            "none".to_owned()
        },
    );
    if Path::new(TOOLCHAIN_ROOT).is_dir() {
        capabilities.insert(
            "formation.toolchain_root".to_owned(),
            TOOLCHAIN_ROOT.to_owned(),
        );
    }
    RuntimeProfile {
        runtime_id: "local".to_owned(),
        capabilities,
    }
}

/// The triple the local machine builds for. A workspace produced here is
/// host-native; cross-compiling is a different request.
pub(crate) fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

fn synthesize(preset: ato_formation::preset::AppPreset) -> AuthoringDraft {
    ato_formation::preset::synthesize_authoring(preset)
}
