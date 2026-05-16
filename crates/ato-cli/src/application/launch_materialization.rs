//! App Session Materialization v0 (RFC: APP_SESSION_MATERIALIZATION).
//!
//! This module is the Execute-phase analogue of
//! [`crate::application::build_materialization`]: it lets `ato app session
//! start` skip a fresh process spawn when an existing ready session matches
//! the current `LaunchSpec`.
//!
//! v0 scope (per RFC v0.2):
//!   * only `ato app session start` is wired in
//!   * `Reuse` and `Spawn` actions only — stale / digest-mismatch / unhealthy
//!     records are NOT killed, just left in place while a new record is
//!     written alongside
//!   * five mandatory reuse conditions: schema_version >= 2,
//!     launch_digest match, pid alive, process_start_time match, healthcheck
//!   * strict port-bound-by-pid verification is deferred to v1
//!
//! See `apps/ato/docs/rfcs/draft/APP_SESSION_MATERIALIZATION.md` §5–§9.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blake3::Hasher;
use fs2::FileExt;

use crate::app_control::{http_get_ok, session_root, StoredSessionInfo};

const LAUNCH_DIGEST_VERSION: &str = "ato-launch-digest-v2";
const LAUNCH_KEY_VERSION: &str = "ato-launch-key-v1";
const SESSION_RECORD_SCHEMA_VERSION: u32 = 2;
/// Session record filename prefix written by `write_session_record`. The
/// `session_id` itself starts with `ato-desktop-session-`, so per-session
/// JSON files land at `<session_root>/ato-desktop-session-<pid>.json`.
const SESSION_FILE_PREFIX: &str = "ato-desktop-session-";
const SESSION_FILE_SUFFIX: &str = ".json";

/// Origin of the current LaunchSpec — useful in diagnostics, not consumed by
/// digest input itself (the digest commits to the bytes, not where they
/// came from).
/// Canonical launch contract used for both digest computation and stale
/// detection.
#[derive(Debug, Clone)]
pub(crate) struct LaunchSpec {
    pub(crate) identity: LaunchIdentity,
    pub(crate) target_label: String,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    /// Logical (path-independent) working directory used for digest computation.
    ///
    /// For registry-installed capsules this is
    /// `"projection:<full_key>/source[:<relative>]"` so the digest does not
    /// change when `ATO_HOME` or the physical projection path changes.
    /// For local capsules this is the canonical manifest parent directory,
    /// which is already stable.
    pub(crate) logical_cwd: String,
    pub(crate) declared_port: Option<u16>,
    pub(crate) readiness_path: String,
    pub(crate) build_input_digest: Option<String>,
    pub(crate) lock_digest: Option<String>,
    pub(crate) toolchain_fingerprint: String,
}

/// Identity of the logical launch slot — independent of spec version. Used
/// to compose `launch_key` (§4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchIdentity {
    /// Remote handle / store scoped id form. The contained string is
    /// already normalized via `capsule_core::handle::normalize_capsule_handle`
    /// when possible, otherwise the raw handle as the user typed it.
    Handle(String),
    /// Local-path start. Stores the canonicalized manifest path so the same
    /// project under different surface paths (`.`, relative, absolute,
    /// symlink) collapses into one slot.
    LocalManifest(PathBuf),
}

impl LaunchIdentity {
    fn fingerprint_input(&self) -> String {
        match self {
            Self::Handle(handle) => format!("handle:{}", handle),
            Self::LocalManifest(path) => format!("local-manifest:{}", path.display()),
        }
    }
}

/// blake3-based content digest of the LaunchSpec. Matches RFC §2.2.
pub(crate) fn compute_launch_digest(spec: &LaunchSpec) -> String {
    let mut hasher = Hasher::new();
    update_text(&mut hasher, LAUNCH_DIGEST_VERSION);
    update_text(&mut hasher, &spec.identity.fingerprint_input());
    update_text(&mut hasher, &spec.target_label);
    update_text(&mut hasher, &spec.command);
    update_text(&mut hasher, "args");
    for arg in &spec.args {
        update_text(&mut hasher, arg);
    }
    update_text(&mut hasher, &spec.logical_cwd);
    update_text(
        &mut hasher,
        &spec
            .declared_port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    update_text(&mut hasher, &spec.readiness_path);
    update_text(
        &mut hasher,
        spec.build_input_digest.as_deref().unwrap_or("unknown"),
    );
    update_text(
        &mut hasher,
        spec.lock_digest.as_deref().unwrap_or("unknown"),
    );
    update_text(&mut hasher, &spec.toolchain_fingerprint);
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// blake3-based logical slot identifier. Stable across spec changes; only
/// changes when (identity, target) changes. Used as the per-slot lock key.
pub(crate) fn compute_launch_key(spec: &LaunchSpec) -> String {
    let mut hasher = Hasher::new();
    update_text(&mut hasher, LAUNCH_KEY_VERSION);
    update_text(&mut hasher, &spec.identity.fingerprint_input());
    update_text(&mut hasher, &spec.target_label);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn update_text(hasher: &mut Hasher, value: &str) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

// ---------------------------------------------------------------------------
// Per-launch_key file lock
// ---------------------------------------------------------------------------

/// RAII guard for the per-launch_key advisory file lock. Drop releases.
///
/// The lock file lives under `<session_root>/locks/<launch_key>.lock`.
/// Lifetime semantics: the lock MUST be held across lookup → spawn → readiness
/// wait → record write. Releasing earlier would let a concurrent caller
/// observe "no reusable record" and spawn a duplicate process (RFC §5.4).
pub(crate) struct LaunchLock {
    _file: File,
}

pub(crate) fn acquire_launch_lock(launch_key: &str) -> Result<LaunchLock> {
    let root = session_root()?;
    let lock_dir = root.join("locks");
    fs::create_dir_all(&lock_dir)
        .with_context(|| format!("failed to create lock dir {}", lock_dir.display()))?;
    // Strip the "blake3:" prefix; filesystem-friendly hex is enough.
    let key_basename = launch_key.trim_start_matches("blake3:");
    let lock_path = lock_dir.join(format!("{}.lock", key_basename));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open launch lock {}", lock_path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("failed to acquire launch lock {}", lock_path.display()))?;
    Ok(LaunchLock { _file: file })
}

// ---------------------------------------------------------------------------
// Reuse decision
// ---------------------------------------------------------------------------

/// Why a candidate session record was rejected for reuse. Surfaces as the
/// `prior_kind` extra on the Execute PHASE-TIMING line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PriorKind {
    /// Record exists but predates the v0 schema (schema_version is missing
    /// or `< 2`).
    SchemaTooOld,
    /// Record's `launch_digest` does not match the current LaunchSpec.
    DigestMismatch,
    /// Record's PID is no longer alive.
    StaleSession,
    /// PID is alive but its creation time disagrees with the record — the
    /// kernel has reused the PID for a different process.
    PidReuseDetected,
    /// PID + start_time + digest all match but the recorded URL does not
    /// answer a healthcheck request.
    UnhealthySession,
}

impl PriorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SchemaTooOld => "schema-too-old",
            Self::DigestMismatch => "digest-mismatch",
            Self::StaleSession => "stale-session",
            Self::PidReuseDetected => "pid-reuse-detected",
            Self::UnhealthySession => "unhealthy-session",
        }
    }
}

/// Outcome of consulting the materialization layer before spawning.
#[derive(Debug, Clone)]
pub(crate) enum ReuseDecision {
    /// A valid, healthy session record matched the current LaunchSpec — the
    /// caller should return its envelope without spawning.
    Reuse { record: Box<StoredSessionInfo> },
    /// No record satisfied all five reuse conditions. The caller should
    /// spawn a fresh process. `prior_kind` records the most-relevant
    /// rejection reason among the candidates inspected (None = no record
    /// existed at all).
    Spawn { prior_kind: Option<PriorKind> },
}

/// Inspect every session record under `<session_root>/desky-session-*.json`,
/// pick the candidate with matching `(identity, target_label)`, and decide
/// whether it is reusable for the current `LaunchSpec`.
///
/// Records with `schema_version < 2` (or unset) are observed but not
/// reusable; their presence still produces a `prior_kind=schema-too-old`
/// hint when no better candidate exists, so the diagnostic stream can show
/// "the session is already running but pre-dates v0 metadata, spawning a
/// fresh one to attach digests."
///
/// This function is idempotent (no fs writes). Callers must hold the
/// per-launch_key lock for the lookup → spawn → record-write sequence to
/// be race-free; see [`acquire_launch_lock`].
pub(crate) fn prepare_reuse_decision(
    spec: &LaunchSpec,
    launch_digest: &str,
) -> Result<ReuseDecision> {
    let root = session_root()?;
    if !root.exists() {
        return Ok(ReuseDecision::Spawn { prior_kind: None });
    }

    let entries =
        fs::read_dir(&root).with_context(|| format!("failed to enumerate {}", root.display()))?;

    let mut best_prior: Option<PriorKind> = None;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !file_name.starts_with(SESSION_FILE_PREFIX) || !file_name.ends_with(SESSION_FILE_SUFFIX)
        {
            continue;
        }

        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let record: StoredSessionInfo = match serde_json::from_slice(&bytes) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Slot match: same identity + target_label. Identity and target_label
        // are not in the record verbatim, so we approximate with the fields
        // we DO record (`handle` / `normalized_handle` / `manifest_path` /
        // `target_label`) and let the digest mismatch take care of the
        // rest.
        if !record_matches_slot(&record, spec) {
            continue;
        }

        // 1. schema_version >= 2
        let Some(schema_version) = record.schema_version else {
            best_prior = Some(best_prior.unwrap_or(PriorKind::SchemaTooOld));
            continue;
        };
        if schema_version < SESSION_RECORD_SCHEMA_VERSION {
            best_prior = Some(best_prior.unwrap_or(PriorKind::SchemaTooOld));
            continue;
        }

        // 2. launch_digest match
        let record_digest = record.launch_digest.as_deref().unwrap_or("");
        if record_digest != launch_digest {
            best_prior = Some(PriorKind::DigestMismatch);
            continue;
        }

        // 3. pid alive
        if !pid_alive(record.pid) {
            best_prior = Some(PriorKind::StaleSession);
            continue;
        }

        // 4. process_start_time match
        let recorded_start = record.process_start_time_unix_ms;
        let live_start = process_start_time_unix_ms(record.pid as u32);
        match (recorded_start, live_start) {
            (Some(a), Some(b)) if a == b => {}
            _ => {
                best_prior = Some(PriorKind::PidReuseDetected);
                continue;
            }
        }

        // 5. healthcheck — pick the URL by display category.
        let Some(url) = healthcheck_url_for(&record) else {
            best_prior = Some(PriorKind::UnhealthySession);
            continue;
        };
        if !healthcheck_ok(&url) {
            best_prior = Some(PriorKind::UnhealthySession);
            continue;
        }

        return Ok(ReuseDecision::Reuse {
            record: Box::new(record),
        });
    }

    Ok(ReuseDecision::Spawn {
        prior_kind: best_prior,
    })
}

fn record_matches_slot(record: &StoredSessionInfo, spec: &LaunchSpec) -> bool {
    if record.target_label != spec.target_label {
        return false;
    }
    match &spec.identity {
        LaunchIdentity::Handle(handle) => {
            // Match either the raw `handle` (as captured at start time) or
            // the normalized form. Either is acceptable as a slot match;
            // launch_digest still arbitrates on the canonical bytes.
            record.handle == *handle || record.normalized_handle == *handle
        }
        LaunchIdentity::LocalManifest(canonical) => {
            // Manifest path is recorded as Display string; compare textually.
            record.manifest_path == canonical.display().to_string()
        }
    }
}

fn healthcheck_url_for(record: &StoredSessionInfo) -> Option<String> {
    if let Some(guest) = &record.guest {
        return Some(guest.healthcheck_url.clone());
    }
    if let Some(web) = &record.web {
        // `web.healthcheck_url` is constructed at start_runtime_session time
        // as `local_url` itself; trust it here.
        return Some(web.healthcheck_url.clone());
    }
    // terminal / service categories are not HTTP-based; not eligible for
    // reuse in v0.
    None
}

/// Parse a `http://host:PORT/path` URL into (port, path). Returns `None`
/// for non-HTTP / non-127.0.0.1 / un-parseable URLs — those are not
/// reusable in v0.
fn parse_localhost_http_url(url: &str) -> Option<(u16, String)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port_str) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => return None,
    };
    if host != "127.0.0.1" && host != "localhost" {
        return None;
    }
    let port: u16 = port_str.parse().ok()?;
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    Some((port, path))
}

fn healthcheck_ok(url: &str) -> bool {
    let Some((port, path)) = parse_localhost_http_url(url) else {
        return false;
    };
    // Reuse the existing wait_for_http_ready inner probe — it already has
    // 1s read/write timeouts and treats transient I/O errors as "not yet."
    http_get_ok(port, &path)
}

// ---------------------------------------------------------------------------
// Post-spawn enrichment
// ---------------------------------------------------------------------------

/// Read the session record at `<session_root>/desky-session-<pid>.json` that
/// `start_runtime_session` / `start_guest_session` just wrote, attach the
/// schema=2 reuse fields, and rewrite atomically. Idempotent.
pub(crate) fn persist_after_spawn(
    pid: u32,
    launch_digest: &str,
    process_start_time_unix_ms: Option<u64>,
) -> Result<()> {
    let root = session_root()?;
    let path = root.join(format!("ato-desktop-session-{}.json", pid));
    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read fresh session record {}", path.display()))?;
    let mut record: StoredSessionInfo = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse fresh session record {}", path.display()))?;

    record.schema_version = Some(SESSION_RECORD_SCHEMA_VERSION);
    record.launch_digest = Some(launch_digest.to_string());
    record.process_start_time_unix_ms = process_start_time_unix_ms;

    let serialized = serde_json::to_vec_pretty(&record)
        .with_context(|| format!("failed to serialize enriched record {}", path.display()))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &serialized).with_context(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// PID liveness + start_time helpers
// ---------------------------------------------------------------------------

pub(crate) fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    unsafe {
        // kill(pid, 0): permission-checked liveness probe. Returns 0 if
        // the signal can be delivered (process exists) and we own it,
        // ESRCH if the PID is dead.
        if libc::kill(pid as libc::pid_t, 0) == 0 {
            return true;
        }
        let err = std::io::Error::last_os_error();
        // EPERM means "process exists but you can't signal it" — alive.
        matches!(err.raw_os_error(), Some(libc::EPERM))
    }
    #[cfg(not(unix))]
    {
        // Windows is not a v0 target for the start_time helper; fall back to
        // optimistic alive so digest / healthcheck still gate reuse there.
        true
    }
}

/// Best-effort process creation time (milliseconds since UNIX epoch).
/// Returns `None` when the platform is unsupported or the OS rejects the
/// query (e.g. the PID died between the alive check and this call). Callers
/// MUST treat `None` as "not reusable" — never as "match anything."
pub(crate) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
    platform::process_start_time_unix_ms(pid)
}

#[cfg(target_os = "macos")]
mod platform {
    use libc::{c_int, c_void, proc_pidinfo};

    // `proc_pidinfo(PROC_PIDTBSDINFO)` returns a `proc_bsdinfo` whose
    // `pbi_start_tvsec` / `pbi_start_tvusec` give the process start time.
    //
    // libc on macOS exposes `proc_pidinfo` and the constant but not the
    // struct shape, so we declare a minimal layout that matches the leading
    // fields we read. The full struct is documented in
    // `<sys/proc_info.h>`; we only need the two starttime fields, which sit
    // near the end.
    #[repr(C)]
    struct ProcBsdinfo {
        _pbi_flags: u32,
        _pbi_status: u32,
        _pbi_xstatus: u32,
        _pbi_pid: u32,
        _pbi_ppid: u32,
        _pbi_uid: u32,
        _pbi_gid: u32,
        _pbi_ruid: u32,
        _pbi_rgid: u32,
        _pbi_svuid: u32,
        _pbi_svgid: u32,
        _rfu_1: u32,
        _pbi_comm: [u8; 16],
        _pbi_name: [u8; 32],
        _pbi_nfiles: u32,
        _pbi_pgid: u32,
        _pbi_pjobc: u32,
        _e_tdev: u32,
        _e_tpgid: u32,
        _pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    const PROC_PIDTBSDINFO: c_int = 3;

    pub(super) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
        let mut info = std::mem::MaybeUninit::<ProcBsdinfo>::uninit();
        let size = std::mem::size_of::<ProcBsdinfo>() as c_int;
        let bytes = unsafe {
            proc_pidinfo(
                pid as c_int,
                PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr() as *mut c_void,
                size,
            )
        };
        if bytes != size {
            return None;
        }
        let info = unsafe { info.assume_init() };
        let secs = info.pbi_start_tvsec;
        let usecs = info.pbi_start_tvusec;
        secs.checked_mul(1_000)?.checked_add(usecs / 1_000)
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;

    pub(super) fn process_start_time_unix_ms(pid: u32) -> Option<u64> {
        // /proc/<pid>/stat field 22 is `starttime` in clock ticks since
        // boot. Combine with /proc/stat `btime` (boot time as unix seconds)
        // and the system clock-tick rate to get unix ms.
        let stat = fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        // The 2nd field (`comm`) is parenthesised and may contain spaces;
        // skip past the closing `)` before tokenising.
        let close = stat.rfind(')')?;
        let rest = &stat[close + 1..];
        let mut fields = rest.split_whitespace();
        // Now the next field is field 3 (`state`); starttime is field 22 of
        // the original stat, i.e. 19 fields from here (22 - 3 = 19, then
        // 0-indexed nth(18)).
        let starttime_jiffies: u64 = fields.nth(18)?.parse().ok()?;

        let stat_root = fs::read_to_string("/proc/stat").ok()?;
        let mut btime_secs: Option<u64> = None;
        for line in stat_root.lines() {
            if let Some(rest) = line.strip_prefix("btime ") {
                btime_secs = rest.trim().parse().ok();
                break;
            }
        }
        let btime_secs = btime_secs?;

        // SAFETY: sysconf(_SC_CLK_TCK) is a documented constant query.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if clk_tck <= 0 {
            return None;
        }
        let clk_tck = clk_tck as u64;

        let unix_secs = btime_secs.checked_add(starttime_jiffies / clk_tck)?;
        let frac_ms = ((starttime_jiffies % clk_tck) * 1_000) / clk_tck;
        unix_secs.checked_mul(1_000)?.checked_add(frac_ms)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub(super) fn process_start_time_unix_ms(_pid: u32) -> Option<u64> {
        // Windows / other: not supported in v0. Returning None makes the
        // reuse path treat any record as "PID-reuse-detected" which is the
        // safe default — the caller falls through to spawn.
        None
    }
}

// ---------------------------------------------------------------------------
// Manifest → LaunchSpec canonical-ization (v0)
// ---------------------------------------------------------------------------

/// Build a `LaunchSpec` from the data already resolved by
/// `SessionStartPhaseRunner::run_install` (RFC v0.2 §3.2 — the canonicalized
/// fields all come from existing v0.3 manifest plumbing).
///
/// `handle_input` is the user-facing handle the caller passed to
/// `ato app session start`. It is used both for slot identity (after
/// canonicalization, see [`LaunchIdentity`]) and as the back-up matcher in
/// [`record_matches_slot`].
///
/// `logical_cwd` is the path-independent working directory string to use in
/// the digest. For registry-installed capsules this should be
/// `"projection:<full_key>/source[:<relative>]"` so the digest is stable
/// regardless of where `ATO_HOME` lives. For local capsules pass the
/// canonical manifest parent directory.
pub(crate) fn canonicalize_launch_spec(
    handle_input: &str,
    target_label: &str,
    plan: &capsule_core::router::ManifestData,
    derived: &capsule_core::launch_spec::LaunchSpec,
    manifest_path: &Path,
    logical_cwd: Option<String>,
) -> Result<LaunchSpec> {
    let identity = canonicalize_identity(handle_input, manifest_path)?;
    let logical_cwd = logical_cwd.unwrap_or_else(|| {
        // Fallback: canonical physical working dir. Stable for local capsules;
        // for registry capsules the caller should always supply a logical cwd.
        derived
            .working_dir
            .canonicalize()
            .unwrap_or_else(|_| derived.working_dir.clone())
            .display()
            .to_string()
    });
    let readiness_path = "/".to_string();
    let toolchain_fingerprint =
        crate::application::build_materialization::toolchain_fingerprint_for_plan(plan);
    Ok(LaunchSpec {
        identity,
        target_label: target_label.to_string(),
        command: derived.command.clone(),
        args: derived.args.clone(),
        logical_cwd,
        declared_port: derived.port,
        readiness_path,
        // v0: build / lock digests are not yet plumbed end-to-end through
        // session start. We leave them as `None` (encoded as `unknown` in
        // the digest) so the launch_digest is stable; if/when build outputs
        // change, the build phase will rebuild before Execute runs and
        // toolchain_fingerprint or env will pick up the difference.
        build_input_digest: None,
        lock_digest: None,
        toolchain_fingerprint,
    })
}

fn canonicalize_identity(handle_input: &str, manifest_path: &Path) -> Result<LaunchIdentity> {
    use capsule_core::handle::normalize_capsule_handle;

    // Try the handle normaliser first; it understands store / GitHub /
    // canonical capsule:// forms. If it accepts the input, the result is
    // the canonical handle string we should slot on.
    if let Ok(normalized) = normalize_capsule_handle(handle_input) {
        return Ok(LaunchIdentity::Handle(normalized.display_string()));
    }

    // Otherwise treat as a local path: canonicalize the manifest_path so
    // distinct surface paths into the same project share a single slot.
    let canonical = manifest_path
        .canonicalize()
        .unwrap_or_else(|_| manifest_path.to_path_buf());
    Ok(LaunchIdentity::LocalManifest(canonical))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> LaunchSpec {
        LaunchSpec {
            identity: LaunchIdentity::Handle("samples/byok-ai-chat".to_string()),
            target_label: "app".to_string(),
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            logical_cwd: "projection:abc123def456/source".to_string(),
            declared_port: Some(3000),
            readiness_path: "/".to_string(),
            build_input_digest: None,
            lock_digest: None,
            toolchain_fingerprint: "node:20|darwin-arm64".to_string(),
        }
    }

    #[test]
    fn launch_digest_is_stable_for_identical_specs() {
        let a = compute_launch_digest(&sample_spec());
        let b = compute_launch_digest(&sample_spec());
        assert_eq!(a, b);
        assert!(a.starts_with("blake3:"));
    }

    #[test]
    fn launch_digest_changes_with_command() {
        let mut s = sample_spec();
        let before = compute_launch_digest(&s);
        s.command = "npm".to_string();
        s.args = vec!["run".to_string(), "start".to_string()];
        let after = compute_launch_digest(&s);
        assert_ne!(before, after);
    }

    #[test]
    fn launch_digest_changes_with_port() {
        let mut s = sample_spec();
        let before = compute_launch_digest(&s);
        s.declared_port = Some(3001);
        let after = compute_launch_digest(&s);
        assert_ne!(before, after);
    }

    #[test]
    fn launch_key_is_stable_across_spec_changes() {
        let mut s = sample_spec();
        let before = compute_launch_key(&s);
        // Changing command / port / logical_cwd MUST NOT change launch_key —
        // those represent spec changes within the same logical slot.
        s.command = "npm".to_string();
        s.declared_port = Some(4000);
        s.logical_cwd = "projection:other_key_here/source".to_string();
        let after = compute_launch_key(&s);
        assert_eq!(before, after);
    }

    #[test]
    fn launch_key_changes_with_target_label() {
        let mut s = sample_spec();
        let before = compute_launch_key(&s);
        s.target_label = "other".to_string();
        let after = compute_launch_key(&s);
        assert_ne!(before, after);
    }

    #[test]
    fn launch_key_changes_with_identity() {
        let mut s = sample_spec();
        let before = compute_launch_key(&s);
        s.identity = LaunchIdentity::Handle("publisher/other-app".to_string());
        let after = compute_launch_key(&s);
        assert_ne!(before, after);
    }

    #[test]
    fn pid_alive_rejects_zero_and_negative() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(-1));
    }

    #[test]
    fn pid_alive_accepts_self() {
        let me = std::process::id() as i32;
        assert!(pid_alive(me));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn process_start_time_unix_ms_returns_some_for_self() {
        let me = std::process::id();
        assert!(process_start_time_unix_ms(me).is_some());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn process_start_time_unix_ms_is_stable_within_a_process() {
        let me = std::process::id();
        let a = process_start_time_unix_ms(me).expect("first read");
        let b = process_start_time_unix_ms(me).expect("second read");
        assert_eq!(a, b);
    }

    #[test]
    fn prior_kind_string_round_trips() {
        for kind in [
            PriorKind::SchemaTooOld,
            PriorKind::DigestMismatch,
            PriorKind::StaleSession,
            PriorKind::PidReuseDetected,
            PriorKind::UnhealthySession,
        ] {
            assert!(!kind.as_str().is_empty());
            assert!(!kind.as_str().contains(' '));
        }
    }
}
