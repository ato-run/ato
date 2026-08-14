#[cfg(unix)]
mod unix_tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use nacelle::config::{
        HealthCheck, NetworkConfig, RuntimeConfig, SandboxConfig, ServiceConfig,
    };
    use nacelle::internal_api::NacelleEvent;
    use nacelle::manager::r3_supervisor::{
        run_services_from_config, run_services_from_config_with_events,
    };

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

    fn python3_available() -> bool {
        Command::new("python3")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn free_port() -> Option<u16> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        Some(listener.local_addr().ok()?.port())
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

    #[tokio::test]
    async fn health_check_emits_ipc_ready_event() {
        if !python3_available() {
            eprintln!("Skipping r3 health-check contract test: python3 unavailable");
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let source = root.join("source");
        fs::create_dir_all(&source).unwrap();

        let Some(port) = free_port() else {
            eprintln!("Skipping r3 health-check contract test: localhost bind unavailable");
            return;
        };
        let server_path = source.join("server.py");
        fs::write(
            &server_path,
            format!(
                r#"import http.server
import socketserver
import threading
import time

PORT = {port}

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, format, *args):
        return

with socketserver.TCPServer(("127.0.0.1", PORT), Handler) as httpd:
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    time.sleep(2.5)
    httpd.shutdown()
    thread.join(timeout=1.0)
"#,
            ),
        )
        .unwrap();

        let mut config = base_config();
        config.services.insert(
            "main".to_string(),
            ServiceConfig {
                executable: "python3".to_string(),
                args: vec!["source/server.py".to_string()],
                cwd: Some(".".to_string()),
                env: None,
                signals: None,
                depends_on: None,
                health_check: Some(HealthCheck {
                    http_get: Some("/health".to_string()),
                    tcp_connect: Some("127.0.0.1".to_string()),
                    port: port.to_string(),
                    interval_secs: Some(1),
                    timeout_secs: Some(5),
                }),
                ports: None,
            },
        );

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = tokio::spawn({
            let config = config.clone();
            let root = root.clone();
            async move {
                run_services_from_config_with_events(&config, &root, None, false, Some(event_tx))
                    .await
            }
        });

        let ready = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ready,
            NacelleEvent::IpcReady {
                service: "main".to_string(),
                endpoint: format!("tcp://127.0.0.1:{port}"),
                port: Some(port),
            }
        );

        let exited = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            exited,
            NacelleEvent::ServiceExited {
                service: "main".to_string(),
                exit_code: Some(0),
            }
        );

        let result = tokio::time::timeout(Duration::from_secs(3), runner)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn non_main_exit_emits_service_exited_event() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
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

        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            run_services_from_config_with_events(&config, &root, None, false, Some(event_tx)),
        )
        .await
        .unwrap();

        assert!(result.is_err());

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            event,
            NacelleEvent::ServiceExited {
                service: "side".to_string(),
                exit_code: Some(2),
            }
        );
    }
}
