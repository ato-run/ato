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
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::official_preview::OfficialPreviewConfig;
use super::{
    ATO_CLI_INSTALL_PATH, BUILDER_INSTALL_PATH, BUILDER_UNIT, Check, CheckStatus,
    DEFAULT_ARTIFACT_ROOT, ENV_FILE, FC_INSTALL_PATH, FC_TGZ_SHA256, FC_TGZ_URL, FC_VERSION,
    GUEST_KERNEL_INSTALL_PATH, GUEST_KERNEL_SHA256, GUEST_KERNEL_URL, RUNNER_UNIT, SYSTEMD_DIR,
    checks, official_preview,
};

pub(crate) struct SetupOptions {
    pub fix: bool,
    pub yes: bool,
    pub artifact_root: Option<String>,
    pub api_url: Option<String>,
    /// `--official-preview`: additionally prepare this host as an OFFICIAL
    /// preview runner behind Caddy (ato-managed slot hostnames, loopback-only
    /// slot ports, ATO_RUNNER_PREVIEW=1). None = the classic builder+runner
    /// setup, byte-identical to before.
    pub official: Option<OfficialPreviewConfig>,
}

/// The on-disk sources setup would copy the Ato binaries FROM (None when none is
/// available). Separated from [`derive_plan`] so the plan stays a pure function of
/// (checks, opts, sources) and the tests are deterministic.
pub(crate) struct BinaryPlan {
    /// Source for `/usr/local/bin/ato` — the running executable (this IS ato).
    pub ato_source: Option<PathBuf>,
    /// Source for `/usr/local/bin/ato-snapshot-builder` — a colocated sibling, if any.
    pub builder_source: Option<PathBuf>,
}

impl BinaryPlan {
    /// Resolve from the running process (reads `current_exe` + the filesystem).
    pub(crate) fn resolve() -> Self {
        Self {
            ato_source: checks::ato_binary_source(),
            builder_source: checks::snapshot_builder_source(),
        }
    }
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
    if !v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
    {
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
pub(crate) fn derive_plan(
    checks: &[Check],
    opts: &SetupOptions,
    bins: &BinaryPlan,
) -> Vec<FixAction> {
    let failed = |id: &str| {
        checks
            .iter()
            .any(|c| c.id == id && c.status == CheckStatus::Missing)
    };
    let is_ok = |id: &str| {
        checks
            .iter()
            .any(|c| c.id == id && c.status == CheckStatus::Ok)
    };
    // The x86_64 stack (Firecracker + guest kernel) must NOT be planned on a
    // non-x86_64 host — the arch check is Blocked there, and Blocked is never fixed.
    let arch_ok = !checks
        .iter()
        .any(|c| c.id == "arch" && c.status == CheckStatus::Blocked);
    let artifact_root = opts
        .artifact_root
        .clone()
        .unwrap_or_else(|| DEFAULT_ARTIFACT_ROOT.to_string());
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
    let docker_missing = checks.iter().any(|c| {
        c.id == "docker" && c.status == CheckStatus::Missing && c.detail.contains("not installed")
    });
    if docker_missing {
        pkgs.push("docker.io");
    }
    if !pkgs.is_empty() {
        plan.push(FixAction {
            id: "apt_install",
            title: format!("Install packages: {}", pkgs.join(", ")),
            commands: vec![
                "apt-get update -qq".to_string(),
                format!(
                    "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq {}",
                    pkgs.join(" ")
                ),
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
            title: format!(
                "Add the operating user to group(s): {}",
                need_groups.join(", ")
            ),
            commands: vec![format!(
                "usermod -aG {} \"${{SUDO_USER:-$USER}}\"",
                need_groups.join(",")
            )],
        });
    }
    // Firecracker is (re)installed when Missing OR when a non-pinned version is
    // present (Warn) — otherwise the doctor's "setup --fix installs the pinned
    // v1.16.0" hint on a version-mismatch would run and install nothing.
    let firecracker_needs_install = arch_ok
        && checks.iter().any(|c| {
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
    if arch_ok && failed("guest_kernel") {
        // GUEST_KERNEL_INSTALL_PATH is a compile-time const (not user input); the
        // .tmp lives under its root-owned parent dir, and the final mv is atomic
        // only after the sha256 check passes.
        let parent = Path::new(GUEST_KERNEL_INSTALL_PATH)
            .parent()
            .unwrap()
            .display();
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
    // Ato binaries the systemd units' ExecStart depends on. Install BEFORE writing
    // the units, so a unit is only ever written when its binary exists (a fresh
    // Ubuntu host would otherwise get a unit that dies on `No such file or
    // directory`). The `<install> src -> dest` pseudo-command is executed by run().
    if failed("ato_cli_binary")
        && let Some(src) = &bins.ato_source
    {
        plan.push(FixAction {
            id: "install_ato_cli",
            title: format!("Install the ato binary to {ATO_CLI_INSTALL_PATH}"),
            commands: vec![format!(
                "<install> {} -> {ATO_CLI_INSTALL_PATH}",
                src.display()
            )],
        });
    }
    if failed("snapshot_builder_binary")
        && let Some(src) = &bins.builder_source
    {
        plan.push(FixAction {
            id: "install_snapshot_builder",
            title: format!("Install ato-snapshot-builder to {BUILDER_INSTALL_PATH}"),
            commands: vec![format!(
                "<install> {} -> {BUILDER_INSTALL_PATH}",
                src.display()
            )],
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
    // Only write a unit whose ExecStart binary is present or will be installed by
    // this same plan — never an enabled-looking unit that cannot start.
    let ato_will_exist = is_ok("ato_cli_binary") || bins.ato_source.is_some();
    let builder_will_exist = is_ok("snapshot_builder_binary") || bins.builder_source.is_some();
    let mut units: Vec<&'static str> = Vec::new();
    if failed("unit_runner") && ato_will_exist {
        units.push(RUNNER_UNIT);
    }
    if failed("unit_builder") && builder_will_exist {
        units.push(BUILDER_UNIT);
    }
    if !units.is_empty() {
        let mut commands: Vec<String> = units
            .iter()
            .map(|u| format!("<write> {SYSTEMD_DIR}/{u}"))
            .collect();
        commands.push("systemctl daemon-reload".to_string());
        plan.push(FixAction {
            id: "systemd_units",
            title: format!(
                "Write {} (existing files backed up), then daemon-reload",
                units.join(" + ")
            ),
            commands,
        });
    }

    // ADR-016: enforce-only CPU delegation drop-in. The check is Ok/Warn (never
    // Missing) unless the env file explicitly selects enforce, so a feature-off
    // setup plans NOTHING here.
    if failed("cpu_delegation_dropin") {
        plan.push(FixAction {
            id: "cpu_delegation_dropin",
            title: format!(
                "Write {} (Delegate=yes + DelegateSubgroup=main), then daemon-reload",
                cpu_delegation_dropin_path()
            ),
            commands: vec![
                format!("<write> {}", cpu_delegation_dropin_path()),
                "systemctl daemon-reload".to_string(),
            ],
        });
    }

    // ── Official-preview extras (ato#1006 ingress PR C) ──
    if let Some(cfg) = &opts.official {
        if failed("caddy") {
            plan.push(FixAction {
                id: "caddy_install",
                title: "Install Caddy (public HTTPS terminator)".to_string(),
                commands: vec![
                    "apt-get update -qq".to_string(),
                    "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq caddy".to_string(),
                ],
            });
        }
        let caddyfile_written = failed("caddyfile");
        if caddyfile_written {
            plan.push(FixAction {
                id: "caddyfile",
                title: format!(
                    "Write {} — base + s0..s{} vhosts → loopback slot ports (existing file backed up)",
                    cfg.caddyfile_path,
                    cfg.max_slots.saturating_sub(1),
                ),
                commands: vec![format!("<write> {}", cfg.caddyfile_path)],
            });
        }
        // A runner unit with a PUBLIC --proxy-listen is rewritten to the
        // loopback default — slot ports must never bypass Caddy.
        if failed("unit_runner_loopback") {
            plan.push(FixAction {
                id: "unit_runner_loopback",
                title: format!(
                    "Rewrite {RUNNER_UNIT}: remove the public --proxy-listen (loopback slot ports only)"
                ),
                commands: vec![
                    format!("<write> {SYSTEMD_DIR}/{RUNNER_UNIT}"),
                    "systemctl daemon-reload".to_string(),
                ],
            });
        }
        if failed("env_official") {
            plan.push(FixAction {
                id: "env_official",
                title: format!(
                    "Append official-preview keys to {ENV_FILE} (ATO_RUNNER_PREVIEW=1, PUBLIC_BASE_URL, MAX_SLOTS)"
                ),
                commands: vec![format!("<write> {ENV_FILE}")],
            });
        }
        // Enable/reload LAST, after the Caddyfile it serves exists.
        if failed("caddy_service") {
            plan.push(FixAction {
                id: "caddy_service",
                title: "Enable + start Caddy".to_string(),
                commands: vec!["systemctl enable --now caddy".to_string()],
            });
        } else if caddyfile_written {
            plan.push(FixAction {
                id: "caddy_reload",
                title: "Reload Caddy with the regenerated Caddyfile".to_string(),
                commands: vec!["systemctl reload caddy".to_string()],
            });
        }
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
        (
            "ATO_API_URL",
            api_url.unwrap_or("https://api.ato.run").to_string(),
        ),
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

/// ADR-016: drop-in path granting the runner service a delegated cgroup
/// subtree. Written ONLY when the env file explicitly selects
/// `ATO_RUNNER_CPU_ENTITLEMENT=enforce`; with the feature off, setup neither
/// writes nor touches it — zero unit diff.
pub(crate) fn cpu_delegation_dropin_path() -> String {
    format!("{SYSTEMD_DIR}/{RUNNER_UNIT}.d/50-cpu-delegation.conf")
}

/// Render the CPU-delegation drop-in. Pure. `Delegate=yes` hands the service's
/// cgroup subtree to the runner; `DelegateSubgroup=main` (systemd ≥ 254;
/// older systemd logs-and-ignores it) moves the service's own processes into a
/// child group so the delegated parent has no interior processes — the cgroup
/// v2 constraint that would otherwise block enabling `+cpu` for slot children.
pub(crate) fn render_cpu_delegation_dropin() -> String {
    "# Written by `ato runner setup` — ADR-016 runtime CPU entitlement (enforce only).\n\
     # Delete this file and daemon-reload to revert; it is only ever written when\n\
     # /etc/ato/runner.env sets ATO_RUNNER_CPU_ENTITLEMENT=enforce.\n\
     [Service]\n\
     Delegate=yes\n\
     DelegateSubgroup=main\n"
        .to_string()
}

/// Render a systemd unit. Pure. Both services run as root in v0 (Firecracker tap
/// setup + /dev/kvm + Docker); they read all config from the env file, so tokens
/// never live in the unit itself.
pub(crate) fn render_unit(unit: &str) -> String {
    // ExecStart uses the SAME install paths setup writes the binaries to, so every
    // unit setup emits has a runnable ExecStart.
    let (desc, exec) = if unit == BUILDER_UNIT {
        (
            "Ato snapshot builder (capsule -> sealed Ready-State artifact)".to_string(),
            // SNAPSHOT_BUILDER_AGENT_TOKEN comes from the env file; %H = hostname.
            format!("{BUILDER_INSTALL_PATH} --agent-id %H --work ${{ATO_SNAPSHOT_ARTIFACT_ROOT}}"),
        )
    } else {
        (
            "Ato connected runner agent (claims run leases, serves restored snapshots)".to_string(),
            format!("{ATO_CLI_INSTALL_PATH} runner serve"),
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

/// `<path>.bak-<epoch>` beside `path`.
fn backup_path(path: &Path) -> PathBuf {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    path.with_file_name(format!(
        "{}.bak-{epoch}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// Back up `path` to `<path>.bak-<epoch>` iff it exists, then write `content`.
fn backup_then_write(path: &Path, content: &str) -> Result<Option<PathBuf>> {
    let mut backup = None;
    if path.exists() {
        let bak = backup_path(path);
        std::fs::copy(path, &bak).with_context(|| format!("backup {} failed", path.display()))?;
        backup = Some(bak);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content).with_context(|| format!("write {} failed", path.display()))?;
    Ok(backup)
}

/// Install `src` → `dest` (0755), backing up an existing `dest` first. A no-op when
/// `src == dest` (the binary is already installed in place — e.g. the running `ato`
/// IS `/usr/local/bin/ato`), so it never truncates itself.
fn install_binary_with_backup(src: &Path, dest: &Path) -> Result<Option<PathBuf>> {
    if src == dest {
        return Ok(None);
    }
    let mut backup = None;
    if dest.exists() {
        let bak = backup_path(dest);
        std::fs::copy(dest, &bak).with_context(|| format!("backup {} failed", dest.display()))?;
        backup = Some(bak);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dest)
        .with_context(|| format!("install {} -> {} failed", src.display(), dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(backup)
}

/// Parse a `<install> {src} -> {dest}` pseudo-command into (src, dest). Pure.
pub(crate) fn parse_install_command(cmd: &str) -> Option<(String, String)> {
    let rest = cmd.strip_prefix("<install> ")?;
    let (src, dest) = rest.split_once(" -> ")?;
    Some((src.trim().to_string(), dest.trim().to_string()))
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
    if let Some(cfg) = &opts.official {
        official_preview::validate_config(cfg)?;
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
    let mut checks = checks::gather();
    if let Some(cfg) = &opts.official {
        checks.extend(official_preview::gather(cfg));
        // Append-only cannot fix a disagreeing operator-set env line — surface
        // the exact manual edit up front (dry-run included).
        let env_vals = std::fs::read_to_string(ENV_FILE)
            .map(|t| checks::env_file_values(&t))
            .unwrap_or_default();
        let (_missing, conflicts) = official_preview::official_env_lines(&env_vals, cfg);
        for c in &conflicts {
            println!("⚠️  {c}");
        }
        if !conflicts.is_empty() {
            println!();
        }
    }
    let bins = BinaryPlan::resolve();
    let blocked: Vec<&Check> = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Blocked)
        .collect();
    let plan = derive_plan(&checks, &opts, &bins);

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

    let artifact_root = opts
        .artifact_root
        .clone()
        .unwrap_or_else(|| DEFAULT_ARTIFACT_ROOT.to_string());
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
            "install_ato_cli" | "install_snapshot_builder" => {
                // `<install> {src} -> {dest}` (built by derive_plan).
                let (src, dest) = parse_install_command(&a.commands[0])
                    .with_context(|| format!("malformed install action: {:?}", a.commands))?;
                let bak = install_binary_with_backup(Path::new(&src), Path::new(&dest))?;
                if let Some(b) = bak {
                    println!("   (previous {dest} backed up to {})", b.display());
                }
            }
            "systemd_units" | "unit_runner_loopback" => {
                // Write EXACTLY the units the plan chose (only those whose ExecStart
                // binary exists / was just installed) — parsed from the plan commands.
                // unit_runner_loopback rewrites the runner unit to the flagless
                // (loopback-default) template through the same path.
                for cmd in &a.commands {
                    if let Some(rest) = cmd.strip_prefix("<write> ") {
                        let path = Path::new(rest);
                        let unit = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let bak = backup_then_write(path, &render_unit(&unit))?;
                        if let Some(b) = bak {
                            println!("   (previous {unit} backed up to {})", b.display());
                        }
                    } else {
                        run_shell(cmd)?;
                    }
                }
            }
            "cpu_delegation_dropin" => {
                for cmd in &a.commands {
                    if let Some(rest) = cmd.strip_prefix("<write> ") {
                        let path = Path::new(rest);
                        if let Some(dir) = path.parent() {
                            std::fs::create_dir_all(dir)
                                .with_context(|| format!("create {}", dir.display()))?;
                        }
                        let bak = backup_then_write(path, &render_cpu_delegation_dropin())?;
                        if let Some(b) = bak {
                            println!("   (previous drop-in backed up to {})", b.display());
                        }
                    } else {
                        run_shell(cmd)?;
                    }
                }
            }
            "caddyfile" => {
                let cfg = opts
                    .official
                    .as_ref()
                    .expect("caddyfile action only in official mode");
                let content = official_preview::render_caddyfile(
                    &official_preview::base_hostname(&cfg.public_base_url),
                    cfg.max_slots,
                );
                let bak = backup_then_write(Path::new(&cfg.caddyfile_path), &content)?;
                if let Some(b) = bak {
                    println!("   (previous Caddyfile backed up to {})", b.display());
                }
            }
            "env_official" => {
                let cfg = opts
                    .official
                    .as_ref()
                    .expect("env_official action only in official mode");
                let existing_text = std::fs::read_to_string(ENV_FILE).unwrap_or_default();
                let existing = checks::env_file_values(&existing_text);
                let (lines, _conflicts) = official_preview::official_env_lines(&existing, cfg);
                let mut content = existing_text;
                if !content.is_empty() && !content.ends_with('\n') {
                    content.push('\n');
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
            _ => {
                for c in &a.commands {
                    run_shell(c)?;
                }
            }
        }
    }

    // Honest next steps: only mention re-login if a group was actually added, and
    // only name the units that were actually written.
    let added_group = plan.iter().any(|a| a.id == "usermod_groups");
    let written_units: Vec<&str> = plan
        .iter()
        .find(|a| a.id == "systemd_units")
        .map(|a| {
            a.commands
                .iter()
                .filter_map(|c| c.strip_prefix("<write> "))
                .filter_map(|p| Path::new(p).file_name().map(|f| f.to_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default();
    println!();
    println!("Setup complete. Next steps:");
    let mut n = 1;
    if added_group {
        println!("  {n}. re-login so the new group membership takes effect");
        n += 1;
    }
    println!("  {n}. ato runner login                      # enroll this host");
    n += 1;
    println!(
        "  {n}. edit {ENV_FILE}: set SNAPSHOT_BUILDER_AGENT_TOKEN (builder) + ATO_RUNNER_PUBLIC_BASE_URL (tunnel)"
    );
    n += 1;
    if !written_units.is_empty() {
        println!("  {n}. systemctl enable --now {}", written_units.join(" "));
        n += 1;
    }
    println!("  {n}. ato runner smoke                      # verify the full local path");
    println!(
        "  {}. ato doctor runner                     # confirm everything is green",
        n + 1
    );
    if let Some(cfg) = &opts.official {
        println!();
        println!("Official-preview firewall (this host must expose ONLY Caddy):");
        println!("  • allow inbound TCP 80 + 443 (Caddy: ACME challenge + public HTTPS)");
        println!("  • do NOT open 8420+ — slot ports are loopback-only; a public slot port");
        println!("    would bypass Caddy, TLS, and the slot-hostname allowlist");
        println!("  • restrict SSH to the admin IP / a private network (e.g. Tailscale)");
        println!();
        println!(
            "Then run Validate on this runner's ingress in the admin console — it probes\n\
             https://{}{} on the base + every slot hostname.",
            official_preview::base_hostname(&cfg.public_base_url),
            official_preview::WELLKNOWN_PATH,
        );
    }
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
        Check {
            id,
            label: "x",
            status,
            detail: "not installed".into(),
            fix: None,
        }
    }
    fn opts() -> SetupOptions {
        SetupOptions {
            fix: false,
            yes: false,
            artifact_root: None,
            api_url: None,
            official: None,
        }
    }
    fn official_opts() -> SetupOptions {
        SetupOptions {
            official: Some(OfficialPreviewConfig {
                public_base_url: "https://runner-abc.runner.ato.run".into(),
                max_slots: 2,
                caddyfile_path: "/etc/caddy/Caddyfile".into(),
                hold_proxy_listen: None,
            }),
            ..opts()
        }
    }
    /// Both binaries available (release layout) — the common case.
    fn bins() -> BinaryPlan {
        BinaryPlan {
            ato_source: Some("/src/ato".into()),
            builder_source: Some("/src/ato-snapshot-builder".into()),
        }
    }

    #[test]
    fn plan_is_empty_when_everything_passes() {
        let all_ok: Vec<Check> = ["docker", "firecracker", "artifact_root", "env_file"]
            .iter()
            .map(|id| check(id, CheckStatus::Ok))
            .collect();
        assert!(derive_plan(&all_ok, &opts(), &bins()).is_empty());
    }

    #[test]
    fn cpu_delegation_dropin_planned_only_when_check_missing() {
        // Missing (enforce requested, drop-in absent/stale) → planned, with a
        // write + daemon-reload.
        let plan = derive_plan(
            &[check("cpu_delegation_dropin", CheckStatus::Missing)],
            &opts(),
            &bins(),
        );
        let action = plan
            .iter()
            .find(|a| a.id == "cpu_delegation_dropin")
            .expect("planned");
        assert!(action.commands[0].starts_with("<write> "));
        assert!(action.commands[0].contains("50-cpu-delegation.conf"));
        assert_eq!(action.commands[1], "systemctl daemon-reload");
        // Ok (off, or already current) → feature-off setups plan NOTHING here.
        let plan = derive_plan(
            &[check("cpu_delegation_dropin", CheckStatus::Ok)],
            &opts(),
            &bins(),
        );
        assert!(plan.iter().all(|a| a.id != "cpu_delegation_dropin"));
        // Warn (stale drop-in while off) is advisory only — never auto-fixed.
        let plan = derive_plan(
            &[check("cpu_delegation_dropin", CheckStatus::Warn)],
            &opts(),
            &bins(),
        );
        assert!(plan.iter().all(|a| a.id != "cpu_delegation_dropin"));
    }

    #[test]
    fn cpu_delegation_dropin_renders_delegation() {
        let text = render_cpu_delegation_dropin();
        assert!(text.contains("[Service]"));
        assert!(text.contains("Delegate=yes"));
        assert!(text.contains("DelegateSubgroup=main"));
        assert!(cpu_delegation_dropin_path().ends_with(".service.d/50-cpu-delegation.conf"));
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
            check("ato_cli_binary", CheckStatus::Missing),
            check("snapshot_builder_binary", CheckStatus::Missing),
            check("env_file", CheckStatus::Missing),
            check("unit_builder", CheckStatus::Missing),
            check("unit_runner", CheckStatus::Missing),
            check("tool_pnpm", CheckStatus::Warn), // optional — never auto-installed
        ];
        let plan = derive_plan(&checks, &opts(), &bins());
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
                "install_ato_cli",
                "install_snapshot_builder",
                "env_file",
                "systemd_units",
            ]
        );
        // Binaries are installed BEFORE the units that depend on them.
        let pos = |id: &str| ids.iter().position(|x| *x == id).unwrap();
        assert!(pos("install_ato_cli") < pos("systemd_units"));
        assert!(pos("install_snapshot_builder") < pos("systemd_units"));
        // Downloads are pinned: the verify command carries the exact sha256.
        let fc = plan.iter().find(|a| a.id == "firecracker_install").unwrap();
        assert!(
            fc.commands
                .iter()
                .any(|c| c.contains(FC_TGZ_SHA256) && c.contains("sha256sum -c"))
        );
        let k = plan.iter().find(|a| a.id == "kernel_install").unwrap();
        assert!(
            k.commands
                .iter()
                .any(|c| c.contains(GUEST_KERNEL_SHA256) && c.contains("sha256sum -c"))
        );
        // Nothing in the plan touches the Blocked check.
        assert!(plan.iter().all(|a| a.id != "cpu_virt"));
    }

    #[test]
    fn official_preview_plan_orders_caddy_actions_and_never_leaks_into_classic_mode() {
        let failing = vec![
            check("caddy", CheckStatus::Missing),
            check("caddyfile", CheckStatus::Missing),
            check("caddy_service", CheckStatus::Missing),
            check("unit_runner_loopback", CheckStatus::Missing),
            check("env_official", CheckStatus::Missing),
        ];
        // Classic mode: official check ids derive NOTHING (byte-identical plans).
        assert!(derive_plan(&failing, &opts(), &bins()).is_empty());

        let plan = derive_plan(&failing, &official_opts(), &bins());
        let ids: Vec<&str> = plan.iter().map(|a| a.id).collect();
        assert_eq!(
            ids,
            vec![
                "caddy_install",
                "caddyfile",
                "unit_runner_loopback",
                "env_official",
                "caddy_service"
            ]
        );
        // Caddy is enabled AFTER the Caddyfile it serves is written.
        let pos = |id: &str| ids.iter().position(|x| *x == id).unwrap();
        assert!(pos("caddyfile") < pos("caddy_service"));
        // The loopback rewrite reloads systemd.
        let loopback = plan
            .iter()
            .find(|a| a.id == "unit_runner_loopback")
            .unwrap();
        assert!(
            loopback
                .commands
                .iter()
                .any(|c| c == "systemctl daemon-reload")
        );
    }

    #[test]
    fn official_preview_reloads_caddy_when_only_the_caddyfile_changed() {
        // Caddy installed + active, but the Caddyfile needs regenerating (e.g.
        // max_slots changed): plan = write + reload, NOT enable.
        let failing = vec![
            check("caddy", CheckStatus::Ok),
            check("caddyfile", CheckStatus::Missing),
            check("caddy_service", CheckStatus::Ok),
        ];
        let plan = derive_plan(&failing, &official_opts(), &bins());
        let ids: Vec<&str> = plan.iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["caddyfile", "caddy_reload"]);
    }

    #[test]
    fn custom_artifact_root_flows_into_the_plan() {
        let checks = vec![check("artifact_root", CheckStatus::Missing)];
        let o = SetupOptions {
            artifact_root: Some("/srv/snapshots".into()),
            ..opts()
        };
        let plan = derive_plan(&checks, &o, &bins());
        assert!(
            plan[0]
                .commands
                .iter()
                .any(|c| c.contains("/srv/snapshots"))
        );
    }

    #[test]
    fn missing_binaries_derive_install_actions_and_units_wait_for_them() {
        // Fresh host: both binaries missing but sources available ⇒ install actions,
        // and both units are written (their ExecStart binaries will exist).
        let checks = vec![
            check("ato_cli_binary", CheckStatus::Missing),
            check("snapshot_builder_binary", CheckStatus::Missing),
            check("unit_runner", CheckStatus::Missing),
            check("unit_builder", CheckStatus::Missing),
        ];
        let plan = derive_plan(&checks, &opts(), &bins());
        let ids: Vec<&str> = plan.iter().map(|a| a.id).collect();
        assert!(ids.contains(&"install_ato_cli") && ids.contains(&"install_snapshot_builder"));
        let units = plan.iter().find(|a| a.id == "systemd_units").unwrap();
        assert_eq!(
            units
                .commands
                .iter()
                .filter(|c| c.starts_with("<write>"))
                .count(),
            2
        );

        // No snapshot-builder source ⇒ NO builder install AND NO builder unit (its
        // ExecStart could never be made to exist), but the runner unit is still written.
        let no_builder = BinaryPlan {
            ato_source: Some("/src/ato".into()),
            builder_source: None,
        };
        let plan = derive_plan(&checks, &opts(), &no_builder);
        let ids: Vec<&str> = plan.iter().map(|a| a.id).collect();
        assert!(!ids.contains(&"install_snapshot_builder"));
        let units = plan.iter().find(|a| a.id == "systemd_units").unwrap();
        let written: Vec<&String> = units
            .commands
            .iter()
            .filter(|c| c.starts_with("<write>"))
            .collect();
        assert_eq!(written.len(), 1);
        assert!(written[0].contains(RUNNER_UNIT) && !written[0].contains(BUILDER_UNIT));
    }

    #[test]
    fn install_action_source_and_dest_are_the_install_paths() {
        let checks = vec![check("ato_cli_binary", CheckStatus::Missing)];
        let plan = derive_plan(&checks, &opts(), &bins());
        let a = plan.iter().find(|a| a.id == "install_ato_cli").unwrap();
        let (src, dest) = parse_install_command(&a.commands[0]).unwrap();
        assert_eq!(src, "/src/ato");
        assert_eq!(dest, ATO_CLI_INSTALL_PATH);
        // And the unit ExecStart uses that exact dest — so the written unit is runnable.
        assert!(
            render_unit(RUNNER_UNIT)
                .contains(&format!("ExecStart={ATO_CLI_INSTALL_PATH} runner serve"))
        );
        assert!(render_unit(BUILDER_UNIT).contains(&format!("ExecStart={BUILDER_INSTALL_PATH}")));
    }

    #[test]
    fn non_x86_64_arch_blocks_the_firecracker_and_kernel_install() {
        assert_eq!(checks::parse_arch("x86_64"), "x86_64");
        assert_eq!(checks::parse_arch("amd64"), "x86_64");
        assert_eq!(checks::parse_arch("aarch64"), "aarch64");
        // Arch Blocked ⇒ neither the x86_64 Firecracker nor the kernel is planned,
        // even though both are Missing.
        let checks = vec![
            check("arch", CheckStatus::Blocked),
            check("firecracker", CheckStatus::Missing),
            check("guest_kernel", CheckStatus::Missing),
            check("docker", CheckStatus::Missing), // arch-independent — still planned
        ];
        let plan = derive_plan(&checks, &opts(), &bins());
        let ids: Vec<&str> = plan.iter().map(|a| a.id).collect();
        assert!(
            !ids.contains(&"firecracker_install"),
            "must not install x86_64 FC on non-x86_64"
        );
        assert!(
            !ids.contains(&"kernel_install"),
            "must not install x86_64 kernel on non-x86_64"
        );
        assert!(
            ids.contains(&"docker_enable"),
            "arch-independent fixes still planned"
        );
    }

    #[test]
    fn install_binary_preserves_existing_and_is_self_safe() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src-ato");
        let dest = dir.path().join("bin").join("ato");
        std::fs::write(&src, b"NEW").unwrap();
        // Fresh dest: no backup, content copied.
        assert!(install_binary_with_backup(&src, &dest).unwrap().is_none());
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
        // Existing dest: previous content backed up, new content installed.
        std::fs::write(&src, b"NEWER").unwrap();
        let bak = install_binary_with_backup(&src, &dest)
            .unwrap()
            .expect("backup expected");
        assert_eq!(std::fs::read(&bak).unwrap(), b"NEW");
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEWER");
        // src == dest ⇒ no-op (never truncates the running binary onto itself).
        assert!(install_binary_with_backup(&dest, &dest).unwrap().is_none());
        assert_eq!(std::fs::read(&dest).unwrap(), b"NEWER");
    }

    #[test]
    fn env_file_merge_is_append_only() {
        let mut existing = BTreeMap::new();
        existing.insert(
            "ATO_API_URL".to_string(),
            "https://staging-api.ato.run".to_string(),
        );
        existing.insert("ATO_FC_BIN".to_string(), "/opt/fc/firecracker".to_string());
        let lines = env_file_missing_lines(
            &existing,
            "/var/lib/ato/snapshots",
            Some("https://api.ato.run"),
        );
        // Operator-set keys are NOT re-emitted (their values stay untouched)…
        assert!(
            lines
                .iter()
                .all(|l| !l.starts_with("ATO_API_URL=") && !l.starts_with("ATO_FC_BIN="))
        );
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
            assert!(
                !text.to_lowercase().contains("token="),
                "unit must not embed a token: {text}"
            );
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
        let plan = derive_plan(&checks, &opts(), &bins());
        let fc = &plan[0].commands[0];
        assert!(
            fc.contains("mktemp -d"),
            "must download into a private tmp dir: {fc}"
        );
        assert!(
            fc.contains(FC_TGZ_SHA256) && fc.contains("sha256sum -c"),
            "must pin sha256"
        );
        assert!(
            !fc.contains("/tmp/ato-fc.tgz"),
            "must not use a predictable /tmp path"
        );
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
        let bak = backup_then_write(&path, "a=2\n")
            .unwrap()
            .expect("backup expected");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "a=1\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a=2\n");
    }
}
