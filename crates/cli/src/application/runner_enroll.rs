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
    // Whether restore_snapshot will be advertised is a LOCAL host-capability question
    // (KVM + Firecracker), decided by the serve loop's first heartbeat — not by /self,
    // which is read here BEFORE that heartbeat and so still shows the registration
    // default. Check the local advertisement instead of a stale control-plane view.
    if advertised_lease_kinds().iter().any(|k| k == "restore_snapshot") {
        println!("• this host advertises restore_snapshot (snapshot runs can dispatch here once serving)");
    } else {
        eprintln!(
            "⚠️  this host will NOT advertise restore_snapshot (KVM/Firecracker not ready). Run `ato doctor runner`; snapshot runs will not dispatch here until it does."
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
/// Produce the new env-file text: **UPSERT** the runner-identity keys enroll owns
/// (replace in place if present, else append) and **append-only** the tuning keys
/// (never clobber an operator/bootstrap value). Pure — comments, ordering, and unrelated
/// keys are preserved.
///
/// The identity keys (`ATO_API_URL`/`ATO_RUNNER_*`) MUST be upserted: `ato runner setup`
/// seeds `ATO_API_URL` with the production default, and enroll's `--api-url` is the
/// authoritative endpoint for THIS enrollment — an append-only merge would leave the
/// service pointed at the wrong control plane with a token it will reject.
pub(crate) fn upsert_env_text(
    existing: &str,
    set: &[(&str, String)],
    append_if_missing: &[(&str, String)],
) -> String {
    let mut out = String::new();
    let mut replaced = vec![false; set.len()];
    for line in existing.lines() {
        let trimmed = line.trim_start();
        let key = (!trimmed.starts_with('#'))
            .then(|| trimmed.split_once('='))
            .flatten()
            .map(|(k, _)| k.trim());
        if let Some(key) = key
            && let Some(i) = set.iter().position(|(k, _)| *k == key)
        {
            out.push_str(&format!("{}={}\n", set[i].0, set[i].1));
            replaced[i] = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    for (i, (k, v)) in set.iter().enumerate() {
        if !replaced[i] {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    let existing_keys = checks::env_file_values(existing);
    for (k, v) in append_if_missing {
        if !existing_keys.contains_key(*k) {
            out.push_str(&format!("{k}={v}\n"));
        }
    }
    out
}

fn write_runner_env(creds: &RunnerCredentials, public_base_url: Option<&str>) {
    let path = Path::new(ENV_FILE);
    let existing_text = std::fs::read_to_string(path).unwrap_or_default();

    // Identity/endpoint keys enroll owns — upserted (its --api-url wins over setup's default).
    let mut set: Vec<(&str, String)> = vec![
        (runner_agent::ENV_RUNNER_API_URL, creds.api_base.clone()),
        (runner_agent::ENV_RUNNER_ID, creds.runner_id.clone()),
        (runner_agent::ENV_RUNNER_TOKEN, creds.runner_token.clone()),
        (runner_agent::ENV_RUNNER_DISPLAY_NAME, creds.display_name.clone()),
    ];
    if let Some(url) = public_base_url {
        set.push(("ATO_RUNNER_PUBLIC_BASE_URL", url.to_string()));
    }
    // Tuning keys — never clobber an operator/bootstrap value.
    let append: Vec<(&str, String)> =
        vec![("ATO_SNAPSHOT_ARTIFACT_ROOT", checks::resolve_artifact_root())];

    let new_text = upsert_env_text(&existing_text, &set, &append);
    if new_text == existing_text {
        println!("• {ENV_FILE}: already up to date");
        return;
    }
    match write_env_secure(path, &new_text) {
        Ok(backup) => {
            let note = backup
                .map(|b| format!(" (previous backed up to {})", b.display()))
                .unwrap_or_default();
            println!("• updated {ENV_FILE} (0600){note}");
        }
        Err(e) => {
            eprintln!(
                "⚠️  could not write {ENV_FILE} ({e:#}). Re-run with `sudo` to configure the systemd service. credentials.json is saved, so `ato runner serve` works interactively."
            );
        }
    }
}

#[cfg(unix)]
fn chmod_0600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {} failed", path.display()))
}
#[cfg(not(unix))]
fn chmod_0600(_path: &Path) -> Result<()> {
    Ok(())
}

/// Create `path` for writing at mode 0600 FROM CREATION (no umask window). On non-unix
/// falls back to a plain create.
fn create_0600(path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("create {} failed", path.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::File::create(path).with_context(|| format!("create {} failed", path.display()))
    }
}

/// Back up an existing file (also holding a token), then atomically replace it with
/// `content` at mode 0600. The file carries the runner token, so it must NEVER exist
/// world/group-readable even momentarily: the new content is written to a 0600 temp file
/// in the SAME directory (created 0600 — no umask window), fsync'd, then `rename`d over
/// the target (atomic on one filesystem — a crash leaves either the old file or the new,
/// never a partial or wrong-mode one). The backup copy is chmod'd 0600 too.
fn write_env_secure(path: &Path, content: &str) -> Result<Option<std::path::PathBuf>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
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
        // std::fs::copy propagates the SOURCE mode (which may be 0644 if setup wrote it
        // without a chmod) — force the backup to 0600 since it may contain a token.
        chmod_0600(&bak)?;
        backup = Some(bak);
    }
    // Write to a 0600 temp beside the target, then atomically rename into place.
    let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut f = create_0600(&tmp)?;
        f.write_all(content.as_bytes())
            .and_then(|()| f.sync_all())
            .with_context(|| format!("write {} failed", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp); // don't leak the temp (it holds the token)
        format!("atomically replace {} failed", path.display())
    })?;
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

    /// Mirror write_runner_env's key split for the tests.
    fn upsert(existing: &str, api: &str, id: &str, token: &str, name: &str, pub_url: Option<&str>, root: &str) -> String {
        let mut set: Vec<(&str, String)> = vec![
            (runner_agent::ENV_RUNNER_API_URL, api.into()),
            (runner_agent::ENV_RUNNER_ID, id.into()),
            (runner_agent::ENV_RUNNER_TOKEN, token.into()),
            (runner_agent::ENV_RUNNER_DISPLAY_NAME, name.into()),
        ];
        if let Some(u) = pub_url { set.push(("ATO_RUNNER_PUBLIC_BASE_URL", u.into())); }
        upsert_env_text(existing, &set, &[("ATO_SNAPSHOT_ARTIFACT_ROOT", root.into())])
    }

    #[test]
    fn env_upsert_writes_identity_keys_on_a_fresh_file() {
        let out = upsert("", "https://api.ato.run", "runner_1", "ato_rnr_secret", "my-host", Some("https://runner.example.com"), "/var/lib/ato/snapshots");
        for want in [
            "ATO_API_URL=https://api.ato.run",
            "ATO_RUNNER_ID=runner_1",
            "ATO_RUNNER_TOKEN=ato_rnr_secret",
            "ATO_RUNNER_DISPLAY_NAME=my-host",
            "ATO_RUNNER_PUBLIC_BASE_URL=https://runner.example.com",
            "ATO_SNAPSHOT_ARTIFACT_ROOT=/var/lib/ato/snapshots",
        ] {
            assert!(out.lines().any(|l| l == want), "missing {want} in:\n{out}");
        }
    }

    #[test]
    fn env_upsert_overrides_the_endpoint_but_not_operator_tuning_keys() {
        // setup seeded ATO_API_URL with the prod default + an operator artifact root +
        // a builder token; enroll must OVERRIDE the endpoint it owns but leave the rest.
        let existing = "# header\nATO_API_URL=https://api.ato.run\nSNAPSHOT_BUILDER_AGENT_TOKEN=builder-secret\nATO_SNAPSHOT_ARTIFACT_ROOT=/srv/snap\n";
        let out = upsert(existing, "http://127.0.0.1:8787", "runner_1", "ato_rnr_secret", "my-host", None, "/var/lib/ato/snapshots");
        // Endpoint replaced IN PLACE (exactly once) with enroll's --api-url.
        assert_eq!(out.lines().filter(|l| l.starts_with("ATO_API_URL=")).count(), 1);
        assert!(out.lines().any(|l| l == "ATO_API_URL=http://127.0.0.1:8787"));
        assert!(!out.contains("ATO_API_URL=https://api.ato.run"));
        // Operator tuning keys untouched.
        assert!(out.lines().any(|l| l == "SNAPSHOT_BUILDER_AGENT_TOKEN=builder-secret"));
        assert!(out.lines().any(|l| l == "ATO_SNAPSHOT_ARTIFACT_ROOT=/srv/snap"));
        assert!(!out.contains("/var/lib/ato/snapshots")); // append-only key not re-added
        // Comment preserved; token appended.
        assert!(out.starts_with("# header\n"));
        assert!(out.lines().any(|l| l == "ATO_RUNNER_TOKEN=ato_rnr_secret"));
        // A re-run with the same values is idempotent (no further change).
        assert_eq!(upsert(&out, "http://127.0.0.1:8787", "runner_1", "ato_rnr_secret", "my-host", None, "/var/lib/ato/snapshots"), out);
    }

    #[test]
    fn write_env_secure_is_atomic_0600_for_file_and_backup() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.env");

        // Fresh write: no backup, content present, and 0600 FROM CREATION.
        assert!(write_env_secure(&path, "A=1\n").unwrap().is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\n");
        #[cfg(unix)]
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);

        // Simulate a pre-existing file left WORLD-READABLE by setup (0644) holding a
        // token: enroll must back it up AND leave the backup 0600 (no token leak), and
        // the replaced file is 0600.
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let bak = write_env_secure(&path, "A=1\nB=2\n").unwrap().expect("backup expected");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "A=1\n"); // original preserved
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "A=1\nB=2\n");
        #[cfg(unix)]
        {
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(
                std::fs::metadata(&bak).unwrap().permissions().mode() & 0o077,
                0,
                "backup must not be group/world readable — it may hold a token"
            );
        }
        // No leftover temp file beside the target.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "atomic write must leave no temp file");
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
