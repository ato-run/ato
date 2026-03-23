use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::application::ports::install::{SourceArtifact, SourcePort, SourceSpec};

#[derive(Debug, Default)]
pub(crate) struct RegistryOrCasSource;

#[async_trait]
impl SourcePort for RegistryOrCasSource {
    async fn fetch(&self, spec: &SourceSpec) -> Result<SourceArtifact> {
        let SourceSpec::RegistryOrCasArtifact { path } = spec else {
            anyhow::bail!("registry/CAS source requires SourceSpec::RegistryOrCasArtifact")
        };

        let bytes = std::fs::read(path).with_context(|| {
            format!(
                "failed to read registry/CAS-resolved artifact {}",
                path.display()
            )
        })?;
        Ok(SourceArtifact {
            bytes,
            source: spec.clone(),
        })
    }
}
