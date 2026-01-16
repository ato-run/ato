use anyhow::{Context, Result};
use nacelle::system::common::IsolationRule;
use nacelle::system::new_network_sandbox;
use std::path::PathBuf;
use std::process::Command;

pub struct RunArgs {
    pub config_path: PathBuf,
    pub args: Vec<String>,
}

pub async fn execute(args: RunArgs) -> Result<()> {
    if args.args.is_empty() {
        anyhow::bail!("No command specified. Use -- <cmd> [args...]");
    }

    if !args.config_path.exists() {
        anyhow::bail!(
            "config.json not found: {} (run capsule pack to generate it)",
            args.config_path.display()
        );
    }

    let config = nacelle::runtime_config::load_config(&args.config_path)
        .with_context(|| format!("Failed to load config.json: {}", args.config_path.display()))?;

    let network_enabled = config.sandbox.network.enabled;
    let enforcement = config.sandbox.network.enforcement.as_str();
    let strict_enforcement = enforcement == "strict";
    let egress_mode = config
        .sandbox
        .network
        .egress
        .as_ref()
        .map(|e| e.mode.as_str())
        .unwrap_or("allow_all");

    let dns_rules = if config.sandbox.network.allow_domains.is_some() {
        nacelle::egress::dns_bootstrap::dns_bootstrap_rules()?
    } else {
        Vec::new()
    };

    let job_id = format!("job-{}", std::process::id());
    let mut sandbox = new_network_sandbox();
    let mut sandbox_enabled = false;

    if network_enabled && egress_mode != "allow_all" {
        let initial_rule = IsolationRule {
            allow_rules: Vec::new(),
            dns_rules: dns_rules.clone(),
            job_id: job_id.clone(),
        };

        match sandbox.prepare(initial_rule).await {
            Ok(()) => sandbox_enabled = true,
            Err(e) => {
                eprintln!("⚠️  Sandbox unavailable: {}", e);
                if strict_enforcement {
                    return Err(anyhow::anyhow!(e));
                }
            }
        }
    }

    let mut allow_rules = Vec::new();
    if let Some(egress) = &config.sandbox.network.egress {
        if let Some(rules) = &egress.rules {
            allow_rules.extend(rules.iter().cloned());
        }
    }
    if let Some(domains) = &config.sandbox.network.allow_domains {
        let resolved = nacelle::egress::resolver::resolve_allow_domains(domains)?;
        allow_rules.extend(resolved);
    }
    if !allow_rules.is_empty() {
        nacelle::egress::validate_egress_rules(&allow_rules)?;
    }

    if network_enabled && egress_mode != "allow_all" && sandbox_enabled {
        let update_rule = IsolationRule {
            allow_rules: allow_rules.clone(),
            dns_rules: dns_rules.clone(),
            job_id: job_id.clone(),
        };

        if !allow_rules.is_empty() || egress_mode == "deny_all" {
            if let Err(e) = sandbox.update_rules(update_rule).await {
                eprintln!("⚠️  Sandbox rule update failed: {}", e);
                if strict_enforcement {
                    return Err(anyhow::anyhow!(e));
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    if network_enabled && sandbox_enabled {
        let mode = nacelle::system::linux::enforcement::EnforcementMode::parse_mode(enforcement);
        nacelle::system::linux::enforcement::check_enforcement(mode)
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    let mut cmd = Command::new(&args.args[0]);
    if args.args.len() > 1 {
        cmd.args(&args.args[1..]);
    }

    if sandbox_enabled {
        sandbox
            .apply_to_child(&mut cmd)
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    let status = cmd.status().context("Failed to spawn command")?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
