//! `ato runner setup --official-preview` — prepare a fixed-IP host as an
//! OFFICIAL preview runner behind Caddy (ato#1006 ingress PR C).
//!
//! The ato-admin console provisions the runner's managed ingress (hierarchical
//! hostname family + grey-cloud DNS A records) and hands the operator its
//! public base URL, e.g. `https://runner-abc.runner.ato.run`. This mode makes
//! the BOX side match that contract:
//!
//!   - Caddy terminates public HTTPS per slot hostname (`<base>` +
//!     `s<N>.<base>`) and proxies each to its LOOPBACK slot port
//!     (`127.0.0.1:8420 + N`) — the Caddyfile is generated here from the same
//!     hostname scheme the control plane registered, so the two sides cannot
//!     drift.
//!   - `ato runner serve` keeps its loopback-only default bind. A runner unit
//!     that passes a non-loopback `--proxy-listen` is flagged and rewritten:
//!     a publicly bound slot port would let clients bypass Caddy/TLS AND the
//!     control plane's slot-hostname allowlist entirely.
//!   - /etc/ato/runner.env gains (append-only) `ATO_RUNNER_PREVIEW=1`,
//!     `ATO_RUNNER_PUBLIC_BASE_URL=<base>`, `ATO_RUNNER_MAX_SLOTS=<n>` — the
//!     systemd service reads all three; PREVIEW=1 is what makes the runner
//!     advertise `restore_snapshot_preview` (once KVM/Firecracker are ready).
//!
//! Pure logic only in this module (validation, planning, rendering, parsing);
//! host probes return `Check`s and the mutation runs through setup.rs's
//! plan/confirm/apply flow.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};

use super::{Check, checks};

/// Where the generated Caddyfile goes unless the operator overrides it.
pub(crate) const DEFAULT_CADDYFILE_PATH: &str = "/etc/caddy/Caddyfile";
pub(crate) const CADDY_UNIT: &str = "caddy.service";
/// First loopback slot port — must match `runner_agent::DEFAULT_PROXY_LISTEN`
/// (asserted in tests) and the control plane's Caddyfile generator.
pub(crate) const LOCAL_BASE_PORT: u16 = 8420;
/// The path Caddy answers 200 on for every ingress vhost, independently of the
/// slot app behind it — the same constant the ato-admin validate probes.
pub(crate) const WELLKNOWN_PATH: &str = "/.well-known/ato-runner-ingress";

/// Official-preview inputs, validated before any plan is derived.
#[derive(Debug, Clone)]
pub(crate) struct OfficialPreviewConfig {
    /// e.g. `https://runner-abc.runner.ato.run` — the ato-managed base the
    /// admin console provisioned. Its host anchors the whole hostname family.
    pub public_base_url: String,
    pub max_slots: usize,
    pub caddyfile_path: String,
}

/// The base hostname extracted from a VALIDATED public base URL.
pub(crate) fn base_hostname(public_base_url: &str) -> String {
    public_base_url
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Validate the managed public base URL:
///   - `https://<hostname>` EXACTLY — no port (Caddy owns 443), no path, no
///     query/fragment, no userinfo;
///   - hostname is a DNS name, not an IP literal / loopback (the control plane
///     blocks raw-IP upstreams — Cloudflare error 1003);
///   - env-file-safe (it is written into /etc/ato/runner.env, so whitespace or
///     control characters would be env-line injection).
pub(crate) fn validate_public_base_url(url: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        bail!("--public-base-url must be https:// (got {url:?}) — Caddy terminates TLS on 443");
    };
    let host = rest.trim_end_matches('/');
    if host.is_empty()
        || rest.matches('/').count() > 1
        || (rest.contains('/') && !rest.ends_with('/'))
    {
        bail!("--public-base-url must be exactly https://<hostname> with no path (got {url:?})");
    }
    if host.contains(':') || host.contains('@') || host.contains('?') || host.contains('#') {
        bail!(
            "--public-base-url must not contain a port, userinfo, query, or fragment (got {url:?})"
        );
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("--public-base-url must not contain whitespace or control characters");
    }
    let h = host.to_ascii_lowercase();
    if h == "localhost" || h.starts_with("127.") {
        bail!("--public-base-url must not be loopback (got {url:?})");
    }
    let is_ipv4 = h.split('.').count() == 4 && h.split('.').all(|o| o.parse::<u8>().is_ok());
    if is_ipv4 {
        bail!(
            "--public-base-url must be a DNS hostname, not an IP literal (a Cloudflare Worker cannot fetch a raw-IP upstream — error 1003)"
        );
    }
    if !h.contains('.')
        || !h
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        bail!(
            "--public-base-url hostname may only contain [a-z0-9.-] and must be a dotted DNS name (got {url:?})"
        );
    }
    // DNS label boundaries: no empty label (leading/trailing/double dot), no
    // leading/trailing hyphen per label, ≤63 chars per label.
    for label in h.split('.') {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            bail!("--public-base-url hostname has an invalid DNS label {label:?} (got {url:?})");
        }
    }
    Ok(())
}

pub(crate) fn validate_config(cfg: &OfficialPreviewConfig) -> Result<()> {
    validate_public_base_url(&cfg.public_base_url)?;
    if !(1..=64).contains(&cfg.max_slots) {
        bail!("--max-slots must be in [1, 64] (got {})", cfg.max_slots);
    }
    if !cfg.caddyfile_path.starts_with('/') || cfg.caddyfile_path.contains("..") {
        bail!(
            "--caddyfile must be a plain absolute path (got {:?})",
            cfg.caddyfile_path
        );
    }
    Ok(())
}

/// `(hostname, loopback port)` per slot: `s0.<base>` → 8420, `s1.<base>` → 8421…
pub(crate) fn slot_hostnames(base_host: &str, max_slots: usize) -> Vec<(String, u16)> {
    (0..max_slots)
        .map(|i| (format!("s{i}.{base_host}"), LOCAL_BASE_PORT + i as u16))
        .collect()
}

/// Render the Caddyfile for this runner's hostname family. Format-compatible
/// with the ato-admin console's generator (the console shows the same file):
/// every vhost answers the well-known probe 200 REGARDLESS of slot app state,
/// then reverse-proxies to its loopback slot port. The base hostname serves
/// the runner root proxy (slot 0's port).
pub(crate) fn render_caddyfile(base_host: &str, max_slots: usize) -> String {
    let vhost = |hostname: &str, port: u16| {
        format!(
            "{hostname} {{\n\
             \thandle {WELLKNOWN_PATH} {{\n\
             \t\trespond \"ok\" 200\n\
             \t}}\n\
             \treverse_proxy 127.0.0.1:{port}\n\
             }}\n"
        )
    };
    let mut blocks = vec![format!(
        "# ato official-preview runner ingress — generated by `ato runner setup --official-preview`.\n\
         # Do not hand-edit; re-run setup after changing max_slots.\n"
    )];
    blocks.push(vhost(base_host, LOCAL_BASE_PORT));
    for (hostname, port) in slot_hostnames(base_host, max_slots) {
        blocks.push(vhost(&hostname, port));
    }
    blocks.join("\n")
}

/// The env-file lines official-preview needs, minus keys `existing` already
/// defines (append-only — an operator-set line is NEVER rewritten). Returns
/// `(missing_lines, conflicts)`; a conflict is an existing key whose value
/// disagrees with what this mode needs, surfaced as an explicit warning
/// because appending cannot fix it.
pub(crate) fn official_env_lines(
    existing: &BTreeMap<String, String>,
    cfg: &OfficialPreviewConfig,
) -> (Vec<String>, Vec<String>) {
    let base_host = base_hostname(&cfg.public_base_url);
    let wanted: Vec<(&str, String)> = vec![
        ("ATO_RUNNER_PREVIEW", "1".to_string()),
        (
            "ATO_RUNNER_PUBLIC_BASE_URL",
            cfg.public_base_url.trim_end_matches('/').to_string(),
        ),
        ("ATO_RUNNER_MAX_SLOTS", cfg.max_slots.to_string()),
        // Per-slot READY URLs. Without this, serve (a) refuses to start at
        // max_slots >= 2 (validate_multi_slot_public_url requires a template)
        // and (b) at max_slots = 1 reports slot 0 ready at the BASE host —
        // which the control plane's slot-hostname allowlist (s0.<base>…)
        // rejects at /ready. The template renders exactly the s{N}.<base>
        // hostnames the admin console registered.
        (
            "ATO_RUNNER_PUBLIC_URL_TEMPLATE",
            format!("https://s{{slot}}.{base_host}"),
        ),
    ];
    let mut lines = Vec::new();
    let mut conflicts = Vec::new();
    for (k, v) in wanted {
        match existing.get(k) {
            None => lines.push(format!("{k}={v}")),
            Some(cur) if cur.trim_end_matches('/') != v.trim_end_matches('/') => {
                conflicts.push(format!(
                    "{k} is already set to {cur:?} (this mode needs {v:?}) — {ENV_FILE} is append-only; edit the line manually",
                    ENV_FILE = super::ENV_FILE,
                ));
            }
            Some(_) => {}
        }
    }
    (lines, conflicts)
}

/// True when a systemd unit's text binds the runner proxy publicly — an
/// ExecStart passing `--proxy-listen` with a host other than 127.0.0.1 /
/// localhost. A publicly bound slot port bypasses Caddy, TLS, and the
/// control plane's slot-hostname allowlist. Pure.
pub(crate) fn unit_has_public_proxy_listen(unit_text: &str) -> bool {
    for line in unit_text.lines() {
        let Some(idx) = line.find("--proxy-listen") else {
            continue;
        };
        let rest = line[idx + "--proxy-listen".len()..].trim_start_matches(['=', ' ']);
        let value = rest.split_whitespace().next().unwrap_or("");
        let host = value.rsplit_once(':').map(|(h, _)| h).unwrap_or(value);
        let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
        if !(h.is_empty() || h == "127.0.0.1" || h == "localhost" || h == "::1") {
            return true;
        }
    }
    false
}

/// Parse `/proc/net/tcp`-format text for LISTEN (st == 0A) sockets on `port`.
/// Pure — the probe reads /proc/net/tcp{,6} and feeds the text here.
pub(crate) fn proc_net_tcp_listens_on(text: &str, port: u16) -> bool {
    text.lines().skip(1).any(|line| {
        // Columns: sl local_address rem_address st …
        let mut cols = line.split_whitespace();
        let local = cols.nth(1).unwrap_or("");
        let st = cols.nth(1).unwrap_or("");
        st.eq_ignore_ascii_case("0A")
            && local
                .rsplit_once(':')
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                == Some(port)
    })
}

fn host_listens_on(port: u16) -> bool {
    ["/proc/net/tcp", "/proc/net/tcp6"].iter().any(|path| {
        std::fs::read_to_string(path)
            .map(|t| proc_net_tcp_listens_on(&t, port))
            .unwrap_or(false)
    })
}

fn caddy_service_state() -> Option<String> {
    let out = std::process::Command::new("systemctl")
        .args(["is-active", CADDY_UNIT])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Official-preview host checks, appended to the base `gather()` set. Check
/// ids here drive the extra plan actions in setup.rs.
pub(crate) fn gather(cfg: &OfficialPreviewConfig) -> Vec<Check> {
    let mut out = Vec::new();
    let base_host = base_hostname(&cfg.public_base_url);

    // Caddy binary.
    out.push(
        match std::process::Command::new("caddy").arg("version").output() {
            Ok(o) if o.status.success() => Check::ok(
                "caddy",
                "Caddy",
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            ),
            _ => Check::missing(
                "caddy",
                "Caddy",
                "not installed",
                "apt-get install -y caddy",
            ),
        },
    );

    // Caddyfile content: ours iff it names the base hostname (regenerating is
    // idempotent; a foreign file is backed up before being replaced).
    out.push(match std::fs::read_to_string(&cfg.caddyfile_path) {
        Ok(text) if text.contains(&base_host) && text.contains(WELLKNOWN_PATH) => Check::ok(
            "caddyfile",
            "Caddyfile",
            format!("{} serves {base_host}", cfg.caddyfile_path),
        ),
        Ok(_) => Check::missing(
            "caddyfile",
            "Caddyfile",
            format!(
                "{} exists but does not serve {base_host}",
                cfg.caddyfile_path
            ),
            "setup --fix regenerates it (existing file backed up)",
        ),
        Err(_) => Check::missing(
            "caddyfile",
            "Caddyfile",
            format!("{} absent", cfg.caddyfile_path),
            "setup --fix writes it",
        ),
    });

    // Caddy service state.
    out.push(match caddy_service_state().as_deref() {
        Some("active") => Check::ok("caddy_service", "Caddy service", "active"),
        Some(state) => Check::missing(
            "caddy_service",
            "Caddy service",
            format!(
                "installed, {}",
                if state.is_empty() { "inactive" } else { state }
            ),
            "systemctl enable --now caddy",
        ),
        None => Check::missing(
            "caddy_service",
            "Caddy service",
            "systemctl unavailable or caddy not installed",
            "install caddy, then systemctl enable --now caddy",
        ),
    });

    // Ports 80/443: Caddy needs both (443 TLS + 80 for the ACME HTTP challenge
    // and the https redirect). Occupied-by-caddy is the healthy end state.
    let caddy_active = caddy_service_state().as_deref() == Some("active");
    for port in [80u16, 443] {
        let id: &'static str = if port == 80 { "port_80" } else { "port_443" };
        out.push(if host_listens_on(port) {
            if caddy_active {
                Check::ok(
                    id,
                    "ingress port",
                    format!("{port} in LISTEN (caddy active)"),
                )
            } else {
                Check::warn(
                    id,
                    "ingress port",
                    format!(
                        "{port} is in LISTEN but caddy is not active — another server owns it?"
                    ),
                    "stop the conflicting service; Caddy must bind 80+443",
                )
            }
        } else {
            Check::ok(
                id,
                "ingress port",
                format!("{port} free (Caddy will bind it)"),
            )
        });
    }

    // Runner unit must not bind the slot proxy publicly.
    let unit_path = Path::new(super::SYSTEMD_DIR).join(super::RUNNER_UNIT);
    if let Ok(text) = std::fs::read_to_string(&unit_path) {
        out.push(if unit_has_public_proxy_listen(&text) {
            Check::missing(
                "unit_runner_loopback",
                "runner proxy bind",
                format!(
                    "{} passes a PUBLIC --proxy-listen — slot ports must stay on 127.0.0.1 (a public bind bypasses Caddy/TLS and the slot-hostname allowlist)",
                    super::RUNNER_UNIT
                ),
                "setup --fix rewrites the unit (loopback default)",
            )
        } else {
            Check::ok("unit_runner_loopback", "runner proxy bind", "loopback only")
        });
    }

    // Official env keys (append-only fix; conflicts surface separately).
    let env_vals = std::fs::read_to_string(super::ENV_FILE)
        .map(|t| checks::env_file_values(&t))
        .unwrap_or_default();
    let (missing, _conflicts) = official_env_lines(&env_vals, cfg);
    out.push(if missing.is_empty() {
        Check::ok(
            "env_official",
            "official-preview env",
            "ATO_RUNNER_PREVIEW / PUBLIC_BASE_URL / MAX_SLOTS / PUBLIC_URL_TEMPLATE configured",
        )
    } else {
        Check::missing(
            "env_official",
            "official-preview env",
            format!("missing: {}", missing.join(", ")),
            "setup --fix appends them",
        )
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OfficialPreviewConfig {
        OfficialPreviewConfig {
            public_base_url: "https://runner-abc.runner.ato.run".into(),
            max_slots: 2,
            caddyfile_path: DEFAULT_CADDYFILE_PATH.into(),
        }
    }

    #[test]
    fn local_base_port_matches_the_serve_default() {
        // The Caddyfile upstreams and `ato runner serve`'s default bind must
        // agree, or every slot proxies into the void.
        assert_eq!(
            format!("127.0.0.1:{LOCAL_BASE_PORT}"),
            crate::application::runner_agent::DEFAULT_PROXY_LISTEN
        );
    }

    #[test]
    fn public_base_url_validation() {
        assert!(validate_public_base_url("https://runner-abc.runner.ato.run").is_ok());
        assert!(validate_public_base_url("https://runner-abc.runner.ato.run/").is_ok());
        for bad in [
            "http://runner-abc.runner.ato.run",       // https only
            "https://runner-abc.runner.ato.run:8421", // no port — Caddy owns 443
            "https://runner-abc.runner.ato.run/app",  // no path
            "https://65.109.37.38",                   // raw IP
            "https://127.0.0.1",                      // loopback
            "https://localhost",                      // loopback
            "https://runner",                         // not a dotted DNS name
            "https://runner abc.run",                 // whitespace ⇒ env injection
            "https://a.run\nATO_X=1",                 // newline ⇒ env injection
            "https://user@host.run",                  // userinfo
            "https://-bad.runner.ato.run",            // leading hyphen label
            "https://bad-.runner.ato.run",            // trailing hyphen label
            "https://bad..runner.ato.run",            // empty label
            "https://.runner.ato.run",                // empty leading label
        ] {
            assert!(
                validate_public_base_url(bad).is_err(),
                "must reject {bad:?}"
            );
        }
    }

    #[test]
    fn caddyfile_covers_base_and_every_slot_with_the_wellknown_handler() {
        let text = render_caddyfile("runner-abc.runner.ato.run", 2);
        // base + s0 + s1 vhosts, each with the well-known handler.
        assert_eq!(text.matches(WELLKNOWN_PATH).count(), 3);
        assert!(text.contains("runner-abc.runner.ato.run {"));
        assert!(text.contains("s0.runner-abc.runner.ato.run {"));
        assert!(text.contains("s1.runner-abc.runner.ato.run {"));
        assert!(!text.contains("s2."), "no vhost beyond max_slots");
        // Upstreams are LOOPBACK slot ports only.
        assert!(text.contains("reverse_proxy 127.0.0.1:8420"));
        assert!(text.contains("reverse_proxy 127.0.0.1:8421"));
        assert!(!text.contains("0.0.0.0"));
    }

    #[test]
    fn env_lines_are_append_only_and_conflicts_surface() {
        // Fresh file: all four keys appended — INCLUDING the per-slot URL
        // template, without which serve refuses multi-slot startup and slot 0
        // would report the base host (rejected by the slot allowlist).
        let (lines, conflicts) = official_env_lines(&BTreeMap::new(), &cfg());
        assert_eq!(
            lines,
            vec![
                "ATO_RUNNER_PREVIEW=1",
                "ATO_RUNNER_PUBLIC_BASE_URL=https://runner-abc.runner.ato.run",
                "ATO_RUNNER_MAX_SLOTS=2",
                "ATO_RUNNER_PUBLIC_URL_TEMPLATE=https://s{slot}.runner-abc.runner.ato.run",
            ]
        );
        assert!(conflicts.is_empty());

        // Matching existing values: nothing appended, no conflicts (trailing
        // slash tolerated).
        let mut existing = BTreeMap::new();
        existing.insert("ATO_RUNNER_PREVIEW".into(), "1".into());
        existing.insert(
            "ATO_RUNNER_PUBLIC_BASE_URL".into(),
            "https://runner-abc.runner.ato.run/".into(),
        );
        existing.insert("ATO_RUNNER_MAX_SLOTS".into(), "2".into());
        existing.insert(
            "ATO_RUNNER_PUBLIC_URL_TEMPLATE".into(),
            "https://s{slot}.runner-abc.runner.ato.run".into(),
        );
        let (lines, conflicts) = official_env_lines(&existing, &cfg());
        assert!(lines.is_empty() && conflicts.is_empty());

        // Disagreeing operator values: NOT rewritten, surfaced as conflicts.
        existing.insert(
            "ATO_RUNNER_PUBLIC_BASE_URL".into(),
            "https://old.example.com".into(),
        );
        existing.insert(
            "ATO_RUNNER_PUBLIC_URL_TEMPLATE".into(),
            "https://runner.example.com:{port}/".into(),
        );
        let (lines, conflicts) = official_env_lines(&existing, &cfg());
        assert!(lines.is_empty());
        assert_eq!(conflicts.len(), 2);
        assert!(
            conflicts
                .iter()
                .any(|c| c.contains("ATO_RUNNER_PUBLIC_BASE_URL"))
        );
        assert!(
            conflicts
                .iter()
                .any(|c| c.contains("ATO_RUNNER_PUBLIC_URL_TEMPLATE"))
        );
    }

    #[test]
    fn template_satisfies_serve_startup_validation_at_multi_slot() {
        // The exact template setup writes must pass BOTH serve-side gates, or
        // the systemd service crash-loops at max_slots >= 2.
        let (lines, _) = official_env_lines(&BTreeMap::new(), &cfg());
        let template = lines
            .iter()
            .find_map(|l| l.strip_prefix("ATO_RUNNER_PUBLIC_URL_TEMPLATE="))
            .expect("template line present");
        crate::application::runner_agent::validate_public_url_template(Some(template))
            .expect("template must carry a {slot}/{port} placeholder");
        crate::application::runner_agent::validate_multi_slot_public_url(2, Some(template))
            .expect("multi-slot serve must accept the official-preview env");
    }

    #[test]
    fn template_renders_slot_hostnames_never_the_base_host() {
        let (lines, _) = official_env_lines(&BTreeMap::new(), &cfg());
        let template = lines
            .iter()
            .find_map(|l| l.strip_prefix("ATO_RUNNER_PUBLIC_URL_TEMPLATE="))
            .unwrap();
        // Slot i serves on LOCAL_BASE_PORT + i; the rendered ready URL must be
        // the s{N} slot hostname the control-plane allowlist registered.
        let s0 = crate::application::runner_agent::render_public_url_template(
            template,
            LOCAL_BASE_PORT,
            0,
        );
        let s1 = crate::application::runner_agent::render_public_url_template(
            template,
            LOCAL_BASE_PORT + 1,
            1,
        );
        assert_eq!(s0, "https://s0.runner-abc.runner.ato.run");
        assert_eq!(s1, "https://s1.runner-abc.runner.ato.run");
        // Never the bare base host (the allowlist only carries slot hostnames).
        for url in [&s0, &s1] {
            assert!(
                !url.starts_with("https://runner-abc."),
                "must not render the base host: {url}"
            );
        }
    }

    #[test]
    fn public_proxy_listen_detection() {
        // The unit setup writes (no flags) is loopback-safe.
        assert!(!unit_has_public_proxy_listen(
            &super::super::setup::render_unit(super::super::RUNNER_UNIT)
        ));
        for public in [
            "ExecStart=/usr/local/bin/ato runner serve --proxy-listen 0.0.0.0:8420",
            "ExecStart=/usr/local/bin/ato runner serve --proxy-listen=10.0.0.5:8420",
            "ExecStart=ato runner serve --proxy-listen [::]:8420",
        ] {
            assert!(unit_has_public_proxy_listen(public), "must flag {public:?}");
        }
        for loopback in [
            "ExecStart=/usr/local/bin/ato runner serve",
            "ExecStart=ato runner serve --proxy-listen 127.0.0.1:8420",
            "ExecStart=ato runner serve --proxy-listen=localhost:9000",
        ] {
            assert!(
                !unit_has_public_proxy_listen(loopback),
                "must pass {loopback:?}"
            );
        }
    }

    #[test]
    fn proc_net_tcp_listen_parse() {
        // 0050 = 80, 01BB = 443; st 0A = LISTEN.
        let tcp = "  sl  local_address rem_address   st\n\
                   0: 00000000:0050 00000000:0000 0A\n\
                   1: 0100007F:20E4 00000000:0000 0A\n\
                   2: 00000000:01BB 00000000:0000 01\n";
        assert!(proc_net_tcp_listens_on(tcp, 80));
        assert!(proc_net_tcp_listens_on(tcp, 8420)); // 0x20E4
        assert!(
            !proc_net_tcp_listens_on(tcp, 443),
            "st 01 (ESTABLISHED) is not LISTEN"
        );
        assert!(!proc_net_tcp_listens_on(tcp, 9999));
    }

    #[test]
    fn config_validation_bounds() {
        assert!(validate_config(&cfg()).is_ok());
        let mut c = cfg();
        c.max_slots = 0;
        assert!(validate_config(&c).is_err());
        c.max_slots = 65;
        assert!(validate_config(&c).is_err());
        c.max_slots = 1;
        c.caddyfile_path = "relative/Caddyfile".into();
        assert!(validate_config(&c).is_err());
        c.caddyfile_path = "/etc/caddy/../evil".into();
        assert!(validate_config(&c).is_err());
    }
}
