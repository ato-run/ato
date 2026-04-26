mod fail_closed_support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use fail_closed_support::prepare_fixture_workspace;
use tempfile::TempDir;

const MANIFEST_LABEL: &str = "oci-multi-component";
const REDIS_IMAGE: &str = "redis:alpine";

fn chmod_executable(path: &std::path::Path) {
    let mut perms = fs::metadata(path)
        .expect("stat fixture entrypoint")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod fixture entrypoint");
}

fn strict_ci() -> bool {
    std::env::var("ATO_STRICT_CI")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn docker_output(args: &[&str]) -> Option<std::process::Output> {
    Command::new("docker").args(args).output().ok()
}

fn docker_ready() -> bool {
    docker_output(&["info"]).is_some_and(|output| output.status.success())
}

fn ensure_image_available(image: &str) -> bool {
    if docker_output(&["image", "inspect", image]).is_some_and(|output| output.status.success()) {
        return true;
    }

    docker_output(&["pull", image]).is_some_and(|output| output.status.success())
}

fn docker_ids(kind: &str, manifest: &str) -> Vec<String> {
    let filter = format!("label=io.ato.manifest={manifest}");
    let output = if kind == "network" {
        Command::new("docker")
            .args(["network", "ls", "-q", "--filter", &filter])
            .output()
            .ok()
    } else {
        Command::new("docker")
            .args(["ps", "-aq", "--filter", &filter])
            .output()
            .ok()
    };

    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn cleanup_docker_resources(manifest: &str) {
    let container_ids = docker_ids("container", manifest);
    if !container_ids.is_empty() {
        let _ = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .args(&container_ids)
            .status();
    }

    let network_ids = docker_ids("network", manifest);
    if !network_ids.is_empty() {
        let _ = Command::new("docker")
            .arg("network")
            .arg("rm")
            .args(&network_ids)
            .status();
    }
}

struct DockerCleanupGuard {
    manifest: String,
}

impl DockerCleanupGuard {
    fn new(manifest: &str) -> Self {
        cleanup_docker_resources(manifest);
        Self {
            manifest: manifest.to_string(),
        }
    }
}

impl Drop for DockerCleanupGuard {
    fn drop(&mut self) {
        cleanup_docker_resources(&self.manifest);
    }
}

fn send_sigterm(child: &mut std::process::Child) {
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
}

fn wait_for_exit(child: &mut std::process::Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll orchestrator child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait().expect("wait for killed child");
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_line_reader<R: Read + Send + 'static>(reader: R, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
}

fn http_get(port: u16) -> String {
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect main HTTP server");
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("write HTTP request");
    let mut buf = String::new();
    stream.read_to_string(&mut buf).expect("read HTTP response");
    buf
}

#[test]
fn oci_orchestration_connects_native_to_redis_and_reaps_containers_on_sigterm() {
    if !docker_ready() {
        eprintln!("skipping OCI orchestration e2e: docker is unavailable");
        return;
    }
    if !ensure_image_available(REDIS_IMAGE) {
        assert!(
            !strict_ci(),
            "strict CI requires docker image {REDIS_IMAGE} to be available"
        );
        eprintln!("skipping OCI orchestration e2e: redis image unavailable");
        return;
    }

    let _docker_guard = DockerCleanupGuard::new(MANIFEST_LABEL);
    let (_workspace, fixture) = prepare_fixture_workspace("oci-multi-component");
    chmod_executable(&fixture.join("run.sh"));
    let home = TempDir::new().expect("create temp HOME");

    let mut child = Command::new(env!("CARGO_BIN_EXE_ato"))
        .arg("run")
        .arg("--yes")
        .arg("--dangerously-skip-permissions")
        .arg(&fixture)
        .env("HOME", home.path())
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orchestrated run");

    let stdout = child.stdout.take().expect("stdout pipe");
    let stderr = child.stderr.take().expect("stderr pipe");
    let (tx, rx) = mpsc::channel();
    spawn_line_reader(stdout, tx.clone());
    spawn_line_reader(stderr, tx);

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut main_port = None;
    let mut bridge_ok = false;
    while Instant::now() < deadline {
        let line = rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap_or_default();
        if line.contains("bridge-ok") {
            bridge_ok = true;
        }
        if let Some(rest) = line.split("http://127.0.0.1:").nth(1) {
            if let Some(port) = rest
                .split('/')
                .next()
                .and_then(|value| value.parse::<u16>().ok())
            {
                main_port = Some(port);
            }
        }
        if main_port.is_some() && bridge_ok {
            break;
        }
    }

    let port = main_port.expect("orchestrator should publish main endpoint");
    assert!(bridge_ok, "bridge container never proved OCI->OCI DNS");

    let response = http_get(port);
    assert!(response.contains("200 OK"), "response={response}");
    assert!(response.contains("db=ok"), "response={response}");
    assert!(
        response.contains("native-loopback-ok"),
        "response={response}"
    );

    let running_containers = docker_ids("container", MANIFEST_LABEL);
    assert!(
        running_containers.len() >= 2,
        "expected redis db + bridge containers to be alive, got {:?}",
        running_containers
    );

    send_sigterm(&mut child);
    let status = wait_for_exit(&mut child, Duration::from_secs(20));
    assert!(
        status.success() || status.code() == Some(143),
        "status={status:?}"
    );

    thread::sleep(Duration::from_secs(1));
    assert!(
        docker_ids("container", MANIFEST_LABEL).is_empty(),
        "docker containers were not cleaned up"
    );
    assert!(
        docker_ids("network", MANIFEST_LABEL).is_empty(),
        "docker network was not cleaned up"
    );
}
