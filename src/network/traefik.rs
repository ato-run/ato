use crate::network::service_registry::ServiceInfo;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub struct TraefikManager {
    process: Arc<Mutex<Option<Child>>>,
    _config_dir: PathBuf,
    routes_file: PathBuf,
}

impl TraefikManager {
    pub fn new(config_dir: &Path) -> Result<Self> {
        let config_dir = config_dir.to_path_buf();
        fs::create_dir_all(&config_dir).context("Failed to create Traefik config directory")?;

        let static_config_path = config_dir.join("static_config.yaml");
        let routes_file = config_dir.join("routes.yaml");

        // Generate static config
        let static_config = r#"
entryPoints:
  web:
    address: ":80"
  websecure:
    address: ":443"

providers:
  file:
    filename: "routes.yaml"
    watch: true

api:
  dashboard: true
  insecure: true
"#;
        fs::write(&static_config_path, static_config)
            .context("Failed to write Traefik static config")?;

        // Initialize empty routes file
        if !routes_file.exists() {
            fs::write(&routes_file, "").context("Failed to initialize routes.yaml")?;
        }

        // Spawn Traefik process
        info!("Starting Traefik with config: {:?}", static_config_path);
        let child = Command::new("traefik")
            .arg(format!("--configFile={}", static_config_path.display()))
            .stdout(Stdio::null()) // Redirect to null to avoid cluttering engine logs
            .stderr(Stdio::piped())
            .spawn();

        let process = match child {
            Ok(c) => {
                info!("Traefik started successfully");
                Some(c)
            }
            Err(e) => {
                warn!("Failed to start Traefik: {}. Is 'traefik' in PATH?", e);
                None
            }
        };

        Ok(Self {
            process: Arc::new(Mutex::new(process)),
            _config_dir: config_dir,
            routes_file,
        })
    }

    pub fn update_routes(&self, services: &[ServiceInfo]) -> Result<()> {
        let config = self.generate_dynamic_config(services);
        let yaml = serde_yaml::to_string(&config).context("Failed to serialize Traefik routes")?;
        fs::write(&self.routes_file, yaml).context("Failed to write routes.yaml")?;
        info!("Updated Traefik routes for {} services", services.len());
        Ok(())
    }

    fn generate_dynamic_config(&self, services: &[ServiceInfo]) -> DynamicConfig {
        let mut routers = HashMap::new();
        let mut traefik_services = HashMap::new();

        for service in services {
            let router_name = format!("{}-router", service.name);
            let service_name = format!("{}-service", service.name);
            let rule = format!("Host(`{}.local`)", service.name);

            routers.insert(
                router_name,
                Router {
                    rule,
                    service: service_name.clone(),
                    entry_points: vec!["web".to_string()],
                },
            );

            traefik_services.insert(
                service_name,
                TraefikService {
                    load_balancer: LoadBalancer {
                        servers: vec![Server {
                            url: format!("http://127.0.0.1:{}", service.port),
                        }],
                    },
                },
            );
        }

        DynamicConfig {
            http: HttpConfig {
                routers,
                services: traefik_services,
            },
        }
    }
}

impl Drop for TraefikManager {
    fn drop(&mut self) {
        if let Ok(mut process_guard) = self.process.lock() {
            if let Some(mut child) = process_guard.take() {
                info!("Stopping Traefik process...");
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct DynamicConfig {
    http: HttpConfig,
}

#[derive(Serialize, Deserialize)]
struct HttpConfig {
    routers: HashMap<String, Router>,
    services: HashMap<String, TraefikService>,
}

#[derive(Serialize, Deserialize)]
struct Router {
    rule: String,
    service: String,
    #[serde(rename = "entryPoints")]
    entry_points: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct TraefikService {
    #[serde(rename = "loadBalancer")]
    load_balancer: LoadBalancer,
}

#[derive(Serialize, Deserialize)]
struct LoadBalancer {
    servers: Vec<Server>,
}

#[derive(Serialize, Deserialize)]
struct Server {
    url: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::service_registry::ServiceInfo;

    #[test]
    fn test_dynamic_config_generation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = TraefikManager::new(temp_dir.path()).unwrap();

        let services = vec![
            ServiceInfo {
                name: "app1".to_string(),
                port: 8080,
                tags: vec![],
            },
            ServiceInfo {
                name: "app2".to_string(),
                port: 9090,
                tags: vec![],
            },
        ];

        let config = manager.generate_dynamic_config(&services);

        assert_eq!(config.http.routers.len(), 2);
        assert_eq!(config.http.services.len(), 2);

        let router1 = config.http.routers.get("app1-router").unwrap();
        assert_eq!(router1.rule, "Host(`app1.local`)");
        assert_eq!(router1.service, "app1-service");

        let service1 = config.http.services.get("app1-service").unwrap();
        assert_eq!(
            service1.load_balancer.servers[0].url,
            "http://127.0.0.1:8080"
        );
    }
}
