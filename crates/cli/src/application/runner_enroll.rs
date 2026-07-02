//! Runner Enrollment v0: `ato runner enroll` + `ato runner status`.
//!
//! After `ato runner setup --fix` (host prepared) and `ato runner smoke` (host can build
//! + serve), `enroll` registers this machine as a Connected Capsule Runner against the
//! control plane and wires up the systemd env file; `status` reports the local + control-
//! plane view. Both reuse the existing `ato runner login` registration and the
//! `credentials.json` the runner agent already reads.
//!
//! Boundaries: `enroll` reuses `run_login` (device-flow OR enrollment token) so there is
//! ONE registration path; it additionally writes `/etc/ato/runner.env` (append-only,
//! never overwriting operator keys) and verifies reachability via the read-only
//! `GET /v1/runners/:id/self` (ato-api). `credentials.json` stays authoritative for
//! `ato runner serve`; the env file is the systemd EnvironmentFile fallback.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::runner_agent::{
    self, RunnerCredentials, advertised_lease_kinds, credentials_path, load_credentials, run_login,
};
use super::runner_bootstrap::{ENV_FILE, RUNNER_UNIT, checks};

pub(crate) struct EnrollOptions {
    pub api_url: Option<String>,
    pub site_base: Option<String>,
    pub display_name: Option<String>,
    pub public_base_url: Option<String>,
    pub headless: bool,
    pub enrollment_token: Option<String>,
    pub start: bool,
}

pub(crate) async fn run_enroll(opts: EnrollOptions) -> Result<()> {
    // 1. Register — device-flow OR enrollment token, via the SAME path as
    //    `ato runner login` (which persists credentials.json, authoritative for serve).
    run_login(
        opts.api_url.clone(),
        opts.site_base.clone(),
        opts.display_name.clone(),
        opts.public_base_url.clone(),
        opts.headless,
        opts.enrollment_token.clone(),
    )
    .await
    .context("runner registration failed")?;

    // 2. Re-load what login persisted (token / id / api_base) for the env file + verify.
    let creds = load_credentials(&credentials_path())
        .context("registration reported success but no credentials were saved")?;

    // 3. Append the systemd env file (never overwriting operator keys).
    write_runner_env(&creds, opts.public_base_url.as_deref());

    // 4. Verify the runner can reach the control plane AND is active.
    let view = fetch_self(&creds)
        .await
        .context("control-plane reachability check failed (is GET /v1/runners/:id/self deployed?)")?;
    println!(
        "✅ Control plane reachable — runner {} status={} online={}",
        view.id, view.status, view.online
    );
    if view.status != "active" {
        bail!("runner registered but status is {:?} (expected active)", view.status);
    }
    if !view.supported_lease_kinds.iter().any(|k| k == "restore_snapshot") {
        eprintln!(
            "⚠️  this runner does not yet advertise restore_snapshot (KVM not ready?). Run `ato doctor runner`; snapshot runs will not dispatch here until it does."
        );
    }

    // 5. Optionally enable + start the runner service.
    if opts.start {
        start_runner_service()?;
    } else {
        println!("Next: sudo systemctl enable --now {RUNNER_UNIT}   (or re-run enroll with --start)");
        println!("Then: ato runner status");
    }
    Ok(())
}

/// Append the runner env keys to `/etc/ato/runner.env`, backing up an existing file and
/// never overwriting an operator-set key. Non-fatal on permission failure (credentials
/// .json already makes an interactive `ato runner serve` work; the env file only matters
/// for the systemd service, which needs root anyway).
/// The runner env lines to APPEND — only keys the operator has NOT already set. Pure,
/// so the append-only invariant (never rewrite an operator value, always include the
/// runner token/id) is tested without a filesystem.
pub(crate) fn runner_env_missing_lines(
    existing: &std::collections::BTreeMap<String, String>,
    api_base: &str,
    runner_id: &str,
    runner_token: &str,
    display_name: &str,
    public_base_url: Option<&str>,
    artifact_root: &str,
) -> Vec<String> {
    let mut wanted: Vec<(&str, String)> = vec![
        (runner_agent::ENV_RUNNER_API_URL, api_base.to_string()),
        (runner_agent::ENV_RUNNER_ID, runner_id.to_string()),
        (runner_agent::ENV_RUNNER_TOKEN, runner_token.to_string()),
        (runner_agent::ENV_RUNNER_DISPLAY_NAME, display_name.to_string()),
    ];
    if let Some(url) = public_base_url {
        wanted.push(("ATO_RUNNER_PUBLIC_BASE_URL", url.to_string()));
    }
    // Only set the artifact root if the operator/bootstrap hasn't.
    if !existing.contains_key("ATO_SNAPSHOT_ARTIFACT_ROOT") {
        wanted.push(("ATO_SNAPSHOT_ARTIFACT_ROOT", artifact_root.to_string()));
    }
    wanted
        .into_iter()
        .filter(|(k, _)| !existing.contains_key(*k))
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}

fn write_runner_env(creds: &RunnerCredentials, public_base_url: Option<&str>) {
    let path = Path::new(ENV_FILE);
    let existing_text = std::fs::read_to_string(path).unwrap_or_default();
    let existing = checks::env_file_values(&existing_text);
    let missing = runner_env_missing_lines(
        &existing,
        &creds.api_base,
        &creds.runner_id,
        &creds.runner_token,
        &creds.display_name,
        public_base_url,
        &checks::resolve_artifact_root(),
    );
    if missing.is_empty() {
        println!("• {ENV_FILE}: all runner keys already present (operator values untouched)");
        return;
    }

    let mut content = existing_text;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if content.is_empty() {
        content.push_str("# Ato runner environment (written by `ato runner enroll`).\n");
    }
    for line in &missing {
        content.push_str(line);
        content.push('\n');
    }
    match write_env_secure(path, &content) {
        Ok(backup) => {
            let note = backup
                .map(|b| format!(" (previous backed up to {})", b.display()))
                .unwrap_or_default();
            println!("• wrote {} key(s) to {ENV_FILE} (0600){note}", missing.len());
        }
        Err(e) => {
            eprintln!(
                "⚠️  could not write {ENV_FILE} ({e:#}). Re-run with `sudo` to configure the systemd service. credentials.json is saved, so `ato runner serve` works interactively."
            );
        }
    }
}

/// Back up an existing file, write `content`, and restrict to 0600 (the file holds the
/// runner token). Returns the backup path if one was made.
fn write_env_secure(path: &Path, content: &str) -> Result<Option<std::path::PathBuf>> {
    let mut backup = None;
    if path.exists() {
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bak = path.with_file_name(format!(
            "{}.bak-{epoch}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::copy(path, &bak).with_context(|| format!("backup {} failed", path.display()))?;
        backup = Some(bak);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content).with_context(|| format!("write {} failed", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(backup)
}

fn start_runner_service() -> Result<()> {
    let status = Command::new("systemctl")
        .args(["enable", "--now", RUNNER_UNIT])
        .status()
        .context("failed to run systemctl")?;
    if !status.success() {
        bail!("`systemctl enable --now {RUNNER_UNIT}` failed (run enroll with sudo)");
    }
    println!("✅ enabled + started {RUNNER_UNIT}");
    Ok(())
}

// ── status ──

/// The device view returned by `GET /v1/runners/:id/self`.
#[derive(Debug, Deserialize)]
pub(crate) struct SelfRunnerView {
    pub id: String,
    pub status: String,
    pub online: bool,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub public_base_url: Option<String>,
    #[serde(default)]
    pub supported_lease_kinds: Vec<String>,
    #[serde(default)]
    pub max_slots: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SelfResponse {
    runner: SelfRunnerView,
}

async fn fetch_self(creds: &RunnerCredentials) -> Result<SelfRunnerView> {
    let url = format!(
        "{}/v1/runners/{}/self",
        creds.api_base.trim_end_matches('/'),
        creds.runner_id
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&creds.runner_token)
        .send()
        .await
        .context("request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("control plane returned HTTP {status} for {url}: {body}");
    }
    let parsed: SelfResponse = resp.json().await.context("invalid /self response")?;
    Ok(parsed.runner)
}

fn unit_active(unit: &str) -> String {
    Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

pub(crate) async fn run_status(json: bool) -> Result<()> {
    let creds = runner_agent::load_runner_credentials().ok();
    let advertised = advertised_lease_kinds();
    let builder_unit = super::runner_bootstrap::BUILDER_UNIT;
    let runner_unit_state = unit_active(RUNNER_UNIT);
    let builder_unit_state = unit_active(builder_unit);

    // Control-plane view (only if enrolled + reachable).
    let view = match &creds {
        Some(c) => fetch_self(c).await.ok(),
        None => None,
    };

    if json {
        let out = serde_json::json!({
            "enrolled": creds.is_some(),
            "runner_id": creds.as_ref().map(|c| c.runner_id.clone()),
            "api_base": creds.as_ref().map(|c| c.api_base.clone()),
            "advertised_lease_kinds_local": advertised,
            "units": { RUNNER_UNIT: runner_unit_state, builder_unit: builder_unit_state },
            "control_plane": view.as_ref().map(|v| serde_json::json!({
                "id": v.id,
                "status": v.status,
                "online": v.online,
                "last_seen_at": v.last_seen_at,
                "public_base_url": v.public_base_url,
                "supported_lease_kinds": v.supported_lease_kinds,
                "max_slots": v.max_slots,
                "advertises_restore_snapshot": v.supported_lease_kinds.iter().any(|k| k == "restore_snapshot"),
            })),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("Ato Runner Status");
    println!();
    match &creds {
        Some(c) => {
            println!("Enrollment:");
            println!("  runner id: {}", c.runner_id);
            println!("  api base:  {}", c.api_base);
        }
        None => {
            println!("Enrollment: NOT ENROLLED (run `ato runner enroll`)");
        }
    }
    println!();
    println!("Services:");
    println!("  {RUNNER_UNIT}: {runner_unit_state}");
    println!("  {builder_unit}: {builder_unit_state}");
    println!();
    println!("Advertised locally: {}", advertised.join(", "));
    println!();
    match view {
        Some(v) => {
            println!("Control plane:");
            println!("  status:            {}", v.status);
            println!("  online:            {}", v.online);
            println!("  last seen:         {}", v.last_seen_at.as_deref().unwrap_or("never"));
            println!("  public base url:   {}", v.public_base_url.as_deref().unwrap_or("(none — apps reachable on this host only)"));
            println!("  supported kinds:   {}", v.supported_lease_kinds.join(", "));
            println!("  max slots:         {}", v.max_slots.map(|n| n.to_string()).unwrap_or_else(|| "1".into()));
            let restore = v.supported_lease_kinds.iter().any(|k| k == "restore_snapshot");
            println!();
            println!(
                "  {} snapshot runs {} dispatch here",
                if restore { "✓" } else { "✗" },
                if restore { "will" } else { "will NOT" }
            );
        }
        None if creds.is_some() => {
            println!("Control plane: UNREACHABLE (check ATO_API_URL / network / that GET /v1/runners/:id/self is deployed)");
        }
        None => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn env_lines_include_token_and_id_on_a_fresh_file() {
        let lines = runner_env_missing_lines(
            &BTreeMap::new(),
            "https://api.ato.run",
            "runner_1",
            "ato_rnr_secret",
            "my-host",
            Some("https://runner.example.com"),
            "/var/lib/ato/snapshots",
        );
        assert!(lines.iter().any(|l| l == "ATO_API_URL=https://api.ato.run"));
        assert!(lines.iter().any(|l| l == "ATO_RUNNER_ID=runner_1"));
        assert!(lines.iter().any(|l| l == "ATO_RUNNER_TOKEN=ato_rnr_secret"));
        assert!(lines.iter().any(|l| l == "ATO_RUNNER_DISPLAY_NAME=my-host"));
        assert!(lines.iter().any(|l| l == "ATO_RUNNER_PUBLIC_BASE_URL=https://runner.example.com"));
        assert!(lines.iter().any(|l| l == "ATO_SNAPSHOT_ARTIFACT_ROOT=/var/lib/ato/snapshots"));
    }

    #[test]
    fn env_merge_is_append_only_and_never_rewrites_operator_keys() {
        let mut existing = BTreeMap::new();
        existing.insert("ATO_API_URL".into(), "https://staging-api.ato.run".into());
        existing.insert("ATO_SNAPSHOT_ARTIFACT_ROOT".into(), "/srv/snap".into());
        let lines = runner_env_missing_lines(
            &existing,
            "https://api.ato.run", // different — must NOT override the operator's
            "runner_1",
            "ato_rnr_secret",
            "my-host",
            None, // no public base url ⇒ not emitted
            "/var/lib/ato/snapshots",
        );
        // Operator-set keys untouched.
        assert!(lines.iter().all(|l| !l.starts_with("ATO_API_URL=")));
        assert!(lines.iter().all(|l| !l.starts_with("ATO_SNAPSHOT_ARTIFACT_ROOT=")));
        // Genuinely missing keys still appended (incl. the token).
        assert!(lines.iter().any(|l| l == "ATO_RUNNER_ID=runner_1"));
        assert!(lines.iter().any(|l| l == "ATO_RUNNER_TOKEN=ato_rnr_secret"));
        // No public URL passed ⇒ that key is absent.
        assert!(lines.iter().all(|l| !l.starts_with("ATO_RUNNER_PUBLIC_BASE_URL=")));
    }

    #[test]
    fn write_env_secure_backs_up_and_sets_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.env");
        // Fresh: no backup.
        assert!(write_env_secure(&path, "A=1\n").unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Existing: original content is backed up before the new write.
        let bak = write_env_secure(&path, "A=1\nB=2\n").unwrap().expect("backup expected");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "A=1\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\nB=2\n");
    }

    #[test]
    fn self_response_deserializes_the_device_view() {
        let body = r#"{"runner":{"id":"r1","status":"active","online":true,"last_seen_at":"2026-07-02T00:00:00.000Z","public_base_url":"https://x","supported_lease_kinds":["run_source_sandbox","restore_snapshot"],"max_slots":3},"heartbeat":{"interval_seconds":30,"online_window_seconds":90}}"#;
        let parsed: SelfResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.runner.id, "r1");
        assert!(parsed.runner.online);
        assert!(parsed.runner.supported_lease_kinds.iter().any(|k| k == "restore_snapshot"));
        assert_eq!(parsed.runner.max_slots, Some(3));
    }
}
