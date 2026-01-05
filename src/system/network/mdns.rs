use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;

pub struct MdnsAnnouncer {
    daemon: ServiceDaemon,
    registered_services: Arc<Mutex<HashMap<String, ServiceInfo>>>,
}

impl MdnsAnnouncer {
    pub fn new() -> Result<Self> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| anyhow::anyhow!("Failed to create mDNS daemon: {}", e))?;
        Ok(Self {
            daemon,
            registered_services: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn register(&self, hostname: &str, port: u16) -> Result<()> {
        let service_type = "_http._tcp.local.";
        let instance_name = hostname;
        let host_ipv4 = "127.0.0.1"; // We are advertising local services
        let host_name = format!("{}.local.", hostname);
        let properties = [("path", "/")];

        let my_service = ServiceInfo::new(
            service_type,
            instance_name,
            &host_name,
            host_ipv4,
            port,
            &properties[..],
        )
        .map_err(|e| anyhow::anyhow!("Invalid service info: {}", e))?;

        info!(
            "mDNS: Attempting to register service type '{}' instance '{}' host '{}' port {}",
            service_type, instance_name, host_name, port
        );

        self.daemon
            .register(my_service.clone())
            .map_err(|e| anyhow::anyhow!("Failed to register mDNS service: {}", e))?;

        let mut services = self.registered_services.lock().unwrap();
        services.insert(hostname.to_string(), my_service);

        info!(
            "mDNS: Successfully registered {}.local on port {}",
            hostname, port
        );
        Ok(())
    }

    pub fn unregister(&self, hostname: &str) -> Result<()> {
        let mut services = self.registered_services.lock().unwrap();
        if let Some(service_info) = services.remove(hostname) {
            self.daemon
                .unregister(service_info.get_fullname())
                .map_err(|e| anyhow::anyhow!("Failed to unregister mDNS service: {}", e))?;
            info!("mDNS: Unregistered {}.local", hostname);
        }
        Ok(())
    }
}
