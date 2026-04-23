use crate::error::{CapsuleError, Result};
use rand::Rng;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::reporter::NoOpReporter;
use crate::runtime::native::NativeHandle;
use crate::{RuntimeMetadata, SessionRunner, SessionRunnerConfig};

use crate::engine;
use crate::packers::bundle::{build_bundle, PackBundleArgs};
use crate::router::ManifestData;
use crate::runtime_config;

pub fn execute(plan: &ManifestData, nacelle_override: Option<PathBuf>) -> Result<i32> {
    let nacelle = engine::discover_nacelle(engine::EngineRequest {
        explicit_path: nacelle_override.clone(),
        manifest_path: Some(plan.manifest_path.clone()),
        compat_input: None,
    })?;

    runtime_config::generate_and_write_config(
        &plan.manifest_path,
        Some("best_effort".to_string()),
        false,
    )?;

    let runtime = tokio::runtime::Runtime::new()?;

    let bundle_path = {
        let mut rng = rand::thread_rng();
        let suffix: u64 = rng.gen();
        let output = std::env::temp_dir().join(format!("capsule-dev-{}.bundle", suffix));

        runtime.block_on(build_bundle(
            PackBundleArgs {
                manifest_path: Some(plan.manifest_path.clone()),
                workspace_root: plan.manifest_dir.clone(),
                compat_input: None,
                runtime_path: None,
                output: Some(output),
                nacelle_path: Some(nacelle),
            },
            std::sync::Arc::new(NoOpReporter),
        ))?
    };

    let exit_code = runtime.block_on(run_bundle_with_metrics(&bundle_path, &plan.manifest_dir))?;

    let _ = std::fs::remove_file(&bundle_path);

    Ok(exit_code)
}

async fn run_bundle_with_metrics(bundle_path: &Path, manifest_dir: &Path) -> Result<i32> {
    let child = Command::new(bundle_path)
        .current_dir(manifest_dir)
        .spawn()
        .map_err(|e| CapsuleError::ProcessStart(format!("Failed to execute bundle {}: {e}", bundle_path.display())))?;

    let pid = child.id();
    drop(child);

    let session_id = format!("dev-{}", rand::thread_rng().gen::<u64>());
    let handle = NativeHandle::new(session_id, pid);
    let reporter = NoOpReporter;
    let config = SessionRunnerConfig::default();

    let metrics = SessionRunner::new(handle, reporter)
        .with_config(config)
        .run()
        .await?;

    Ok(extract_exit_code(&metrics))
}

fn extract_exit_code(metrics: &crate::UnifiedMetrics) -> i32 {
    match &metrics.metadata {
        RuntimeMetadata::Nacelle { exit_code, .. } => (*exit_code).unwrap_or(1),
        _ => 1,
    }
}
