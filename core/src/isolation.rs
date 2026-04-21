use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct HostIsolationContext {
    base_dir: PathBuf,
    vars: HashMap<String, String>,
}

impl HostIsolationContext {
    pub fn new(root: &Path, namespace: &str) -> std::io::Result<Self> {
        let base_dir = root.join(format!(".ato-{namespace}-host"));
        let home_dir = base_dir.join("home");
        let tmp_dir = base_dir.join("tmp");
        let cache_dir = base_dir.join("cache");
        let config_dir = base_dir.join("config");
        let pnpm_store_dir = cache_dir.join("pnpm-store");
        let npm_cache_dir = cache_dir.join("npm");
        let yarn_cache_dir = cache_dir.join("yarn");
        let uv_cache_dir = cache_dir.join("uv");
        let pip_cache_dir = cache_dir.join("pip");
        let bun_cache_dir = cache_dir.join("bun");

        for dir in [
            &home_dir,
            &tmp_dir,
            &cache_dir,
            &config_dir,
            &pnpm_store_dir,
            &npm_cache_dir,
            &yarn_cache_dir,
            &uv_cache_dir,
            &pip_cache_dir,
            &bun_cache_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }

        let npm_userconfig = config_dir.join("npmrc");
        let pip_config = config_dir.join("pip.conf");
        for file in [&npm_userconfig, &pip_config] {
            if !file.exists() {
                std::fs::write(file, "")?;
            }
        }

        let vars = HashMap::from([
            ("HOME".to_string(), home_dir.to_string_lossy().to_string()),
            (
                "USERPROFILE".to_string(),
                home_dir.to_string_lossy().to_string(),
            ),
            ("TMPDIR".to_string(), tmp_dir.to_string_lossy().to_string()),
            ("TMP".to_string(), tmp_dir.to_string_lossy().to_string()),
            ("TEMP".to_string(), tmp_dir.to_string_lossy().to_string()),
            (
                "XDG_CACHE_HOME".to_string(),
                cache_dir.to_string_lossy().to_string(),
            ),
            (
                "XDG_CONFIG_HOME".to_string(),
                config_dir.to_string_lossy().to_string(),
            ),
            (
                "npm_config_cache".to_string(),
                npm_cache_dir.to_string_lossy().to_string(),
            ),
            (
                "npm_config_userconfig".to_string(),
                npm_userconfig.to_string_lossy().to_string(),
            ),
            (
                "NPM_CONFIG_USERCONFIG".to_string(),
                npm_userconfig.to_string_lossy().to_string(),
            ),
            (
                "pnpm_config_store_dir".to_string(),
                pnpm_store_dir.to_string_lossy().to_string(),
            ),
            (
                "PNPM_HOME".to_string(),
                pnpm_store_dir.to_string_lossy().to_string(),
            ),
            (
                "YARN_CACHE_FOLDER".to_string(),
                yarn_cache_dir.to_string_lossy().to_string(),
            ),
            (
                "UV_CACHE_DIR".to_string(),
                uv_cache_dir.to_string_lossy().to_string(),
            ),
            (
                "PIP_CACHE_DIR".to_string(),
                pip_cache_dir.to_string_lossy().to_string(),
            ),
            (
                "PIP_CONFIG_FILE".to_string(),
                pip_config.to_string_lossy().to_string(),
            ),
            (
                "BUN_INSTALL_CACHE_DIR".to_string(),
                bun_cache_dir.to_string_lossy().to_string(),
            ),
        ]);

        Ok(Self { base_dir, vars })
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    pub fn protects_key(&self, key: &str) -> bool {
        self.vars.contains_key(key)
    }

    pub fn apply_to_command(
        &self,
        command: &mut Command,
        extra_env: impl IntoIterator<Item = (String, String)>,
    ) {
        command.env_clear();
        for key in passthrough_env_keys() {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        // Pass through CAPSULE_* prefix vars from the host environment (spec §2.4).
        for (key, value) in std::env::vars() {
            if key.starts_with("CAPSULE_") {
                command.env(&key, value);
            }
        }
        command.envs(&self.vars);
        for (key, value) in extra_env {
            if self.protects_key(&key) {
                continue;
            }
            command.env(key, value);
        }
    }
}

/// Environment variables passed through from the host to the capsule subprocess.
///
/// Spec §2.4 baseline: `PATH`, `LANG`, `HOME` (reconstructed by `HostIsolationContext`),
/// and `CAPSULE_*` prefix (handled separately in `apply_to_command`).
///
/// The proxy and TLS certificate variables below are pragmatic additions beyond the
/// strict spec minimum. They are needed for capsules running behind corporate proxies
/// or on systems with custom CA bundles, where stripping them would silently break
/// outbound TLS. The Windows-specific vars (`SYSTEMROOT`, `WINDIR`, `COMSPEC`,
/// `PATHEXT`) are required for the Windows runtime to function at all.
pub fn passthrough_env_keys() -> &'static [&'static str] {
    &[
        "PATH",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
        "GIT_SSL_CAINFO",
        "CURL_CA_BUNDLE",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
    ]
}

#[cfg(test)]
mod tests {
    use super::HostIsolationContext;
    use std::collections::HashMap;

    #[test]
    fn isolation_context_sets_expected_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let context = HostIsolationContext::new(temp.path(), "test").expect("context");

        assert!(context.base_dir().ends_with(".ato-test-host"));
        assert!(context.vars().get("HOME").is_some());
        assert!(context.vars().get("TMPDIR").is_some());
        assert!(context.vars().get("npm_config_cache").is_some());
        assert!(context.vars().get("UV_CACHE_DIR").is_some());
    }

    #[test]
    fn apply_to_command_preserves_isolation_keys() {
        let temp = tempfile::tempdir().expect("tempdir");
        let context = HostIsolationContext::new(temp.path(), "test").expect("context");
        let mut command = std::process::Command::new("echo");
        command.arg("ok");

        context.apply_to_command(
            &mut command,
            vec![
                ("HOME".to_string(), "/unsafe".to_string()),
                ("ATO_SERVICE_DB_HOST".to_string(), "127.0.0.1".to_string()),
            ],
        );

        let envs = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect::<HashMap<_, _>>();

        assert_ne!(
            envs.get("HOME").and_then(|value| value.clone()),
            Some("/unsafe".to_string())
        );
        assert_eq!(
            envs.get("ATO_SERVICE_DB_HOST")
                .and_then(|value| value.clone()),
            Some("127.0.0.1".to_string())
        );
    }
}
