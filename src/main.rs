//! Nacelle Engine - Main Entry Point
//!
//! v2.0: Bundle Runtime Model
//! - Self-extracting bundle (embedded runtime)
//! - Direct execution with supervisor and sandbox

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // v2.0: Check if running as self-extracting bundle
    if is_self_extracting_bundle()? {
        return bootstrap_bundled_runtime().await;
    }

    // If not a bundle, show help message
    eprintln!("🔴 Nacelle v2.0: Not running as a bundle");
    eprintln!("This binary should be executed as a self-extracting bundle.");
    eprintln!("Use 'nacelle pack --bundle' to create executable bundles.");
    std::process::exit(1);
}

/// v2.0: Check if this binary contains an embedded bundle
fn is_self_extracting_bundle() -> anyhow::Result<bool> {
    let exe_path = std::env::current_exe()?;
    nacelle::bundle::is_self_extracting_bundle(&exe_path)
}

/// v2.0: Bootstrap and run embedded runtime
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

    let mut enforcer: Option<nacelle::engine::ebpf_enforcer::EgressEnforcerHandle> = None;
    let dns_rules = if config.sandbox.network.allow_domains.is_some() {
        nacelle::egress::dns_bootstrap::dns_bootstrap_rules()?
    } else {
        Vec::new()
    };

    if egress_mode != "allow_all" {
        let job_id = format!("job-{}", std::process::id());
        match nacelle::engine::ebpf_enforcer::start_enforcer(&[], &dns_rules, &job_id) {
            Ok(handle) => {
                enforcer = Some(handle);
            }
            Err(e) => {
                eprintln!("⚠️  eBPF enforcer unavailable: {}", e);
                if matches!(
                    nacelle::engine::enforcement_guard::EnforcementMode::parse_mode(
                        config.sandbox.network.enforcement.as_str()
                    ),
                    nacelle::engine::enforcement_guard::EnforcementMode::Strict
                ) {
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
    if egress_mode != "allow_all" {
        if let Some(handle) = enforcer.as_mut() {
            if !allow_rules.is_empty() || egress_mode == "deny_all" {
                handle.update_allowlist(&allow_rules)?;
            }
        }

        if !dns_rules.is_empty() {
            // Keep DNS rules alongside the final allowlist so domain lookups still work.
            let job_id = format!("job-{}", std::process::id());
            if let Ok(handle) =
                nacelle::engine::ebpf_enforcer::start_enforcer(&allow_rules, &dns_rules, &job_id)
            {
                enforcer = Some(handle);
            }
        }
    }
    nacelle::engine::startup_gc::cleanup_orphan_cgroups();
    let enforcement_mode = nacelle::engine::enforcement_guard::EnforcementMode::parse_mode(
        config.sandbox.network.enforcement.as_str(),
    );
    nacelle::engine::enforcement_guard::check_enforcement(enforcement_mode)
        .map_err(|e| anyhow::anyhow!(e))?;
    let cgroup_path = enforcer.as_ref().map(|h| h.cgroup_path.as_path());
    nacelle::engine::r3_supervisor::run_services_from_config(&config, &temp_dir, cgroup_path)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}
