use std::sync::Arc;

use async_trait::async_trait;
use capsule_core::{CapsuleReporter, UsageReporter};

#[async_trait]
pub trait OutputPort: CapsuleReporter + UsageReporter + Send + Sync {
    fn is_json(&self) -> bool;
}

#[allow(dead_code)]
pub type SharedOutputPort = Arc<dyn OutputPort>;
