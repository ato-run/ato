use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::engine;
use crate::error::{CapsuleError, Result};
use crate::lockfile;
use crate::manifest;
use crate::packers::bundle::{build_bundle, PackBundleArgs};
use crate::packers::capsule as capsule_packer;
use crate::r3_config;
use crate::resource::cas::create_cas_client_from_env;
use crate::router::ManifestData;
use crate::validation;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct SourcePackOptions {
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub config_json: Arc<r3_config::ConfigJson>,
    pub config_path: PathBuf,
    pub output: Option<PathBuf>,
    pub runtime: Option<PathBuf>,
    pub skip_l1: bool,
    pub skip_validation: bool,
    pub nacelle_override: Option<PathBuf>,
    pub standalone: bool,
    pub strict_manifest: bool,
    pub timings: bool,
}

#[derive(Debug, Clone)]
pub struct PreparedSourceConfig {
    pub config_json: Arc<r3_config::ConfigJson>,
    pub config_path: PathBuf,
}

pub fn prepare_source_config(
    manifest_path: &Path,
    enforcement: String,
    standalone: bool,
) -> Result<PreparedSourceConfig> {
    let config_json = Arc::new(r3_config::generate_config(
        manifest_path,
        Some(enforcement),
        standalone,
    )?);
    let config_path = r3_config::write_config(manifest_path, config_json.as_ref())?;

    Ok(PreparedSourceConfig {
        config_json,
        config_path,
    })
}

pub fn pack(
    plan: &ManifestData,
    opts: SourcePackOptions,
    reporter: std::sync::Arc<dyn crate::reporter::CapsuleReporter + 'static>,
) -> Result<PathBuf> {
    let rt = tokio::runtime::Runtime::new()?;
    let strict_manifest = opts.strict_manifest || strict_manifest_from_env()?;

    let loaded = manifest::load_manifest(&opts.manifest_path)?;
    let source_digest = loaded
        .model
        .targets
        .as_ref()
        .and_then(|targets| targets.source_digest.as_deref());
    if let Some(digest) = source_digest {
        debug!("Phase 0: checking CAS for source_digest");
        let cas = create_cas_client_from_env()?;
        let exists = rt.block_on(cas.exists(digest))?;
        if !exists {
            if strict_manifest {
                return Err(CapsuleError::StrictManifestFallbackNotAllowed(format!(
                    "CAS blob not found for source_digest {}",
                    digest
                )));
            }
            let message = format!(
                "⚠️  source_digest {} is not available in CAS; falling back to local source packaging",
                digest
            );
            futures::executor::block_on(reporter.warn(message))?;
        }
    } else if strict_manifest {
        return Err(CapsuleError::StrictManifestFallbackNotAllowed(
            "source_digest is missing; strict-manifest forbids fallback to local source packaging"
                .to_string(),
        ));
    }

    if !opts.skip_validation && !opts.skip_l1 {
        debug!("Phase 1: L1 source policy scan");
        let source_dir = opts.manifest_dir.join("source");
        if source_dir.exists() {
            let scan_extensions = &["py", "sh", "js", "ts", "go", "rs"];
            match validation::source_policy::scan_source_directory(&source_dir, scan_extensions) {
                Ok(()) => {
                    debug!("L1 source policy scan passed");
                }
                Err(e) => {
                    futures::executor::block_on(
                        reporter.warn(format!("   ❌ L1 Policy violation: {}", e)),
                    )?;
                    futures::executor::block_on(
                        reporter.warn(
                            "\n💡 Tip: Fix the security issue or use --skip-l1 (not recommended)"
                                .to_string(),
                        ),
                    )?;
                    return Err(CapsuleError::Pack(
                        "L1 Source Policy check failed".to_string(),
                    ));
                }
            }
        } else {
            debug!("No source/ directory found; skipping L1 source scan");
        }
    } else if opts.skip_l1 {
        debug!("L1 source policy scan skipped (--skip-l1)");
    }

    if !opts.skip_validation {
        debug!("Phase 1b: entrypoint validation");
        validate_entrypoint(&opts.manifest_path, &opts.manifest_dir)?;
        debug!("Entrypoint validation passed");
    }

    debug!("Phase 2: using pre-generated R3 config.json");
    let config_reporter = reporter.clone();
    if !opts.config_path.exists() {
        return Err(CapsuleError::Pack(format!(
            "config.json is missing: {}",
            opts.config_path.display()
        )));
    }
    debug!("config.json ready: {}", opts.config_path.display());

    let lockfile_started = Instant::now();
    let lockfile_path = rt.block_on(lockfile::ensure_lockfile(
        &opts.manifest_path,
        &loaded.raw,
        &loaded.raw_text,
        config_reporter,
        opts.timings,
    ))?;
    if opts.timings {
        futures::executor::block_on(reporter.notify(format!(
            "⏱ [timings] source.ensure_lockfile: {} ms",
            lockfile_started.elapsed().as_millis()
        )))?;
    }

    debug!("capsule.lock generated: {}", lockfile_path.display());

    if opts.standalone {
        debug!("Phase 3: building self-extracting bundle");
        let nacelle = engine::discover_nacelle(engine::EngineRequest {
            explicit_path: opts.nacelle_override,
            manifest_path: Some(opts.manifest_path.clone()),
        })?;

        let bundle_started = Instant::now();
        let bundle_path = rt.block_on(build_bundle(
            PackBundleArgs {
                manifest_path: opts.manifest_path.clone(),
                runtime_path: opts.runtime.clone(),
                output: opts.output.clone(),
                nacelle_path: Some(nacelle),
            },
            reporter.clone(),
        ))?;
        if opts.timings {
            futures::executor::block_on(reporter.notify(format!(
                "⏱ [timings] source.build_bundle: {} ms",
                bundle_started.elapsed().as_millis()
            )))?;
        }

        debug!("Self-extracting bundle created: {}", bundle_path.display());
        Ok(bundle_path)
    } else {
        debug!("Phase 3: creating capsule archive");

        let archive_started = Instant::now();
        let artifact_path = rt.block_on(capsule_packer::pack(
            plan,
            capsule_packer::CapsulePackOptions {
                manifest_path: opts.manifest_path.clone(),
                manifest_dir: opts.manifest_dir.clone(),
                output: opts.output.clone(),
                config_json: opts.config_json,
                config_path: opts.config_path,
                lockfile_path,
            },
            reporter.clone(),
        ))?;
        if opts.timings {
            futures::executor::block_on(reporter.notify(format!(
                "⏱ [timings] source.archive_pack: {} ms",
                archive_started.elapsed().as_millis()
            )))?;
        }

        debug!("Capsule archive created: {}", artifact_path.display());
        Ok(artifact_path)
    }
}

fn strict_manifest_from_env() -> Result<bool> {
    let raw = match std::env::var("ATO_STRICT_MANIFEST") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(false),
        Err(err) => {
            return Err(CapsuleError::Config(format!(
                "Failed to read ATO_STRICT_MANIFEST: {}",
                err
            )));
        }
    };

    parse_bool_env("ATO_STRICT_MANIFEST", &raw)
}

fn parse_bool_env(key: &str, raw: &str) -> Result<bool> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        _ => Err(CapsuleError::Config(format!(
            "Invalid {} value '{}'; expected one of 1,true,yes,on,0,false,no,off",
            key, raw
        ))),
    }
}

fn validate_entrypoint(manifest_path: &Path, manifest_dir: &Path) -> Result<()> {
    let manifest = manifest::load_manifest(manifest_path)
        .map_err(|err| CapsuleError::Pack(err.to_string()))?
        .raw;

    let default_target = manifest
        .get("default_target")
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CapsuleError::Pack("default_target is required".to_string()))?;

    let target = manifest
        .get("targets")
        .and_then(|t| t.as_table())
        .and_then(|t| t.get(default_target))
        .and_then(|t| t.as_table())
        .ok_or_else(|| {
            CapsuleError::Pack(format!(
                "default_target '{}' is missing from targets",
                default_target
            ))
        })?;

    let runtime = target
        .get("runtime")
        .and_then(|value| value.as_str())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let run_command = target
        .get("run_command")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    // v0.3 run-command targets are valid without a file entrypoint.
    if runtime == "source" && run_command.is_some() {
        return Ok(());
    }

    let entrypoint = target
        .get("entrypoint")
        .and_then(|e| e.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CapsuleError::Pack("No entrypoint defined in capsule.toml".to_string()))?;

    let clean_entrypoint = entrypoint.trim_start_matches("./");

    if !clean_entrypoint.contains('/') && !clean_entrypoint.contains('\\') {
        if clean_entrypoint.contains(' ') || clean_entrypoint.contains('\t') {
            return Ok(());
        }
        return Ok(());
    }

    let target_manifest_dir = manifest
        .get("targets")
        .and_then(|t| t.as_table())
        .and_then(|t| t.get(default_target))
        .and_then(|target| target.get("working_dir"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| manifest_dir.join(value))
        .unwrap_or_else(|| manifest_dir.to_path_buf());

    let entrypoint_path = target_manifest_dir.join(clean_entrypoint);
    let source_entrypoint_path = target_manifest_dir.join("source").join(clean_entrypoint);

    if !entrypoint_path.exists() && !source_entrypoint_path.exists() {
        return Err(CapsuleError::Pack(format!(
            r#"Entrypoint not found

  The entrypoint defined in capsule.toml does not exist:
    Path: {}

  Checked locations:
    - Project root: {}
    - Source directory: {}

  Please ensure the file exists in the project root or source/ directory,
  or update the 'entrypoint' field in capsule.toml.
"#,
            entrypoint,
            entrypoint_path.display(),
            source_entrypoint_path.display()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::parse_bool_env;
    use super::validate_entrypoint;
    use tempfile::tempdir;

    #[test]
    fn parse_bool_env_accepts_truthy_values() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            let parsed = parse_bool_env("TEST", value).expect("parse env");
            assert!(parsed, "value should be true: {}", value);
        }
    }

    #[test]
    fn validate_entrypoint_allows_v03_run_command_without_entrypoint() {
        let temp = tempdir().expect("tempdir");
        let manifest_path = temp.path().join("capsule.toml");
        fs::write(
            &manifest_path,
            r#"
schema_version = "0.3"
name = "deno-demo"
version = "0.1.0"
type = "app"
runtime = "source/deno"
run = "deno task start"
"#,
        )
        .expect("write manifest");

        validate_entrypoint(&manifest_path, temp.path())
            .expect("run-command manifest should validate");
    }

    #[test]
    fn parse_bool_env_accepts_falsey_values() {
        for value in ["0", "false", "FALSE", "no", "off", ""] {
            let parsed = parse_bool_env("TEST", value).expect("parse env");
            assert!(!parsed, "value should be false: {}", value);
        }
    }

    #[test]
    fn parse_bool_env_rejects_unknown_value() {
        let err = parse_bool_env("TEST", "maybe").expect_err("must reject unknown");
        assert!(err.to_string().contains("Invalid TEST value"));
    }
}
