use serde::Deserialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Deserialize, Default, Clone)]
pub struct FileConfig {
    #[serde(default)]
    pub server: Option<ServerConfig>,
    #[serde(default)]
    pub wasm: Option<WasmConfig>,
    #[serde(default)]
    pub coordinator: Option<CoordinatorConfig>,
    #[serde(default)]
    pub status: Option<StatusConfig>,
    #[serde(default)]
    pub runtime: Option<RuntimeSection>,
    #[serde(default)]
    pub security: Option<SecurityConfig>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub cloud: Option<CloudConfig>,
    #[serde(default)]
    pub models: Option<ModelsConfig>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ServerConfig {
    pub listen_addr: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct WasmConfig {
    pub module_path: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CoordinatorConfig {
    pub grpc_endpoint: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct StatusConfig {
    pub report_interval_secs: Option<u64>,
    #[serde(default)]
    pub taints: Vec<StatusTaint>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct RuntimeSection {
    pub preferred: Option<String>,
    pub binary_path: Option<String>,
    pub bundle_root: Option<String>,
    pub state_root: Option<String>,
    pub log_dir: Option<String>,
    pub hook_retry_attempts: Option<u32>,

    /// Allow insecure development mode for Source runtime.
    /// When false (default), dev_mode requests in manifests are ignored and
    /// source capsules always run in sandboxed mode.
    /// Nacelle Specification V1.1.0 Compliance: Only set to true in development environments.
    /// Can be overridden by CAPSULED_ALLOW_DEV_MODE environment variable.
    #[serde(default)]
    pub allow_insecure_dev_mode: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct SecurityConfig {
    #[serde(default)]
    pub allowed_host_paths: Vec<String>,
    pub audit_log_path: Option<String>,
    pub audit_key_path: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct NetworkConfig {
    pub state_dir: Option<String>,
    pub local_domain: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct CloudConfig {
    pub enabled: bool,
    pub api_endpoint: Option<String>,
    pub api_key: Option<String>,
    pub rclone: Option<RcloneConfig>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct ModelsConfig {
    pub cache_dir: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct RcloneConfig {
    pub config_type: String,
    pub provider: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
pub struct StatusTaint {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub effect: String,
}

impl FileConfig {
    pub fn server_listen_addr(&self) -> Option<&str> {
        self.server
            .as_ref()
            .and_then(|cfg| cfg.listen_addr.as_deref())
    }

    pub fn wasm_module_path(&self) -> Option<&str> {
        self.wasm
            .as_ref()
            .and_then(|cfg| cfg.module_path.as_deref())
    }

    pub fn coordinator_endpoint(&self) -> Option<&str> {
        self.coordinator
            .as_ref()
            .and_then(|cfg| cfg.grpc_endpoint.as_deref())
    }

    pub fn status_interval_secs(&self) -> Option<u64> {
        self.status
            .as_ref()
            .and_then(|cfg| cfg.report_interval_secs)
    }

    pub fn status_taints(&self) -> &[StatusTaint] {
        self.status
            .as_ref()
            .map(|cfg| cfg.taints.as_slice())
            .unwrap_or(&[])
    }

    pub fn runtime(&self) -> Option<&RuntimeSection> {
        self.runtime.as_ref()
    }

    pub fn security_allowed_paths(&self) -> &[String] {
        self.security
            .as_ref()
            .map(|cfg| cfg.allowed_host_paths.as_slice())
            .unwrap_or(&[])
    }

    pub fn security_audit_log_path(&self) -> Option<&str> {
        self.security
            .as_ref()
            .and_then(|cfg| cfg.audit_log_path.as_deref())
    }

    pub fn security_audit_key_path(&self) -> Option<&str> {
        self.security
            .as_ref()
            .and_then(|cfg| cfg.audit_key_path.as_deref())
    }

    pub fn network(&self) -> Option<&NetworkConfig> {
        self.network.as_ref()
    }

    pub fn cloud(&self) -> Option<&CloudConfig> {
        self.cloud.as_ref()
    }

    pub fn models_cache_dir(&self) -> Option<&str> {
        self.models
            .as_ref()
            .and_then(|cfg| cfg.cache_dir.as_deref())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("failed to read config file {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse config file {path:?}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Option<FileConfig>, ConfigError> {
    let path_ref = path.as_ref();
    match fs::read_to_string(path_ref) {
        Ok(contents) => {
            if contents.trim().is_empty() {
                return Ok(None);
            }
            toml::from_str::<FileConfig>(&contents)
                .map(Some)
                .map_err(|source| ConfigError::Parse {
                    path: path_ref.to_path_buf(),
                    source,
                })
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            path: path_ref.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn load_config_returns_none_when_file_missing() {
        let path = std::env::temp_dir().join(format!(
            "capsuled-engine-config-missing-{}",
            unique_suffix()
        ));
        let cfg = load_config(&path).expect("missing file should not be an error");
        assert!(cfg.is_none());
    }

    #[test]
    fn load_config_parses_expected_fields() {
        let path = std::env::temp_dir().join(format!("capsuled-engine-config-{}", unique_suffix()));
        let toml = r#"
            [server]
            listen_addr = "127.0.0.1:6000"

            [wasm]
            module_path = "/tmp/logic.wasm"

            [coordinator]
            grpc_endpoint = "https://coordinator:50052"

            [status]
            report_interval_secs = 45

            [[status.taints]]
            key = "gpu"
            value = "absent"
            effect = "NoSchedule"

            [runtime]
            preferred = "youki"
            binary_path = "/usr/bin/youki"
            bundle_root = "/var/lib/capsuled/bundles"
            state_root = "/run/capsuled"
            log_dir = "/var/log/capsuled"
            hook_retry_attempts = 2
        "#;
        fs::write(&path, toml).expect("should write test config");

        let cfg = load_config(&path)
            .expect("parsing should succeed")
            .expect("config should exist");
        assert_eq!(cfg.server_listen_addr(), Some("127.0.0.1:6000"));
        assert_eq!(cfg.wasm_module_path(), Some("/tmp/logic.wasm"));
        assert_eq!(
            cfg.coordinator_endpoint(),
            Some("https://coordinator:50052")
        );
        assert_eq!(cfg.status_interval_secs(), Some(45));
        assert_eq!(
            cfg.status_taints(),
            &[StatusTaint {
                key: "gpu".into(),
                value: "absent".into(),
                effect: "NoSchedule".into(),
            }]
        );

        let runtime = cfg.runtime().expect("runtime section should be parsed");
        assert_eq!(runtime.preferred.as_deref(), Some("youki"));
        assert_eq!(runtime.binary_path.as_deref(), Some("/usr/bin/youki"));
        assert_eq!(
            runtime.bundle_root.as_deref(),
            Some("/var/lib/capsuled/bundles")
        );
        assert_eq!(runtime.state_root.as_deref(), Some("/run/capsuled"));
        assert_eq!(runtime.log_dir.as_deref(), Some("/var/log/capsuled"));
        assert_eq!(runtime.hook_retry_attempts, Some(2));

        fs::remove_file(&path).ok();
    }

    #[test]
    fn load_config_reports_parse_error() {
        let path = std::env::temp_dir().join(format!(
            "capsuled-engine-config-invalid-{}",
            unique_suffix()
        ));
        fs::write(&path, "not = valid = toml").expect("should write invalid config");

        let err = load_config(&path).expect_err("invalid config should error");
        match err {
            ConfigError::Parse { .. } => {}
            _ => panic!("expected parse error, got {err:?}"),
        }

        fs::remove_file(&path).ok();
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should go forward")
            .as_nanos()
    }
}
