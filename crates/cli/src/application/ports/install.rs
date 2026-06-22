use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    LocalArtifact { path: PathBuf },
    RegistryOrCasArtifact { path: PathBuf },
}

impl SourceSpec {
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        match self {
            Self::LocalArtifact { path } | Self::RegistryOrCasArtifact { path } => path.as_path(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceArtifact {
    pub bytes: Vec<u8>,
    pub source: SourceSpec,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSpec {
    ExecutionSandbox { root_dir: PathBuf },
    TestSandbox { root_dir: PathBuf },
}

impl TargetSpec {
    #[allow(dead_code)]
    pub fn root_dir(&self) -> &Path {
        match self {
            Self::ExecutionSandbox { root_dir } | Self::TestSandbox { root_dir } => {
                root_dir.as_path()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledEnvironment {
    pub root_dir: PathBuf,
    pub source: SourceSpec,
}

#[async_trait]
pub trait SourcePort: Send + Sync {
    async fn fetch(&self, spec: &SourceSpec) -> Result<SourceArtifact>;
}

#[async_trait]
pub trait TargetPort: Send + Sync {
    async fn unpack(
        &self,
        artifact: SourceArtifact,
        spec: &TargetSpec,
    ) -> Result<InstalledEnvironment>;
}

pub type SharedSourcePort = Arc<dyn SourcePort>;
pub type SharedTargetPort = Arc<dyn TargetPort>;
