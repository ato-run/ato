//! Integration coverage for the hermetic `ato install --from-local <dir>` path
//! (issue #561). These tests run the real `ato` binary against a deterministic
//! local capsule fixture inside an isolated `ATO_HOME`, with the Store/GitHub
//! base URLs pointed at an unroutable address to prove no remote fetch occurs.
//!
//! Scope: the install path + installed-state ledger. Launch is intentionally not
//! exercised here — `ato launch` bridges into the run pipeline, which needs a
//! runtime/session/Desktop context that a unit/integration test cannot stand up
//! cheaply. The tests assert that the installed record resolves (a real
//! installed app/profile/revision exists) and that the ledger is recorded, which
//! is what the hermetic Desktop relaunch smoke depends on.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::Command;
use serial_test::serial;

/// Base URL guaranteed to refuse/never-route: any accidental remote call fails
/// fast instead of reaching the real Store. `--from-local` must not touch it.
const UNROUTABLE_API: &str = "http://127.0.0.1:1";

fn basic_web_fixture() -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("local-install")
        .join("basic-web")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Run `ato install --from-local <dir>` in a fresh hermetic ATO_HOME.
/// Returns `(status_success, parsed_json_or_none, stdout, stderr)`.
fn run_from_local(
    scratch: &ScratchDir,
    source_dir: &Path,
    extra_env: &[(&str, &str)],
) -> (bool, Option<serde_json::Value>, String, String) {
    let ato_home = scratch.path().join("ato-home");
    let home = scratch.path().join("home");
    let output_dir = scratch.path().join("store");
    fs::create_dir_all(&ato_home).expect("create ATO_HOME");
    fs::create_dir_all(&home).expect("create HOME");

    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("ato"));
    cmd.current_dir(scratch.path())
        .env("ATO_HOME", &ato_home)
        .env("HOME", &home)
        .env("ATO_STORE_API_URL", UNROUTABLE_API)
        .env("ATO_GITHUB_API_BASE_URL", UNROUTABLE_API)
        .env("ATO_TELEMETRY", "0");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.args(["install", "--from-local"])
        .arg(source_dir)
        .arg("--output")
        .arg(&output_dir)
        .args(["--no-project", "--json"]);

    let output = cmd.output().expect("run ato install --from-local");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let parsed = parse_install_result_json(&stdout);
    (output.status.success(), parsed, stdout, stderr)
}

fn ato_home(scratch: &ScratchDir) -> PathBuf {
    scratch.path().join("ato-home")
}

/// The hermetic install must produce a usable installed app/profile/revision in
/// ATO_HOME, with an `ipk_<32hex>` install profile key and a `local/<slug>`
/// capsule handle, reusing the normal install pipeline.
#[test]
#[serial]
fn install_from_local_creates_installed_profile() {
    let scratch = ScratchDir::new("install_from_local_creates_installed_profile");
    let (ok, parsed, stdout, stderr) = run_from_local(&scratch, &basic_web_fixture(), &[]);
    assert!(ok, "install failed\nstdout={stdout}\nstderr={stderr}");

    let result = parsed.expect("install result JSON");
    assert_eq!(
        result.get("scoped_id").and_then(|v| v.as_str()),
        Some("local/basic-web"),
        "local fixture should get a local/<slug> handle"
    );
    let lifecycle = result
        .get("install_lifecycle")
        .and_then(|v| v.as_object())
        .expect("install_lifecycle object (installed profile must be registered)");
    let ipk = lifecycle
        .get("install_profile_key")
        .and_then(|v| v.as_str())
        .expect("install_profile_key");
    assert_ipk_shape(ipk);

    // The frozen revision directory must exist on disk.
    let current_revision_path = lifecycle
        .get("current_revision_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .expect("current_revision_path");
    assert!(
        current_revision_path.exists(),
        "current revision path missing: {}",
        current_revision_path.display()
    );
}

/// The output must surface the installed identity, including the
/// `install_profile_key: ipk_…` the operator needs for `ato launch <ipk>`.
#[test]
#[serial]
fn install_from_local_output_includes_ipk() {
    let scratch = ScratchDir::new("install_from_local_output_includes_ipk");
    let (ok, parsed, stdout, stderr) = run_from_local(&scratch, &basic_web_fixture(), &[]);
    assert!(ok, "install failed\nstdout={stdout}\nstderr={stderr}");
    let result = parsed.expect("install result JSON");
    let ipk = result
        .get("install_lifecycle")
        .and_then(|v| v.get("install_profile_key"))
        .and_then(|v| v.as_str())
        .expect("install_profile_key in JSON output");
    assert_ipk_shape(ipk);
    // No raw host fixture path should leak into user-facing identity fields.
    assert_eq!(
        result.get("capsule_id").and_then(|v| v.as_str()),
        Some("local:local/basic-web"),
        "capsule_id must be the stable local id, not a host path"
    );
}

/// The current revision must be recorded so relaunch can pin it.
#[test]
#[serial]
fn install_from_local_records_current_revision() {
    let scratch = ScratchDir::new("install_from_local_records_current_revision");
    let (ok, parsed, stdout, stderr) = run_from_local(&scratch, &basic_web_fixture(), &[]);
    assert!(ok, "install failed\nstdout={stdout}\nstderr={stderr}");
    let result = parsed.expect("install result JSON");
    let lifecycle = result.get("install_lifecycle").expect("install_lifecycle");
    let rev = lifecycle
        .get("install_revision_id")
        .and_then(|v| v.as_str())
        .expect("install_revision_id");
    assert!(
        rev.starts_with("rev_"),
        "install_revision_id should be a rev_ id: {rev}"
    );

    // The installed-state DB must hold the app + revision metadata for relaunch:
    // the launch ledger is keyed by (ipk, revision).
    let ipk = lifecycle
        .get("install_profile_key")
        .and_then(|v| v.as_str())
        .expect("ipk");
    let claims = read_launch_conditions(&ato_home(&scratch), ipk);
    assert!(
        claims
            .iter()
            .any(|c| c.install_revision_id.as_deref() == Some(rev)),
        "ledger must reference the recorded current revision {rev}; claims={claims:?}"
    );
}

/// The installed-state ledger baseline must be written (never bypassed): a
/// successful install always records at least the extraction-status marker.
#[test]
#[serial]
fn install_from_local_records_installed_state_baseline() {
    let scratch = ScratchDir::new("install_from_local_records_installed_state_baseline");
    let (ok, parsed, stdout, stderr) = run_from_local(&scratch, &basic_web_fixture(), &[]);
    assert!(ok, "install failed\nstdout={stdout}\nstderr={stderr}");
    let ipk = parsed
        .expect("install result JSON")
        .get("install_lifecycle")
        .and_then(|v| v.get("install_profile_key"))
        .and_then(|v| v.as_str())
        .expect("ipk")
        .to_string();

    let db_path = ato_home(&scratch)
        .join("state")
        .join("installed_state.sqlite3");
    assert!(
        db_path.exists(),
        "installed-state DB missing: {}",
        db_path.display()
    );
    let claims = read_launch_conditions(&ato_home(&scratch), &ipk);
    assert!(
        !claims.is_empty(),
        "launch-condition ledger must not be empty after install (SOT bypassed?)"
    );
    // basic-web declares a port, so a `port` launch condition must be extracted.
    assert!(
        claims.iter().any(|c| c.kind == "port"),
        "expected a port launch condition for the fixture's declared port; claims={claims:?}"
    );
}

/// `--from-local` must not perform any remote fetch: the Store/GitHub URLs are
/// unroutable, so a successful install proves the path stayed offline.
#[test]
#[serial]
fn install_from_local_does_not_fetch_remote() {
    let scratch = ScratchDir::new("install_from_local_does_not_fetch_remote");
    let (ok, _parsed, stdout, stderr) = run_from_local(&scratch, &basic_web_fixture(), &[]);
    assert!(
        ok,
        "install must succeed with unroutable Store/GitHub URLs (no remote fetch)\nstdout={stdout}\nstderr={stderr}"
    );
    // Defensive: the failure modes of a remote attempt would surface these.
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("Failed to connect to registry")
            && !combined.contains("install-draft")
            && !combined.contains("tarball"),
        "install appears to have attempted a remote fetch:\n{combined}"
    );
}

/// A missing source directory is a typed, actionable error — not a panic, and
/// nothing is installed. (The typed error is rendered to stdout as JSON in
/// `--json` mode, so the message is asserted against the combined output.)
#[test]
#[serial]
fn install_from_local_requires_existing_directory() {
    let scratch = ScratchDir::new("install_from_local_requires_existing_directory");
    let missing = scratch.path().join("does-not-exist");
    let (ok, _parsed, stdout, stderr) = run_from_local(&scratch, &missing, &[]);
    assert!(!ok, "install of a missing directory must fail");
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("does not exist"),
        "error must explain the directory is missing: {combined}"
    );
    // Typed (not the generic E999 "internal_error / please file a bug").
    assert!(
        !combined.contains("internal_error"),
        "invalid input must be a typed error, not E999 internal_error: {combined}"
    );
}

/// A directory without capsule.toml is a typed, actionable error.
#[test]
#[serial]
fn install_from_local_requires_capsule_toml() {
    let scratch = ScratchDir::new("install_from_local_requires_capsule_toml");
    let empty_dir = scratch.path().join("empty-capsule");
    fs::create_dir_all(&empty_dir).expect("create empty source dir");
    let (ok, _parsed, stdout, stderr) = run_from_local(&scratch, &empty_dir, &[]);
    assert!(!ok, "install without capsule.toml must fail");
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.contains("capsule.toml"),
        "error must mention the missing capsule.toml: {combined}"
    );
    assert!(
        !combined.contains("internal_error"),
        "missing manifest must be a typed error, not E999 internal_error: {combined}"
    );
}

/// The installed artifact is resolvable on the installed-app lifecycle layer —
/// the same layer `ato launch <ipk>` resolves before bridging into the run
/// pipeline. `ato revisions <ipk>` is a read-only resolution of that record
/// (current revision pinned), so it proves the install is launchable without
/// standing up a runtime/session/Desktop (full execution is documented as
/// needing Desktop/session context in
/// docs/dev-notes/hermetic-desktop-relaunch-smoke.md).
#[test]
#[serial]
fn install_from_local_then_launch_fixture_record_resolves() {
    let scratch = ScratchDir::new("install_from_local_then_launch_fixture_record_resolves");
    let (ok, parsed, stdout, stderr) = run_from_local(&scratch, &basic_web_fixture(), &[]);
    assert!(ok, "install failed\nstdout={stdout}\nstderr={stderr}");
    let result = parsed.expect("install result JSON");
    let lifecycle = result.get("install_lifecycle").expect("install_lifecycle");
    let ipk = lifecycle
        .get("install_profile_key")
        .and_then(|v| v.as_str())
        .expect("ipk")
        .to_string();
    let rev = lifecycle
        .get("install_revision_id")
        .and_then(|v| v.as_str())
        .expect("rev")
        .to_string();

    // `ato revisions <ipk> --json` reads the installed-app record (no execution).
    let ato_home = ato_home(&scratch);
    let home = scratch.path().join("home");
    let output = Command::new(assert_cmd::cargo::cargo_bin!("ato"))
        .current_dir(scratch.path())
        .env("ATO_HOME", &ato_home)
        .env("HOME", &home)
        .env("ATO_STORE_API_URL", UNROUTABLE_API)
        .env("ATO_GITHUB_API_BASE_URL", UNROUTABLE_API)
        .env("ATO_TELEMETRY", "0")
        .args(["revisions", &ipk, "--json"])
        .output()
        .expect("run ato revisions");
    assert!(
        output.status.success(),
        "ato revisions must resolve the installed ipk\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let revisions: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("revisions JSON array");
    let current = revisions
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("is_current").and_then(|v| v.as_bool()) == Some(true))
        })
        .expect("a current revision must be recorded for the installed app");
    assert_eq!(
        current.get("rev_id").and_then(|v| v.as_str()),
        Some(rev.as_str()),
        "the current revision must match the one recorded at install"
    );
}

// ── helpers ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct StoredCondition {
    kind: String,
    #[allow(dead_code)]
    condition_key: String,
    install_revision_id: Option<String>,
}

/// Read the launch-condition ledger for an ipk from the hermetic installed-state
/// DB via the public capsule-core API (not raw SQL).
fn read_launch_conditions(ato_home: &Path, ipk: &str) -> Vec<StoredCondition> {
    // The DB lives at $ATO_HOME/state/. Point capsule-core's home resolution at
    // the hermetic ATO_HOME for this read.
    // SAFETY: tests in this file run serially (`#[serial]`), so mutating the
    // process env here does not race another test.
    let previous = std::env::var("ATO_HOME").ok();
    unsafe {
        std::env::set_var("ATO_HOME", ato_home);
    }
    let db = capsule::installed_state::InstalledStateDb::open_default()
        .expect("open hermetic installed-state DB");
    let claims = db
        .list_launch_condition_claims(ipk)
        .expect("list launch condition claims");
    match previous {
        Some(value) => unsafe { std::env::set_var("ATO_HOME", value) },
        None => unsafe { std::env::remove_var("ATO_HOME") },
    }
    claims
        .into_iter()
        .map(|claim| StoredCondition {
            kind: claim.kind.as_str().to_string(),
            condition_key: claim.condition_key,
            install_revision_id: claim.install_revision_id,
        })
        .collect()
}

fn assert_ipk_shape(value: &str) {
    assert_eq!(value.len(), "ipk_".len() + 32, "unexpected IPK length");
    assert!(value.starts_with("ipk_"), "unexpected IPK prefix: {value}");
    assert!(
        value
            .trim_start_matches("ipk_")
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
        "IPK must be lowercase hex: {value}"
    );
}

fn parse_install_result_json(stdout: &str) -> Option<serde_json::Value> {
    for (index, _) in stdout.match_indices('{').rev() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&stdout[index..])
            && value.get("install_lifecycle").is_some()
        {
            return Some(value);
        }
    }
    None
}

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(name: &str) -> Self {
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos(),
            rand::random::<u64>()
        );
        let path = workspace_root().join(".tmp").join(name).join(unique);
        fs::create_dir_all(&path).expect("create scratch dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
