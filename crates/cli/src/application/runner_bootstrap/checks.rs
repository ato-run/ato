//! Host checks for the capsule-runner profile. Read-only: every probe here only
//! observes (file presence, `--version`, group membership) — mutation lives in
//! [`super::setup`] behind an explicit confirmation.
//!
//! Pure parsers are split from the probes so the classification logic is testable
//! without a KVM/Docker host.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use super::{
    Check, DEFAULT_ARTIFACT_ROOT, ENV_FILE, FC_INSTALL_PATH, FC_VERSION,
    GUEST_KERNEL_INSTALL_PATH,
};

// ── Pure parsers ──

/// `(id, version_id)` from /etc/os-release text (values may be quoted).
pub(crate) fn parse_os_release(text: &str) -> (Option<String>, Option<String>) {
    let val = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .map(|v| v.trim().trim_matches('"').to_string())
    };
    (val("ID"), val("VERSION_ID"))
}

/// True when /proc/cpuinfo advertises hardware virtualization (vmx=Intel, svm=AMD).
pub(crate) fn cpu_has_virt(cpuinfo: &str) -> bool {
    cpuinfo
        .lines()
        .filter(|l| l.starts_with("flags") || l.starts_with("Features"))
        .any(|l| {
            l.split_whitespace()
                .any(|f| f == "vmx" || f == "svm")
        })
}

/// True when `groups_output` (space-separated group names) contains `group`.
pub(crate) fn groups_contain(groups_output: &str, group: &str) -> bool {
    groups_output.split_whitespace().any(|g| g == group)
}

/// Parse a KEY=VALUE env file (comments/blank lines ignored; later keys win).
pub(crate) fn env_file_values(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

// ── Probe helpers ──

fn cmd_stdout(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn which(bin: &str) -> Option<String> {
    cmd_stdout("sh", &["-c", &format!("command -v {bin}")]).filter(|s| !s.is_empty())
}

fn is_root() -> bool {
    // Effective uid via `id -u` — no unsafe libc geteuid needed for a diagnostic.
    cmd_stdout("id", &["-u"]).as_deref() == Some("0")
}

/// Resolve the Firecracker binary this host would use: `ATO_FC_BIN` (explicit) →
/// the setup install path → `firecracker` on PATH.
pub(crate) fn resolve_fc_bin() -> Option<String> {
    if let Ok(v) = std::env::var("ATO_FC_BIN")
        && !v.trim().is_empty()
    {
        return Some(v);
    }
    if Path::new(FC_INSTALL_PATH).exists() {
        return Some(FC_INSTALL_PATH.to_string());
    }
    which("firecracker")
}

/// Resolve the guest kernel: `ATO_FC_KERNEL` (explicit) → the setup install path.
pub(crate) fn resolve_guest_kernel() -> Option<String> {
    if let Ok(v) = std::env::var("ATO_FC_KERNEL")
        && !v.trim().is_empty()
    {
        return Some(v);
    }
    Path::new(GUEST_KERNEL_INSTALL_PATH)
        .exists()
        .then(|| GUEST_KERNEL_INSTALL_PATH.to_string())
}

/// Resolve the artifact root: `ATO_SNAPSHOT_ARTIFACT_ROOT` → env file → default.
pub(crate) fn resolve_artifact_root() -> String {
    if let Ok(v) = std::env::var("ATO_SNAPSHOT_ARTIFACT_ROOT")
        && !v.trim().is_empty()
    {
        return v;
    }
    if let Ok(text) = std::fs::read_to_string(ENV_FILE)
        && let Some(v) = env_file_values(&text).get("ATO_SNAPSHOT_ARTIFACT_ROOT")
    {
        return v.clone();
    }
    DEFAULT_ARTIFACT_ROOT.to_string()
}

// ── The capsule-runner check suite ──

/// All read-only checks for the capsule-runner profile, in display order.
pub(crate) fn gather() -> Vec<Check> {
    let mut checks = Vec::new();

    // OS: Ubuntu is the only supported v0 host — anything else is Blocked (we
    // will not apt-install on a distro we have not validated).
    checks.push(match std::fs::read_to_string("/etc/os-release") {
        Ok(text) => {
            let (id, ver) = parse_os_release(&text);
            match id.as_deref() {
                Some("ubuntu") => Check::ok(
                    "os_ubuntu",
                    "Ubuntu",
                    format!("Ubuntu {}", ver.unwrap_or_else(|| "?".into())),
                ),
                other => Check::blocked(
                    "os_ubuntu",
                    "Ubuntu",
                    format!(
                        "unsupported OS {:?} — v0 supports Ubuntu only; install Ubuntu 22.04/24.04",
                        other.unwrap_or("unknown")
                    ),
                ),
            }
        }
        Err(e) => Check::blocked("os_ubuntu", "Ubuntu", format!("cannot read /etc/os-release: {e}")),
    });

    // CPU virtualization: software cannot enable this — the one truly manual step.
    checks.push(match std::fs::read_to_string("/proc/cpuinfo") {
        Ok(text) if cpu_has_virt(&text) => Check::ok("cpu_virt", "CPU virtualization", "vmx/svm present"),
        Ok(_) => Check::blocked(
            "cpu_virt",
            "CPU virtualization",
            "no vmx/svm CPU flag — enable VT-x/AMD-V in BIOS/UEFI (or use a nested-virt cloud shape)",
        ),
        Err(e) => Check::blocked("cpu_virt", "CPU virtualization", format!("cannot read /proc/cpuinfo: {e}")),
    });

    // /dev/kvm: exists AND this user can open it (existence alone is not access).
    checks.push(if Path::new("/dev/kvm").exists() {
        match std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm") {
            Ok(_) => Check::ok("kvm_device", "/dev/kvm", "present, read-write"),
            Err(e) => Check::missing(
                "kvm_device",
                "/dev/kvm",
                format!("present but not openable: {e}"),
                "add this user to the kvm group (sudo usermod -aG kvm $USER, then re-login)",
            ),
        }
    } else {
        Check::missing(
            "kvm_device",
            "/dev/kvm",
            "absent — KVM module not loaded (or virtualization disabled)",
            "sudo modprobe kvm_intel|kvm_amd (persists via /etc/modules-load.d)",
        )
    });

    // Group membership (root bypasses groups; report honestly either way).
    let root = is_root();
    let groups = cmd_stdout("groups", &[]).unwrap_or_default();
    for (id, label, group) in
        [("kvm_group", "user in kvm group", "kvm"), ("docker_group", "user in docker group", "docker")]
    {
        checks.push(if root {
            Check::ok(id, label, "running as root (group not required)")
        } else if groups_contain(&groups, group) {
            Check::ok(id, label, format!("member of {group}"))
        } else {
            Check::missing(
                id,
                label,
                format!("not a member of {group}"),
                format!("sudo usermod -aG {group} $USER (then re-login)"),
            )
        });
    }

    // Docker: binary + daemon actually answering (a stopped daemon is not "installed").
    checks.push(match which("docker") {
        Some(_) => match cmd_stdout("docker", &["version", "--format", "{{.Server.Version}}"]) {
            Some(v) => Check::ok("docker", "Docker", format!("daemon {v}")),
            None => Check::missing(
                "docker",
                "Docker",
                "binary present but the daemon is not reachable",
                "sudo systemctl enable --now docker (and docker group membership)",
            ),
        },
        None => Check::missing("docker", "Docker", "not installed", "apt install docker.io"),
    });

    // Firecracker: pinned version preferred; another version is Warn, not Ok —
    // the KVM validations all ran on the pinned stack.
    checks.push(match resolve_fc_bin() {
        Some(bin) => match cmd_stdout(&bin, &["--version"]) {
            Some(v) if v.contains(FC_VERSION.trim_start_matches('v')) => {
                Check::ok("firecracker", "Firecracker", format!("{bin}: {}", v.lines().next().unwrap_or(&v)))
            }
            Some(v) => Check::warn(
                "firecracker",
                "Firecracker",
                format!("{bin}: {} (validated stack is {FC_VERSION})", v.lines().next().unwrap_or(&v)),
                format!("ato runner setup --fix installs the pinned {FC_VERSION}"),
            ),
            None => Check::missing(
                "firecracker",
                "Firecracker",
                format!("{bin} did not answer --version"),
                "ato runner setup --fix reinstalls the pinned release",
            ),
        },
        None => Check::missing(
            "firecracker",
            "Firecracker",
            "not installed",
            format!("ato runner setup --fix installs {FC_VERSION} (sha256-verified) to {FC_INSTALL_PATH}"),
        ),
    });

    // Guest kernel.
    checks.push(match resolve_guest_kernel() {
        Some(k) => Check::ok("guest_kernel", "guest kernel", k),
        None => Check::missing(
            "guest_kernel",
            "guest kernel",
            "vmlinux not found (ATO_FC_KERNEL unset, no installed kernel)",
            format!("ato runner setup --fix installs vmlinux-5.10.223 (sha256-verified) to {GUEST_KERNEL_INSTALL_PATH}"),
        ),
    });

    // tun/tap + cgroup v2 (Firecracker networking + jailer expectations).
    checks.push(if Path::new("/dev/net/tun").exists() {
        Check::ok("tun_tap", "tun/tap", "/dev/net/tun present")
    } else {
        Check::missing("tun_tap", "tun/tap", "/dev/net/tun absent", "sudo modprobe tun")
    });
    checks.push(if Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        Check::ok("cgroup_v2", "cgroup v2", "unified hierarchy mounted")
    } else {
        Check::warn(
            "cgroup_v2",
            "cgroup v2",
            "unified hierarchy not detected",
            "boot with systemd.unified_cgroup_hierarchy=1 (Ubuntu ≥21.10 default)",
        )
    });

    // Required + optional tooling. rust/node/pnpm/wrangler are only needed to
    // build ato from source / run a local control plane — absent is Warn, not Missing.
    for (id, bin) in [("tool_git", "git"), ("tool_curl", "curl")] {
        checks.push(match which(bin) {
            Some(p) => Check::ok(id, bin_label(bin), p),
            None => Check::missing(id, bin_label(bin), "not installed", format!("apt install {bin}")),
        });
    }
    for (id, bin, hint) in [
        ("tool_cargo", "cargo", "rustup.rs (only needed to build ato/snapshot-builder from source)"),
        ("tool_node", "node", "nodesource or nvm (only needed for a local ato-api control plane)"),
        ("tool_pnpm", "pnpm", "npm i -g pnpm (only needed for a local ato-api control plane)"),
        ("tool_wrangler", "wrangler", "pnpm add -g wrangler (only needed for a local ato-api control plane)"),
    ] {
        checks.push(match which(bin) {
            Some(p) => Check::ok(id, bin_label(bin), p),
            None => Check::warn(id, bin_label(bin), "not installed (optional)", hint),
        });
    }

    // Artifact root: exists AND writable by this user.
    let root_dir = resolve_artifact_root();
    checks.push(match std::fs::metadata(&root_dir) {
        Ok(m) if m.is_dir() => {
            let probe = Path::new(&root_dir).join(".ato-doctor-probe");
            match std::fs::write(&probe, b"probe") {
                Ok(()) => {
                    let _ = std::fs::remove_file(&probe);
                    Check::ok("artifact_root", "artifact root", format!("{root_dir} (writable)"))
                }
                Err(e) => Check::missing(
                    "artifact_root",
                    "artifact root",
                    format!("{root_dir} exists but is not writable: {e}"),
                    "ato runner setup --fix chowns it to the operating user",
                ),
            }
        }
        _ => Check::missing(
            "artifact_root",
            "artifact root",
            format!("{root_dir} absent"),
            "ato runner setup --fix creates it",
        ),
    });

    // Env file + the values services need.
    checks.push(match std::fs::read_to_string(ENV_FILE) {
        Ok(text) => {
            let vals = env_file_values(&text);
            let missing: Vec<&str> = ["ATO_SNAPSHOT_ARTIFACT_ROOT", "ATO_API_URL", "ATO_FC_BIN", "ATO_FC_KERNEL"]
                .into_iter()
                .filter(|k| !vals.contains_key(*k))
                .collect();
            if missing.is_empty() {
                Check::ok("env_file", "runner env file", format!("{ENV_FILE} complete"))
            } else {
                Check::missing(
                    "env_file",
                    "runner env file",
                    format!("{ENV_FILE} lacks {}", missing.join(", ")),
                    "ato runner setup --fix appends the missing keys (existing lines untouched)",
                )
            }
        }
        Err(_) => Check::missing(
            "env_file",
            "runner env file",
            format!("{ENV_FILE} absent"),
            "ato runner setup --fix writes it",
        ),
    });

    // Public URL + runner token: needed before serving publicly, but not for the
    // build/smoke paths — Warn with the exact follow-up.
    let env_vals = std::fs::read_to_string(ENV_FILE).map(|t| env_file_values(&t)).unwrap_or_default();
    checks.push(
        match std::env::var("ATO_RUNNER_PUBLIC_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| env_vals.get("ATO_RUNNER_PUBLIC_BASE_URL").cloned())
        {
            Some(u) => Check::ok("public_url", "public URL", u),
            None => Check::warn(
                "public_url",
                "public URL",
                "not configured — apps will be reachable on this host only",
                "configure a tunnel (Cloudflare Tunnel / Tailscale Funnel / ngrok) and set ATO_RUNNER_PUBLIC_BASE_URL",
            ),
        },
    );
    checks.push({
        let creds = crate::application::runner_agent::credentials_path();
        if creds.exists() {
            Check::ok("runner_token", "runner token", creds.display().to_string())
        } else {
            Check::warn(
                "runner_token",
                "runner token",
                "no runner credentials — this host is not enrolled",
                "ato runner login (or --enrollment-token for headless hosts)",
            )
        }
    });

    // systemd units, when present (setup installs them; absent is Missing-with-fix).
    for (id, unit) in
        [("unit_builder", super::BUILDER_UNIT), ("unit_runner", super::RUNNER_UNIT)]
    {
        let state = cmd_stdout("systemctl", &["is-active", unit]);
        checks.push(match state.as_deref() {
            Some("active") => Check::ok(id, unit_label(unit), "active"),
            Some(other) if Path::new(super::SYSTEMD_DIR).join(unit).exists() => Check::warn(
                id,
                unit_label(unit),
                format!("installed, {other}"),
                format!("systemctl status {unit} (start it once its env/token is configured)"),
            ),
            _ => Check::missing(
                id,
                unit_label(unit),
                "not installed",
                "ato runner setup --fix writes the unit",
            ),
        });
    }

    checks
}

fn bin_label(bin: &str) -> &'static str {
    match bin {
        "git" => "git",
        "curl" => "curl",
        "cargo" => "rust/cargo (optional)",
        "node" => "node (optional)",
        "pnpm" => "pnpm (optional)",
        "wrangler" => "wrangler (optional)",
        _ => "tool",
    }
}

fn unit_label(unit: &str) -> &'static str {
    if unit.contains("builder") { "snapshot-builder service" } else { "runner-agent service" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_release_with_and_without_quotes() {
        let (id, ver) = parse_os_release("NAME=\"Ubuntu\"\nID=ubuntu\nVERSION_ID=\"22.04\"\n");
        assert_eq!(id.as_deref(), Some("ubuntu"));
        assert_eq!(ver.as_deref(), Some("22.04"));
        let (id, _) = parse_os_release("ID=debian\n");
        assert_eq!(id.as_deref(), Some("debian"));
        // ID_LIKE must not shadow ID.
        let (id, _) = parse_os_release("ID_LIKE=debian\nID=ubuntu\n");
        assert_eq!(id.as_deref(), Some("ubuntu"));
    }

    #[test]
    fn cpu_virt_detects_vmx_and_svm_only_in_flag_lines() {
        assert!(cpu_has_virt("flags\t\t: fpu vme vmx ssse3\n"));
        assert!(cpu_has_virt("flags\t\t: fpu svm sse2\n"));
        assert!(!cpu_has_virt("flags\t\t: fpu vme sse2\n"));
        // "vmx" inside another token or outside a flags line must not count.
        assert!(!cpu_has_virt("model name : vmx-emulator 3000\n"));
        assert!(!cpu_has_virt("flags\t\t: avmx2\n"));
    }

    #[test]
    fn groups_membership_is_exact_token_match() {
        assert!(groups_contain("ubuntu adm kvm docker", "kvm"));
        assert!(!groups_contain("ubuntu adm kvm-alike docker", "kvm"));
        assert!(!groups_contain("", "kvm"));
    }

    #[test]
    fn env_file_parse_ignores_comments_and_trims() {
        let vals = env_file_values(
            "# comment\nATO_API_URL=http://x\n\n  ATO_FC_BIN = /usr/local/bin/firecracker \n#ATO_OFF=1\n",
        );
        assert_eq!(vals.get("ATO_API_URL").map(String::as_str), Some("http://x"));
        assert_eq!(vals.get("ATO_FC_BIN").map(String::as_str), Some("/usr/local/bin/firecracker"));
        assert!(!vals.contains_key("ATO_OFF"));
    }
}
