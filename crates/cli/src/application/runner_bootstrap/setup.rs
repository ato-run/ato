//! `ato runner setup [--fix]` — prepare this Ubuntu host as an all-in-one snapshot
//! builder + capsule runner.
//!
//! Host-mutating, so three hard rules:
//! 1. **Nothing runs without `--fix` + an explicit confirmation** (or `--yes` for
//!    provisioning scripts). Without `--fix` the derived plan is printed and nothing
//!    is touched.
//! 2. **Existing files are backed up, never clobbered**: the env file is only
//!    appended to (missing keys), and unit files are copied to `<name>.bak-<epoch>`
//!    before being rewritten.
//! 3. **Downloads are sha256-pinned** (Firecracker release tarball, guest kernel) —
//!    a mismatch aborts the step; an unverified VMM is never installed.
//!
//! Blocked items (BIOS virtualization, non-Ubuntu OS) are printed as exact manual
//! steps — `--fix` cannot and does not try to fix them.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{
    BUILDER_UNIT, Check, CheckStatus, DEFAULT_ARTIFACT_ROOT, ENV_FILE, FC_INSTALL_PATH,
    FC_TGZ_SHA256, FC_TGZ_URL, FC_VERSION, GUEST_KERNEL_INSTALL_PATH, GUEST_KERNEL_SHA256,
    GUEST_KERNEL_URL, RUNNER_UNIT, SYSTEMD_DIR, checks,
};

pub(crate) struct SetupOptions {
    pub fix: bool,
    pub yes: bool,
    pub artifact_root: Option<String>,
    pub api_url: Option<String>,
}

/// Reject an artifact-root that is not a plain absolute path. These values are
/// interpolated into root-run `sh -c` commands and written into a root-read env
/// file, so a value containing shell metacharacters, whitespace, or a newline
/// would be command / env-line injection under sudo. Restricting to an absolute
/// path over `[A-Za-z0-9_/.-]` makes injection structurally impossible.
pub(crate) fn validate_artifact_root(v: &str) -> Result<()> {
    if !v.starts_with('/') {
        bail!("--artifact-root must be an absolute path (got {v:?})");
    }
    if v.contains("..") {
        bail!("--artifact-root must not contain '..' (got {v:?})");
    }
    if !v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-')) {
        bail!("--artifact-root may only contain [A-Za-z0-9_/.-] (got {v:?})");
    }
    Ok(())
}

/// Reject an api-url that is not a clean absolute http(s) URL. It is written into
/// the root-read env file, so a newline/whitespace would inject an arbitrary env
/// line (e.g. a second key a root service would then honor).
pub(crate) fn validate_api_url(v: &str) -> Result<()> {
    if !(v.starts_with("http://") || v.starts_with("https://")) {
        bail!("--api-url must be an http(s) URL (got {v:?})");
    }
    if v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("--api-url must not contain whitespace or control characters");
    }
    Ok(())
}

/// One host mutation the plan would apply. `commands` are what actually runs —
/// shown verbatim in the plan so the operator confirms exactly what will execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FixAction {
    pub id: &'static str,
    pub title: String,
    pub commands: Vec<String>,
}

/// Derive the fix plan from failed checks. Pure — testable without a host.
pub(crate) fn derive_plan(checks: &[Check], opts: &SetupOptions) -> Vec<FixAction> {
    let failed =
        |id: &str| checks.iter().any(|c| c.id == id && c.status == CheckStatus::Missing);
    let artifact_root =
        opts.artifact_root.clone().unwrap_or_else(|| DEFAULT_ARTIFACT_ROOT.to_string());
    let mut plan = Vec::new();

    // apt-able prerequisites in ONE transaction (docker.io only when Docker itself
    // is missing — an unreachable daemon is fixed by the enable step instead).
    let mut pkgs: Vec<&str> = Vec::new();
    if failed("tool_git") {
        pkgs.push("git");
    }
    if failed("tool_curl") {
        pkgs.push("curl");
    }
    let docker_missing = checks
        .iter()
        .any(|c| c.id == "docker" && c.status == CheckStatus::Missing && c.detail.contains("not installed"));
    if docker_missing {
        pkgs.push("docker.io");
    }
    if !pkgs.is_empty() {
        plan.push(FixAction {
            id: "apt_install",
            title: format!("Install packages: {}", pkgs.join(", ")),
            commands: vec![
                "apt-get update -qq".to_string(),
                format!("DEBIAN_FRONTEND=noninteractive apt-get install -y -qq {}", pkgs.join(" ")),
            ],
        });
    }
    if failed("docker") {
        plan.push(FixAction {
            id: "docker_enable",
            title: "Enable + start the Docker daemon".to_string(),
            commands: vec!["systemctl enable --now docker".to_string()],
        });
    }
    if failed("kvm_device") {
        plan.push(FixAction {
            id: "kvm_module",
            title: "Load the KVM module (and persist it)".to_string(),
            commands: vec![
                "sh -c 'grep -q vmx /proc/cpuinfo && modprobe kvm_intel || modprobe kvm_amd'".to_string(),
                "sh -c 'grep -q vmx /proc/cpuinfo && echo kvm_intel || echo kvm_amd' > /etc/modules-load.d/ato-kvm.conf".to_string(),
            ],
        });
    }
    // Group grants for the invoking (sudo) user.
    let need_groups: Vec<&str> = [("kvm_group", "kvm"), ("docker_group", "docker")]
        .iter()
        .filter(|(id, _)| failed(id))
        .map(|(_, g)| *g)
        .collect();
    if !need_groups.is_empty() {
        plan.push(FixAction {
            id: "usermod_groups",
            title: format!("Add the operating user to group(s): {}", need_groups.join(", ")),
            commands: vec![format!(
                "usermod -aG {} \"${{SUDO_USER:-$USER}}\"",
                need_groups.join(",")
            )],
        });
    }
    // Firecracker is (re)installed when Missing OR when a non-pinned version is
    // present (Warn) — otherwise the doctor's "setup --fix installs the pinned
    // v1.16.0" hint on a version-mismatch would run and install nothing.
    let firecracker_needs_install = checks.iter().any(|c| {
        c.id == "firecracker" && matches!(c.status, CheckStatus::Missing | CheckStatus::Warn)
    });
    if firecracker_needs_install {
        // Download + extract in a fresh private (root-owned, 0700) tmp dir so a
        // local user cannot pre-plant /tmp/release-*/… and have `install` copy an
        // attacker file to a root path. The sha256 check gates the tarball; the
        // private dir gates the extracted layout.
        plan.push(FixAction {
            id: "firecracker_install",
            title: format!("Install Firecracker {FC_VERSION} (sha256-verified) to {FC_INSTALL_PATH}"),
            commands: vec![format!(
                "d=$(mktemp -d) && curl -fsSL -o \"$d/fc.tgz\" {FC_TGZ_URL} && \
                 echo '{FC_TGZ_SHA256}  '\"$d/fc.tgz\" | sha256sum -c - && \
                 tar -xzf \"$d/fc.tgz\" -C \"$d\" && \
                 install -m 0755 \"$d/release-{FC_VERSION}-x86_64/firecracker-{FC_VERSION}-x86_64\" {FC_INSTALL_PATH} && \
                 rm -rf \"$d\""
            )],
        });
    }
    if failed("guest_kernel") {
        // GUEST_KERNEL_INSTALL_PATH is a compile-time const (not user input); the
        // .tmp lives under its root-owned parent dir, and the final mv is atomic
        // only after the sha256 check passes.
        let parent = Path::new(GUEST_KERNEL_INSTALL_PATH).parent().unwrap().display();
        plan.push(FixAction {
            id: "kernel_install",
            title: format!("Install guest kernel vmlinux-5.10.223 (sha256-verified) to {GUEST_KERNEL_INSTALL_PATH}"),
            commands: vec![format!(
                "mkdir -p {parent} && curl -fsSL -o {GUEST_KERNEL_INSTALL_PATH}.tmp {GUEST_KERNEL_URL} && \
                 echo '{GUEST_KERNEL_SHA256}  {GUEST_KERNEL_INSTALL_PATH}.tmp' | sha256sum -c - && \
                 mv {GUEST_KERNEL_INSTALL_PATH}.tmp {GUEST_KERNEL_INSTALL_PATH}"
            )],
        });
    }
    if failed("tun_tap") {
        plan.push(FixAction {
            id: "tun_module",
            title: "Load the tun module (and persist it)".to_string(),
            commands: vec![
                "modprobe tun".to_string(),
                "sh -c 'echo tun > /etc/modules-load.d/ato-tun.conf'".to_string(),
            ],
        });
    }
    if failed("artifact_root") {
        plan.push(FixAction {
            id: "artifact_root",
            title: format!("Create artifact root {artifact_root} (owned by the operating user)"),
            commands: vec![
                format!("mkdir -p {artifact_root}"),
                format!("chown \"${{SUDO_USER:-$USER}}\" {artifact_root}"),
            ],
        });
    }
    // env file + units are written by us (not shell) so backup semantics are exact;
    // the plan records them as pseudo-commands for the confirmation display.
    if failed("env_file") {
        plan.push(FixAction {
            id: "env_file",
            title: format!("Write {ENV_FILE} (append missing keys only; existing lines untouched)"),
            commands: vec![format!("<write> {ENV_FILE}")],
        });
    }
    if failed("unit_builder") || failed("unit_runner") {
        plan.push(FixAction {
            id: "systemd_units",
            title: format!("Write {BUILDER_UNIT} + {RUNNER_UNIT} (existing files backed up), then daemon-reload"),
            commands: vec![
                format!("<write> {SYSTEMD_DIR}/{BUILDER_UNIT}"),
                format!("<write> {SYSTEMD_DIR}/{RUNNER_UNIT}"),
                "systemctl daemon-reload".to_string(),
            ],
        });
    }
    plan
}

/// The env-file lines the services need, minus keys `existing` already defines.
/// Pure — the append-only merge is the tested invariant (never rewrite a line the
/// operator set).
pub(crate) fn env_file_missing_lines(
    existing: &BTreeMap<String, String>,
    artifact_root: &str,
    api_url: Option<&str>,
) -> Vec<String> {
    let wanted: Vec<(&str, String)> = vec![
        ("ATO_SNAPSHOT_ARTIFACT_ROOT", artifact_root.to_string()),
        ("ATO_API_URL", api_url.unwrap_or("https://api.ato.run").to_string()),
        ("ATO_FC_BIN", FC_INSTALL_PATH.to_string()),
        ("ATO_FC_KERNEL", GUEST_KERNEL_INSTALL_PATH.to_string()),
        ("ATO_SNAPSHOT_BACKEND", "firecracker".to_string()),
    ];
    wanted
        .into_iter()
        .filter(|(k, _)| !existing.contains_key(*k))
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}

/// Render a systemd unit. Pure. Both services run as root in v0 (Firecracker tap
/// setup + /dev/kvm + Docker); they read all config from the env file, so tokens
/// never live in the unit itself.
pub(crate) fn render_unit(unit: &str) -> String {
    let (desc, exec) = if unit == BUILDER_UNIT {
        (
            "Ato snapshot builder (capsule -> sealed Ready-State artifact)",
            // SNAPSHOT_BUILDER_AGENT_TOKEN comes from the env file; %H = hostname.
            "/usr/local/bin/ato-snapshot-builder --agent-id %H --work ${ATO_SNAPSHOT_ARTIFACT_ROOT}",
        )
    } else {
        (
            "Ato connected runner agent (claims run leases, serves restored snapshots)",
            "/usr/local/bin/ato runner serve",
        )
    };
    format!(
        "# Written by `ato runner setup` — edit {ENV_FILE} for configuration.\n\
         [Unit]\n\
         Description={desc}\n\
         After=network-online.target docker.service\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         EnvironmentFile={ENV_FILE}\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Back up `path` to `<path>.bak-<epoch>` iff it exists, then write `content`.
fn backup_then_write(path: &Path, content: &str) -> Result<Option<std::path::PathBuf>> {
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
    Ok(backup)
}

fn run_shell(cmd: &str) -> Result<()> {
    let status = Command::new("sh").arg("-c").arg(cmd).status()?;
    if !status.success() {
        bail!("command failed ({}): {cmd}", status);
    }
    Ok(())
}

pub(crate) fn run(opts: SetupOptions) -> Result<()> {
    if let Some(url) = &opts.api_url {
        validate_api_url(url)?;
    }
    // Converge on the SAME artifact root the doctor probes (env → env-file →
    // default) when the operator did not override it, so `setup --fix` repairs the
    // directory doctor flagged rather than silently creating the default elsewhere.
    let mut opts = opts;
    if opts.artifact_root.is_none() {
        opts.artifact_root = Some(checks::resolve_artifact_root());
    }
    // Validate the EFFECTIVE root (flag OR env/env-file resolved) BEFORE it reaches
    // any plan command or the env file — even in dry-run, so a dangerous value is
    // rejected up front rather than displayed as a command to be "confirmed".
    if let Some(root) = &opts.artifact_root {
        validate_artifact_root(root)?;
    }
    let checks = checks::gather();
    let blocked: Vec<&Check> =
        checks.iter().filter(|c| c.status == CheckStatus::Blocked).collect();
    let plan = derive_plan(&checks, &opts);

    if !blocked.is_empty() {
        println!("Manual steps required first (setup cannot fix these):");
        for c in &blocked {
            println!("  ✗ {}: {}", c.label, c.detail);
        }
        println!();
    }
    if plan.is_empty() {
        println!("Nothing to fix — the host already satisfies every fixable check.");
        if blocked.is_empty() {
            println!("This machine is ready as a Capsule Runner.");
        }
        return Ok(());
    }

    println!("Setup plan ({} action(s)):", plan.len());
    for a in &plan {
        println!("  • {}", a.title);
        for c in &a.commands {
            println!("      $ {c}");
        }
    }

    if !opts.fix {
        println!();
        println!("Dry run — nothing was changed. Re-run with --fix to apply.");
        return Ok(());
    }

    // Host mutation from here: root + explicit confirmation required.
    if !is_root() {
        bail!("--fix mutates the host and must run as root: sudo ato runner setup --fix");
    }
    if !opts.yes {
        print!("\nApply these changes to this host? Type 'yes' to continue: ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != "yes" {
            bail!("aborted — no changes were made");
        }
    }

    let artifact_root =
        opts.artifact_root.clone().unwrap_or_else(|| DEFAULT_ARTIFACT_ROOT.to_string());
    for a in &plan {
        println!("→ {}", a.title);
        match a.id {
            "env_file" => {
                let existing_text = std::fs::read_to_string(ENV_FILE).unwrap_or_default();
                let existing = checks::env_file_values(&existing_text);
                let lines =
                    env_file_missing_lines(&existing, &artifact_root, opts.api_url.as_deref());
                let mut content = existing_text;
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
                }
                if content.is_empty() {
                    content.push_str("# Ato runner host environment (written by `ato runner setup`).\n# Required before starting the builder service:\n#   SNAPSHOT_BUILDER_AGENT_TOKEN=...\n# Optional public URL advertised by the runner agent:\n#   ATO_RUNNER_PUBLIC_BASE_URL=...\n");
                }
                for l in &lines {
                    content.push_str(l);
                    content.push('\n');
                }
                let bak = backup_then_write(Path::new(ENV_FILE), &content)?;
                if let Some(b) = bak {
                    println!("   (previous file backed up to {})", b.display());
                }
            }
            "systemd_units" => {
                for unit in [BUILDER_UNIT, RUNNER_UNIT] {
                    let path = Path::new(SYSTEMD_DIR).join(unit);
                    let bak = backup_then_write(&path, &render_unit(unit))?;
                    if let Some(b) = bak {
                        println!("   (previous {unit} backed up to {})", b.display());
                    }
                }
                run_shell("systemctl daemon-reload")?;
            }
            _ => {
                for c in &a.commands {
                    run_shell(c)?;
                }
            }
        }
    }

    println!();
    println!("Setup complete. Next steps:");
    println!("  1. re-login (group membership takes effect on a new session)");
    println!("  2. ato runner login                      # enroll this host");
    println!("  3. edit {ENV_FILE}: set SNAPSHOT_BUILDER_AGENT_TOKEN (builder) and ATO_RUNNER_PUBLIC_BASE_URL (tunnel)");
    println!("  4. systemctl enable --now {BUILDER_UNIT} {RUNNER_UNIT}");
    println!("  5. ato runner smoke                      # verify the full local path");
    println!("  6. ato doctor runner                     # confirm everything is green");
    Ok(())
}

fn is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &'static str, status: CheckStatus) -> Check {
        Check { id, label: "x", status, detail: "not installed".into(), fix: None }
    }
    fn opts() -> SetupOptions {
        SetupOptions { fix: false, yes: false, artifact_root: None, api_url: None }
    }

    #[test]
    fn plan_is_empty_when_everything_passes() {
        let all_ok: Vec<Check> = ["docker", "firecracker", "artifact_root", "env_file"]
            .iter()
            .map(|id| check(id, CheckStatus::Ok))
            .collect();
        assert!(derive_plan(&all_ok, &opts()).is_empty());
    }

    #[test]
    fn plan_derives_the_expected_actions_and_never_fixes_blocked() {
        let checks = vec![
            check("cpu_virt", CheckStatus::Blocked), // BIOS — must NOT appear in the plan
            check("docker", CheckStatus::Missing),
            check("firecracker", CheckStatus::Missing),
            check("guest_kernel", CheckStatus::Missing),
            check("artifact_root", CheckStatus::Missing),
            check("kvm_group", CheckStatus::Missing),
            check("env_file", CheckStatus::Missing),
            check("unit_builder", CheckStatus::Missing),
            check("tool_pnpm", CheckStatus::Warn), // optional — never auto-installed
        ];
        let plan = derive_plan(&checks, &opts());
        let ids: Vec<&str> = plan.iter().map(|a| a.id).collect();
        assert_eq!(
            ids,
            vec![
                "apt_install",
                "docker_enable",
                "usermod_groups",
                "firecracker_install",
                "kernel_install",
                "artifact_root",
                "env_file",
                "systemd_units"
            ]
        );
        // Downloads are pinned: the verify command carries the exact sha256.
        let fc = plan.iter().find(|a| a.id == "firecracker_install").unwrap();
        assert!(fc.commands.iter().any(|c| c.contains(FC_TGZ_SHA256) && c.contains("sha256sum -c")));
        let k = plan.iter().find(|a| a.id == "kernel_install").unwrap();
        assert!(k.commands.iter().any(|c| c.contains(GUEST_KERNEL_SHA256) && c.contains("sha256sum -c")));
        // Nothing in the plan touches the Blocked check.
        assert!(plan.iter().all(|a| a.id != "cpu_virt"));
    }

    #[test]
    fn custom_artifact_root_flows_into_the_plan() {
        let checks = vec![check("artifact_root", CheckStatus::Missing)];
        let o = SetupOptions { artifact_root: Some("/srv/snapshots".into()), ..opts() };
        let plan = derive_plan(&checks, &o);
        assert!(plan[0].commands.iter().any(|c| c.contains("/srv/snapshots")));
    }

    #[test]
    fn env_file_merge_is_append_only() {
        let mut existing = BTreeMap::new();
        existing.insert("ATO_API_URL".to_string(), "https://staging-api.ato.run".to_string());
        existing.insert("ATO_FC_BIN".to_string(), "/opt/fc/firecracker".to_string());
        let lines = env_file_missing_lines(&existing, "/var/lib/ato/snapshots", Some("https://api.ato.run"));
        // Operator-set keys are NOT re-emitted (their values stay untouched)…
        assert!(lines.iter().all(|l| !l.starts_with("ATO_API_URL=") && !l.starts_with("ATO_FC_BIN=")));
        // …and only the genuinely missing keys are appended.
        assert_eq!(
            lines,
            vec![
                "ATO_SNAPSHOT_ARTIFACT_ROOT=/var/lib/ato/snapshots",
                "ATO_FC_KERNEL=/var/lib/ato/kernel/vmlinux-5.10.223",
                "ATO_SNAPSHOT_BACKEND=firecracker",
            ]
        );
    }

    #[test]
    fn units_read_config_from_the_env_file_and_never_embed_tokens() {
        for unit in [BUILDER_UNIT, RUNNER_UNIT] {
            let text = render_unit(unit);
            assert!(text.contains(&format!("EnvironmentFile={ENV_FILE}")));
            assert!(text.contains("Restart=on-failure"));
            assert!(!text.to_lowercase().contains("token="), "unit must not embed a token: {text}");
        }
        assert!(render_unit(BUILDER_UNIT).contains("ato-snapshot-builder"));
        assert!(render_unit(RUNNER_UNIT).contains("runner serve"));
    }

    #[test]
    fn artifact_root_validation_blocks_injection() {
        // Good.
        assert!(validate_artifact_root("/var/lib/ato/snapshots").is_ok());
        assert!(validate_artifact_root("/srv/ato_snap-1").is_ok());
        // Injection / traversal / relative / metacharacters ⇒ rejected.
        for bad in [
            "/var/lib; rm -rf /",
            "/var/$(reboot)",
            "/var/`id`",
            "relative/path",
            "/var/../etc",
            "/var/lib snapshots",
            "/var/lib\nATO_X=1",
            "/a|b",
        ] {
            assert!(validate_artifact_root(bad).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn api_url_validation_blocks_env_line_injection() {
        assert!(validate_api_url("https://api.ato.run").is_ok());
        assert!(validate_api_url("http://127.0.0.1:8787").is_ok());
        for bad in [
            "ftp://x",
            "api.ato.run",
            "https://api.ato.run\nSNAPSHOT_BUILDER_AGENT_TOKEN=evil",
            "https://api.ato.run token",
            "https://api\t.run",
        ] {
            assert!(validate_api_url(bad).is_err(), "must reject {bad:?}");
        }
    }

    #[test]
    fn firecracker_install_uses_private_tmp_and_pins_sha() {
        let checks = vec![check("firecracker", CheckStatus::Missing)];
        let plan = derive_plan(&checks, &opts());
        let fc = &plan[0].commands[0];
        assert!(fc.contains("mktemp -d"), "must download into a private tmp dir: {fc}");
        assert!(fc.contains(FC_TGZ_SHA256) && fc.contains("sha256sum -c"), "must pin sha256");
        assert!(!fc.contains("/tmp/ato-fc.tgz"), "must not use a predictable /tmp path");
    }

    #[test]
    fn backup_then_write_backs_up_existing_and_creates_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.env");
        // Fresh file: no backup.
        let bak = backup_then_write(&path, "a=1\n").unwrap();
        assert!(bak.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=1\n");
        // Existing file: backed up with the ORIGINAL content preserved.
        let bak = backup_then_write(&path, "a=2\n").unwrap().expect("backup expected");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "a=1\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
    }
}
