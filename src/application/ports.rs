pub(crate) mod install;
pub(crate) mod publish;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use capsule_core::{CapsuleReporter, UsageReporter};

#[async_trait]
pub trait OutputPort: CapsuleReporter + UsageReporter + Send + Sync {
    fn is_json(&self) -> bool;
}

#[allow(dead_code)]
pub type SharedOutputPort = Arc<dyn OutputPort>;

#[allow(dead_code)]
pub trait InteractionPort: Send + Sync {
    fn confirm(&self, prompt: &str, default: bool) -> Result<bool>;
    fn render_manifest_preview(&self, manifest_path: &Path, preview_toml: &str) -> Result<()>;
    fn open_editor(&self, path: &Path) -> Result<()>;
}
