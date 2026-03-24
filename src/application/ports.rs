pub(crate) mod install;
pub(crate) mod output;
pub(crate) mod publish;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

#[allow(unused_imports)]
pub use output::{OutputPort, SharedOutputPort};

#[allow(dead_code)]
pub trait InteractionPort: Send + Sync {
    fn confirm(&self, prompt: &str, default: bool) -> Result<bool>;
    fn render_manifest_preview(&self, manifest_path: &Path, preview_toml: &str) -> Result<()>;
    fn open_editor(&self, path: &Path) -> Result<()>;
}

#[allow(dead_code)]
pub type SharedInteractionPort = Arc<dyn InteractionPort>;
