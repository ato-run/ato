use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::application::ports::install::{SourceArtifact, SourcePort, SourceSpec};

#[derive(Debug, Default)]
pub(crate) struct LocalArtifactSource;

#[async_trait]
impl SourcePort for LocalArtifactSource {
    async fn fetch(&self, spec: &SourceSpec) -> Result<SourceArtifact> {
        let SourceSpec::LocalArtifact { path } = spec else {
            anyhow::bail!("local artifact source requires SourceSpec::LocalArtifact")
        };

        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read local artifact {}", path.display()))?;
        Ok(SourceArtifact {
            bytes,
            source: spec.clone(),
        })
    }
}
