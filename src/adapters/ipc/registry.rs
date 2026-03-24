//! IPC Service Registry — in-memory service discovery and lifecycle tracking.
//!
//! The Registry maintains a thread-safe map of running IPC services.
//! ato-cli registers services when they start and removes them on shutdown.
//!
//! ## Thread Safety
//! The registry is wrapped in `Arc<Mutex<...>>` for safe concurrent access
//! from the broker, refcount manager, and CLI status commands.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::types::{IpcServiceInfo, IpcServiceStatus, IpcTransport};

/// Thread-safe IPC Service Registry.
#[derive(Debug, Clone)]
pub struct IpcRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Default)]
struct RegistryInner {
    services: HashMap<String, IpcServiceInfo>,
}

impl IpcRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner::default())),
        }
    }

    /// Register a new IPC service.
    ///
    /// If a service with the same name already exists, it is replaced.
    pub fn register(&self, info: IpcServiceInfo) {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        inner.services.insert(info.name.clone(), info);
    }

    /// Unregister an IPC service by name.
    ///
    /// Returns the removed service info, or `None` if not found.
    pub fn unregister(&self, name: &str) -> Option<IpcServiceInfo> {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        inner.services.remove(name)
    }

    /// Look up a service by name.
    ///
    /// Returns a clone of the service info if found.
    pub fn lookup(&self, name: &str) -> Option<IpcServiceInfo> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner.services.get(name).cloned()
    }

    /// List all registered services.
    #[cfg(test)]
    pub fn list(&self) -> Vec<IpcServiceInfo> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner.services.values().cloned().collect()
    }

    /// Return the number of registered services.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        let inner = self.inner.lock().expect("registry lock poisoned");
        inner.services.len()
    }

    /// Check if the registry is empty.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Update the reference count for a service.
    ///
    /// Returns `true` if the service was found and updated.
    #[cfg(test)]
    pub fn update_ref_count(&self, name: &str, ref_count: u32) -> bool {
        let mut inner = self.inner.lock().expect("registry lock poisoned");
        if let Some(svc) = inner.services.get_mut(name) {
            svc.ref_count = ref_count;
            true
        } else {
            false
        }
    }

    /// Get status snapshot for all services (for CLI display).
    pub fn status_snapshot(&self) -> Vec<IpcServiceStatus> {
        let inner = self.inner.lock().expect("registry lock poisoned");
        let now = Instant::now();

        inner
            .services
            .values()
            .map(|svc| {
                let uptime_secs = svc
                    .started_at
                    .map(|t| now.duration_since(t).as_secs())
                    .unwrap_or(0);

                IpcServiceStatus {
                    name: svc.name.clone(),
                    mode: svc.sharing_mode,
                    ref_count: svc.ref_count,
                    transport: match &svc.endpoint {
                        IpcTransport::Stdio => "stdio".to_string(),
                        IpcTransport::UnixSocket(_) => "unix".to_string(),
                        IpcTransport::Tcp(_) => "tcp".to_string(),
                        IpcTransport::Tsnet(_) => "tsnet".to_string(),
                    },
                    endpoint: svc.endpoint.endpoint_display(),
                    runtime: svc.runtime_kind,
                    uptime_secs,
                    pid: svc.pid,
                }
            })
            .collect()
    }
}

impl Default for IpcRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::types::{IpcRuntimeKind, SharingMode};
    use std::path::PathBuf;

    fn make_service(name: &str) -> IpcServiceInfo {
        IpcServiceInfo {
            name: name.to_string(),
            pid: Some(12345),
            endpoint: IpcTransport::UnixSocket(PathBuf::from(format!(
                "/tmp/capsule-ipc/{}.sock",
                name
            ))),
            capabilities: vec!["greet".to_string()],
            ref_count: 0,
            started_at: Some(Instant::now()),
            runtime_kind: IpcRuntimeKind::Source,
            sharing_mode: SharingMode::Singleton,
        }
    }

    #[test]
    fn test_register_and_lookup() {
        let registry = IpcRegistry::new();
        let svc = make_service("greeter");
        registry.register(svc);

        let found = registry.lookup("greeter");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "greeter");
    }

    #[test]
    fn test_lookup_missing() {
        let registry = IpcRegistry::new();
        assert!(registry.lookup("nonexistent").is_none());
    }

    #[test]
    fn test_unregister() {
        let registry = IpcRegistry::new();
        registry.register(make_service("greeter"));
        assert_eq!(registry.len(), 1);

        let removed = registry.unregister("greeter");
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_unregister_missing() {
        let registry = IpcRegistry::new();
        assert!(registry.unregister("nonexistent").is_none());
    }

    #[test]
    fn test_list() {
        let registry = IpcRegistry::new();
        registry.register(make_service("svc-a"));
        registry.register(make_service("svc-b"));
        registry.register(make_service("svc-c"));

        let list = registry.list();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_register_replaces_existing() {
        let registry = IpcRegistry::new();
        let mut svc = make_service("greeter");
        svc.pid = Some(100);
        registry.register(svc);

        let mut svc2 = make_service("greeter");
        svc2.pid = Some(200);
        registry.register(svc2);

        assert_eq!(registry.len(), 1);
        let found = registry.lookup("greeter").unwrap();
        assert_eq!(found.pid, Some(200));
    }

    #[test]
    fn test_update_ref_count() {
        let registry = IpcRegistry::new();
        registry.register(make_service("greeter"));

        assert!(registry.update_ref_count("greeter", 3));
        let svc = registry.lookup("greeter").unwrap();
        assert_eq!(svc.ref_count, 3);
    }

    #[test]
    fn test_update_ref_count_missing() {
        let registry = IpcRegistry::new();
        assert!(!registry.update_ref_count("nonexistent", 1));
    }

    #[test]
    fn test_status_snapshot() {
        let registry = IpcRegistry::new();
        registry.register(make_service("greeter"));

        let snapshot = registry.status_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "greeter");
        assert_eq!(snapshot[0].transport, "unix");
        assert!(snapshot[0].endpoint.starts_with("unix://"));
    }

    #[test]
    fn test_is_empty() {
        let registry = IpcRegistry::new();
        assert!(registry.is_empty());
        registry.register(make_service("svc"));
        assert!(!registry.is_empty());
    }

    #[test]
    fn test_thread_safety() {
        use std::thread;

        let registry = IpcRegistry::new();
        let r1 = registry.clone();
        let r2 = registry.clone();

        let t1 = thread::spawn(move || {
            for i in 0..100 {
                r1.register(make_service(&format!("svc-{}", i)));
            }
        });

        let t2 = thread::spawn(move || {
            for _ in 0..100 {
                let _ = r2.list();
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        assert_eq!(registry.len(), 100);
    }
}
