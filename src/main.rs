//! Nacelle Engine - Main Entry Point
//!
//! v0.2.0: Bundle Runtime Model
//! - Self-extracting bundle (embedded runtime)
//! - Direct execution with supervisor and sandbox

mod cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // v0.2.0: Check if running as self-extracting bundle
    if is_self_extracting_bundle()? {
        return bootstrap_bundled_runtime().await;
    }

    // If not a bundle, dispatch to CLI
    cli::execute().await
}

/// v0.2.0: Check if this binary contains an embedded bundle
fn is_self_extracting_bundle() -> anyhow::Result<bool> {
    let exe_path = std::env::current_exe()?;
    nacelle::bundle::is_self_extracting_bundle(&exe_path)
}

/// v0.2.0: Bootstrap and run embedded runtime
async fn bootstrap_bundled_runtime() -> anyhow::Result<()> {
    println!("🚀 Starting nacelle bundle...");

    let temp_dir = std::env::temp_dir().join(format!("nacelle-{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir)?;

    println!("📦 Extracting to {:?}...", temp_dir);

    let exe_path = std::env::current_exe()?;
    nacelle::bundle::extract_bundle_to_dir(&exe_path, &temp_dir)?;

    let config_path = temp_dir.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("No config.json found in bundle (R3 requires config.json)");
    }

    let config = nacelle::runtime_config::load_config(&config_path)?;
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
    let enforcement_str = config.sandbox.network.enforcement.as_str();
    let strict_enforcement = enforcement_str == "strict";
    let job_id = format!("job-{}", std::process::id());
    let mut sandbox = nacelle::system::new_network_sandbox();
    let mut sandbox_enabled = false;

    if egress_mode != "allow_all" {
        let initial_rule = nacelle::system::common::IsolationRule {
            allow_rules: Vec::new(),
            dns_rules: dns_rules.clone(),
            job_id: job_id.clone(),
        };
        match sandbox.prepare(initial_rule).await {
            Ok(_) => sandbox_enabled = true,
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
    if egress_mode != "allow_all" && sandbox_enabled {
        let update_rule = nacelle::system::common::IsolationRule {
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
    {
        nacelle::system::linux::cgroup::cleanup_orphan_cgroups();
        let enforcement_mode =
            nacelle::system::linux::enforcement::EnforcementMode::parse_mode(enforcement_str);
        nacelle::system::linux::enforcement::check_enforcement(enforcement_mode)
            .map_err(|e| anyhow::anyhow!(e))?;
    }

    let sandbox_ref = if sandbox_enabled {
        Some(sandbox.as_ref())
    } else {
        None
    };
    nacelle::manager::r3_supervisor::run_services_from_config(&config, &temp_dir, sandbox_ref)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}
