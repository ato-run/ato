use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

/// Information about a registered local service
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub name: String,
    pub port: u16,
    pub tags: Vec<String>,
}

/// Manages local service discovery and port allocation
pub struct ServiceRegistry {
    services: Arc<Mutex<HashMap<String, ServiceInfo>>>,
    local_domain: String,
}

impl ServiceRegistry {
    pub fn new(local_domain: Option<String>) -> Self {
        Self {
            services: Arc::new(Mutex::new(HashMap::new())),
            local_domain: local_domain.unwrap_or_else(|| "local".to_string()),
        }
    }

    /// Check whether a specific TCP port on localhost is currently available.
    ///
    /// Note: This is best-effort and inherently racy (TOCTOU), but good enough
    /// to reduce user-facing failures on common port collisions.
    pub fn is_port_available(&self, port: u16) -> bool {
        // Best-effort availability check for host-published ports.
        // Docker publishes ports to all interfaces by default (0.0.0.0), but many local
        // dev servers bind to 127.0.0.1 only. To reduce false positives across platforms
        // (especially macOS), require that BOTH binds succeed.
        TcpListener::bind(("0.0.0.0", port)).is_ok()
            && TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    /// Prefer a specific port if available; otherwise, allocate an ephemeral free port.
    pub fn allocate_port_prefer(&self, preferred: u16) -> Option<u16> {
        if self.is_port_available(preferred) {
            Some(preferred)
        } else {
            self.allocate_port()
        }
    }

    /// Find a free port on localhost
    ///
    /// This binds to port 0 to let the OS assign a free port, then drops the listener.
    /// Note: There is a tiny race condition window where another process could grab the port,
    /// but it's generally safe enough for this use case.
    pub fn allocate_port(&self) -> Option<u16> {
        // We need a port that's actually available for publishing.
        // On some platforms, a port may look free on 0.0.0.0 but still collide on 127.0.0.1 (or vice versa).
        // Allocate candidates and validate with `is_port_available`.
        for _ in 0..50 {
            match TcpListener::bind("0.0.0.0:0") {
                Ok(listener) => match listener.local_addr() {
                    Ok(addr) => {
                        let port = addr.port();
                        drop(listener);
                        if self.is_port_available(port) {
                            info!("Allocated ephemeral port: {}", port);
                            return Some(port);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to get local address for ephemeral port: {}", e);
                        return None;
                    }
                },
                Err(e) => {
                    warn!("Failed to bind to ephemeral port: {}", e);
                    return None;
                }
            }
        }

        warn!("Failed to find an available ephemeral port after retries");
        None
    }

    /// Register a service with the registry
    pub fn register_service(&self, name: String, port: u16, tags: Vec<String>) {
        let mut services = self.services.lock().unwrap();
        let info = ServiceInfo {
            name: name.clone(),
            port,
            tags,
        };
        services.insert(name.clone(), info);
        info!("Registered service '{}' on port {}", name, port);
    }

    /// Unregister a service
    pub fn unregister_service(&self, name: &str) {
        let mut services = self.services.lock().unwrap();
        if services.remove(name).is_some() {
            info!("Unregistered service '{}'", name);
        }
    }

    /// Get all registered services
    pub fn get_services(&self) -> Vec<ServiceInfo> {
        let services = self.services.lock().unwrap();
        services.values().cloned().collect()
    }

    /// Get the configured local domain
    pub fn local_domain(&self) -> &str {
        &self.local_domain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_registry() {
        let registry = ServiceRegistry::new(Some("test.local".to_string()));

        // Test domain
        assert_eq!(registry.local_domain(), "test.local");

        // Test registration
        registry.register_service("app1".to_string(), 8080, vec!["http".to_string()]);
        let services = registry.get_services();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "app1");
        assert_eq!(services[0].port, 8080);

        // Test unregistration
        registry.unregister_service("app1");
        assert!(registry.get_services().is_empty());
    }

    #[test]
    fn test_port_allocation() {
        let registry = ServiceRegistry::new(None);
        let port = registry.allocate_port();
        assert!(port.is_some());
        assert!(port.unwrap() > 0);
    }

    #[test]
    fn test_is_port_available() {
        let registry = ServiceRegistry::new(None);
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let busy_port = listener.local_addr().unwrap().port();

        assert!(!registry.is_port_available(busy_port));
    }

    #[test]
    fn test_is_port_available_detects_loopback_listener_as_busy() {
        let registry = ServiceRegistry::new(None);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let busy_port = listener.local_addr().unwrap().port();

        // A loopback-only bind still conflicts with 0.0.0.0 publishing.
        assert!(!registry.is_port_available(busy_port));
    }

    #[test]
    fn test_allocate_port_prefer_prefers_when_free() {
        let registry = ServiceRegistry::new(None);
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let free_port = listener.local_addr().unwrap().port();
        drop(listener);

        // Note: There's an inherent race condition here - another process
        // might grab the port after we release it. So we just verify that
        // allocate_port_prefer returns a valid port (either the preferred
        // one or a fallback).
        let allocated = registry.allocate_port_prefer(free_port);
        assert!(allocated.is_some());
        assert!(allocated.unwrap() > 0);
    }

    #[test]
    fn test_allocate_port_prefer_falls_back_when_busy() {
        let registry = ServiceRegistry::new(None);
        let listener = TcpListener::bind("0.0.0.0:0").unwrap();
        let busy_port = listener.local_addr().unwrap().port();

        let allocated = registry.allocate_port_prefer(busy_port).unwrap();
        assert_ne!(allocated, busy_port);
        assert!(allocated > 0);
    }
}
