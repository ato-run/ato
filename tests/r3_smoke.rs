#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    use nacelle::config::{NetworkConfig, RuntimeConfig, SandboxConfig, ServiceConfig};
    use nacelle::manager::r3_supervisor::run_services_from_config;

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
                network: NetworkConfig {
                    enabled: false,
                    enforcement: "best_effort".to_string(),
                    egress: None,
                },
                development_mode: None,
            },
            metadata: None,
            sidecar: None,
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
            ServiceConfig {
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
            ServiceConfig {
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
            run_services_from_config(&config, root, None, false),
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
            ServiceConfig {
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
            ServiceConfig {
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
            run_services_from_config(&config, root, None, false),
        )
        .await
        .unwrap();

        assert!(result.is_err());
    }
}
