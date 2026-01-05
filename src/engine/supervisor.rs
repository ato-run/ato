use std::process::Child;
use std::sync::{Arc, Mutex};
use tracing::info;

#[derive(Clone, Debug)]
pub struct ProcessSupervisor {
    children: Arc<Mutex<Vec<Child>>>,
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSupervisor {
    pub fn new() -> Self {
        Self {
            children: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn register(&self, child: Child) {
        if let Ok(mut children) = self.children.lock() {
            children.push(child);
        }
    }
}

impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        // Only the last reference holder should kill processes, but Arc handles memory, not Drop logic for inner content.
        // However, since we want to kill processes when the *supervisor* is dropped (which usually happens at main exit),
        // we need to be careful.
        // Actually, Arc doesn't call Drop on inner T when cloned.
        // But here ProcessSupervisor holds the Arc.
        // We want to kill processes when the application exits.
        // A better approach for a global supervisor is to have a dedicated cleanup function or rely on the fact that
        // when the main thread exits, we want to clean up.

        // In this implementation, we'll rely on the fact that we likely have one main supervisor instance
        // or we can check strong_count if we want to be precise, but explicit cleanup is safer.
        // For now, let's implement a `cleanup` method and also try in Drop if it's the last reference.

        if Arc::strong_count(&self.children) == 1 {
            if let Ok(mut children) = self.children.lock() {
                if !children.is_empty() {
                    info!(
                        "ProcessSupervisor: Cleaning up {} child processes...",
                        children.len()
                    );
                    for child in children.iter_mut() {
                        let _ = child.kill();
                        let _ = child.wait(); // Prevent zombies
                    }
                    children.clear();
                }
            }
        }
    }
}
