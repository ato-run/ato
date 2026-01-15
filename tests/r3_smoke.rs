#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use nacelle::engine::r3_supervisor::run_services_from_config;
    use nacelle::runtime_config::{RuntimeConfig, SandboxConfig};

    fn write_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        let mut perms = fs::metadata(path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).unwrap();
    }

    fn base_config() -> RuntimeConfig {
        RuntimeConfig {
            version: "1.0.0".to_string(),
            services: Default::default(),
            sandbox: SandboxConfig {
                enabled: false,
                filesystem: None,
                network: nacelle::runtime_config::NetworkConfig {
                    enabled: false,
                    allow_domains: None,
                    enforcement: "best_effort".to_string(),
                    egress: None,
                },
                development_mode: None,
            },
            metadata: None,
        }
    }

    #[tokio::test]
    async fn main_exit_terminates_sidecars() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();

        let main_path = source.join("main.sh");
        let side_path = source.join("side.sh");

        write_executable(&main_path, "#!/bin/sh\nexit 0\n");
        write_executable(&side_path, "#!/bin/sh\nsleep 5\necho side > side.txt\n");

        let mut config = base_config();
        config.services.insert(
            "main".to_string(),
            nacelle::runtime_config::ServiceConfig {
                executable: "source/main.sh".to_string(),
                args: vec![],
                cwd: Some("source".to_string()),
                env: None,
                signals: None,
                depends_on: None,
                health_check: None,
                ports: None,
            },
        );
        config.services.insert(
            "side".to_string(),
            nacelle::runtime_config::ServiceConfig {
                executable: "source/side.sh".to_string(),
                args: vec![],
                cwd: Some("source".to_string()),
                env: None,
                signals: None,
                depends_on: None,
                health_check: None,
                ports: None,
            },
        );

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            run_services_from_config(&config, root, None),
        )
        .await
        .unwrap();

        assert!(result.is_ok());
        assert!(!source.join("side.txt").exists());
    }

    #[tokio::test]
    async fn non_main_exit_is_fail_fast() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();

        let main_path = source.join("main.sh");
        let side_path = source.join("side.sh");

        write_executable(&main_path, "#!/bin/sh\nsleep 5\n");
        write_executable(&side_path, "#!/bin/sh\nexit 2\n");

        let mut config = base_config();
        config.services.insert(
            "main".to_string(),
            nacelle::runtime_config::ServiceConfig {
                executable: "source/main.sh".to_string(),
                args: vec![],
                cwd: Some("source".to_string()),
                env: None,
                signals: None,
                depends_on: None,
                health_check: None,
                ports: None,
            },
        );
        config.services.insert(
            "side".to_string(),
            nacelle::runtime_config::ServiceConfig {
                executable: "source/side.sh".to_string(),
                args: vec![],
                cwd: Some("source".to_string()),
                env: None,
                signals: None,
                depends_on: None,
                health_check: None,
                ports: None,
            },
        );

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            run_services_from_config(&config, root, None),
        )
        .await
        .unwrap();

        assert!(result.is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn enforcement_guard_strict_fails_non_linux() {
        let strict = nacelle::engine::enforcement_guard::EnforcementMode::Strict;
        let best_effort = nacelle::engine::enforcement_guard::EnforcementMode::BestEffort;

        assert!(nacelle::engine::enforcement_guard::check_enforcement(strict).is_err());
        assert!(nacelle::engine::enforcement_guard::check_enforcement(best_effort).is_ok());
    }
}
