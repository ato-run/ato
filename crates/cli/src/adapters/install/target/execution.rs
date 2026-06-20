use anyhow::Result;
use async_trait::async_trait;

use crate::adapters::install::target::unpack_capsule_into_directory;
use crate::application::ports::install::{
    InstalledEnvironment, SourceArtifact, TargetPort, TargetSpec,
};

#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct ExecutionSandboxTarget;

#[async_trait]
impl TargetPort for ExecutionSandboxTarget {
    async fn unpack(
        &self,
        artifact: SourceArtifact,
        spec: &TargetSpec,
    ) -> Result<InstalledEnvironment> {
        let TargetSpec::ExecutionSandbox { root_dir } = spec else {
            anyhow::bail!("execution sandbox target requires TargetSpec::ExecutionSandbox")
        };

        unpack_capsule_into_directory(&artifact.bytes, root_dir)?;
        Ok(InstalledEnvironment {
            root_dir: root_dir.clone(),
            source: artifact.source,
        })
    }
}
