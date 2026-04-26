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

    if std::env::args_os().len() <= 1 {
        eprintln!("nacelle: missing operand");
        eprintln!("Try 'nacelle --help' for more information.");
        eprintln!("(Note: You should probably be using 'capsule' instead)");
        std::process::exit(2);
    }

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

    let keep_extracted = std::env::var_os("NACELLE_BUNDLE_KEEP_EXTRACTED").is_some();
    let extraction_dir = nacelle::bundle::prepare_extraction_dir(keep_extracted)?;
    let temp_dir_path = extraction_dir.path().to_path_buf();

    println!("📦 Extracting to {:?}...", temp_dir_path);
    if extraction_dir.preserved() {
        eprintln!(
            "Preserving extracted bundle contents at {}",
            temp_dir_path.display()
        );
    }

    let exe_path = std::env::current_exe()?;
    nacelle::bundle::extract_bundle_to_dir(&exe_path, &temp_dir_path)?;

    let config_path = temp_dir_path.join("config.json");
    if !config_path.exists() {
        anyhow::bail!("No config.json found in bundle (R3 requires config.json)");
    }

    let mut config = nacelle::config::load_config(&config_path)?;

    if let Some(main_svc) = config.services.get_mut("main") {
        if main_svc.cwd == Some("source".to_string()) && !temp_dir_path.join("source").is_dir() {
            main_svc.cwd = Some(".".to_string());
        }
    }

    let network_enabled = config.sandbox.network.enabled;
    let egress_mode = config
        .sandbox
        .network
        .egress
        .as_ref()
        .map(|e| e.mode.as_str())
        .unwrap_or("allow_all");

    let dns_rules = Vec::new();
    let enforcement_str = config.sandbox.network.enforcement.as_str();
    let strict_enforcement = enforcement_str == "strict";
    if strict_enforcement && !network_enabled {
        anyhow::bail!("Strict sandbox enforcement requires sandbox.network.enabled=true");
    }
    let job_id = format!("job-{}", std::process::id());
    let mut sandbox = nacelle::system::new_network_sandbox();
    let mut sandbox_enabled = false;

    if network_enabled && (egress_mode != "allow_all" || strict_enforcement) {
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
                    #[cfg(any(target_os = "macos", target_os = "windows"))]
                    {
                        eprintln!(
                            "Hint: This OS may not support strict sandbox yet. If you trust this code, retry with '--unsafe-bypass-sandbox' from ato-cli."
                        );
                    }
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
    if !allow_rules.is_empty() {
        nacelle::config::validate_egress_rules(&allow_rules)?;
    }
    if network_enabled && egress_mode != "allow_all" && sandbox_enabled {
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
    if network_enabled && sandbox_enabled {
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
    nacelle::manager::r3_supervisor::run_services_from_config(
        &config,
        &temp_dir_path,
        sandbox_ref,
        strict_enforcement,
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}
