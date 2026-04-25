use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;

use crate::adapters::install::target::unpack_capsule_into_directory;
use crate::application::ports::install::{
    InstalledEnvironment, SourceArtifact, TargetPort, TargetSpec,
};

#[derive(Debug, Default)]
pub(crate) struct TestSandboxTarget;

#[async_trait]
impl TargetPort for TestSandboxTarget {
    async fn unpack(
        &self,
        artifact: SourceArtifact,
        spec: &TargetSpec,
    ) -> Result<InstalledEnvironment> {
        let TargetSpec::TestSandbox { root_dir } = spec else {
            anyhow::bail!("test sandbox target requires TargetSpec::TestSandbox")
        };

        unpack_capsule_into_directory(&artifact.bytes, root_dir)?;
        Ok(InstalledEnvironment {
            root_dir: root_dir.clone(),
            source: artifact.source,
        })
    }
}

pub(crate) fn publish_test_sandbox_spec(
    base_dir: &Path,
    scoped_id: &str,
    version: &str,
) -> TargetSpec {
    let run_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let scoped = scoped_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let root_dir = base_dir
        .join(".ato")
        .join("publish")
        .join("install")
        .join(scoped)
        .join(version)
        .join(format!("sandbox-{}", run_id));
    TargetSpec::TestSandbox { root_dir }
}

#[allow(dead_code)]
pub(crate) fn test_sandbox_root(spec: &TargetSpec) -> PathBuf {
    match spec {
        TargetSpec::TestSandbox { root_dir } => root_dir.clone(),
        _ => panic!("test_sandbox_root requires TargetSpec::TestSandbox"),
    }
}
