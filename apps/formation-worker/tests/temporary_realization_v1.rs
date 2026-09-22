//! Temporary realization on the local Runtime (ADR-019).
//!
//! A candidate is run to be measured through the Runtime's own process
//! executor — bwrap namespaces, the Landlock shim, the endpoint ABI — and
//! destroyed before the attempt returns. The Derivation's argv, environment
//! and cwd are executed exactly: nothing reads them for port numbers.
//!
//! These tests run the real thing. On a host that cannot contain a process
//! they assert the refusal instead, because a Formation that cannot contain a
//! candidate must not run it.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ato_formation::intent::{DependencyPlan, Lane, PROGRAM_INTENT_V1_SCHEMA, ProgramIntentV1};
use ato_formation_worker::ephemeral::{
    RequiredObservation, RequiredPort, TemporaryRealization, TemporaryRealizationRequest,
};
use ato_formation_worker::sandbox::containment_available;

/// An endpoint-aware server: it binds the port the Runtime hands it through
/// ATO_ENDPOINT_APP_HTTP_PORT, and reports on its own surroundings.
const PROBE_SERVER: &str = r#"
import json, os, socket, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

for target in ("/app/runtime-created.txt", "/tmp/runtime-created.txt"):
    try:
        with open(target, "w") as handle:
            handle.write("written by the candidate")
    except OSError:
        pass

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        url = urlparse(self.path)
        query = parse_qs(url.query)
        status, body = 404, b""
        if url.path == "/health":
            status, body = 200, b"ok"
        elif url.path == "/fail":
            status, body = 500, b"no"
        elif url.path == "/argv":
            status, body = 200, json.dumps(sys.argv).encode()
        elif url.path == "/cwd":
            status, body = 200, os.getcwd().encode()
        elif url.path == "/env":
            status, body = 200, os.environ.get(query["name"][0], "<unset>").encode()
        elif url.path == "/read":
            try:
                with open(query["path"][0], "rb") as handle:
                    status, body = 200, handle.read()
            except OSError as error:
                status, body = 403, str(error).encode()
        elif url.path == "/egress":
            try:
                socket.create_connection(("1.1.1.1", 443), timeout=3).close()
                status, body = 200, b"open"
            except OSError as error:
                status, body = 200, ("blocked " + str(error)).encode()
        self.send_response(status)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

port = int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;

/// A fixed-port server: it binds the port its argv names, literally.
const FIXED_SERVER: &str = r#"
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == "/health" else 404)
        self.end_headers()
        self.wfile.write(b"ok" if self.path == "/health" else b"")

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", int(sys.argv[2])), Handler).serve_forever()
"#;

fn python() -> &'static str {
    "/usr/bin/python3"
}

fn intent(argv: Vec<String>, guest_port: u16) -> ProgramIntentV1 {
    ProgramIntentV1 {
        schema: PROGRAM_INTENT_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        runtime: BTreeMap::new(),
        dependencies: DependencyPlan::None,
        launch_argv: argv,
        cwd_relative: String::new(),
        public_env: BTreeMap::new(),
        exported_ports: vec![("app.http".to_owned(), guest_port)],
        readiness_http_path: None,
        state_slots: Vec::new(),
        static_output_root: None,
        static_entry_path: None,
        static_spa_fallback: false,
        static_build: None,
        static_compile: None,
    }
}

/// The probe server's argv, with a marker the processes can be found by and
/// two values that merely LOOK like the guest port.
fn server_argv(marker: &str, guest_port: u16) -> Vec<String> {
    vec![
        python().to_owned(),
        "/app/server.py".to_owned(),
        marker.to_owned(),
        guest_port.to_string(),
        format!("http://example:{guest_port}"),
        format!("--port={guest_port}"),
    ]
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("server.py"), PROBE_SERVER).expect("server");
    std::fs::write(dir.path().join("fixed.py"), FIXED_SERVER).expect("fixed");
    std::fs::create_dir_all(dir.path().join("server")).expect("subdir");
    std::fs::write(dir.path().join("server/app.py"), PROBE_SERVER).expect("nested");
    dir
}

fn marker(name: &str) -> String {
    format!("ato-formation-test-{name}-{}", std::process::id())
}

fn processes_matching(marker: &str) -> Vec<String> {
    let output = std::process::Command::new("pgrep")
        .args(["-f", marker])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_gone(marker: &str) {
    // The Runtime's stop proves the group is gone; this checks from outside.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !processes_matching(marker).is_empty() {
        assert!(
            Instant::now() < deadline,
            "candidate processes outlived the realization: {:?}",
            processes_matching(marker)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn shim() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ato-formation-worker"))
}

struct Launch {
    source: tempfile::TempDir,
    scratch: tempfile::TempDir,
    intent: ProgramIntentV1,
    ports: Vec<RequiredPort>,
}

impl Launch {
    fn new(argv: Vec<String>, guest_port: u16) -> Self {
        Self {
            source: workspace(),
            scratch: tempfile::tempdir().expect("tempdir"),
            intent: intent(argv, guest_port),
            ports: vec![RequiredPort {
                port_id: "app.http".to_owned(),
                guest_port,
            }],
        }
    }

    fn realization_scratch(&self) -> PathBuf {
        self.scratch.path().join("realization")
    }

    fn launch(&self) -> anyhow::Result<TemporaryRealization> {
        TemporaryRealization::launch(&TemporaryRealizationRequest {
            workspace: self.source.path(),
            scratch: &self.realization_scratch(),
            intent: &self.intent,
            ports: &self.ports,
            shim: &shim(),
            attempt_id: "test",
        })
    }
}

fn get(realization: &TemporaryRealization, path: &str) -> (u16, String) {
    let observed = realization
        .observe(&[RequiredObservation {
            port_id: "app.http".to_owned(),
            path: path.to_owned(),
        }])
        .expect("observed");
    let port = realization.endpoints()[0].host_port;
    // The digest is what evidence keeps; the tests also want the words.
    let body = reqwest::blocking::get(format!("http://127.0.0.1:{port}{path}"))
        .and_then(|response| response.text())
        .unwrap_or_default();
    (observed[0].status, body)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// On a host that cannot contain a process, the realization refuses to run
/// it at all.
fn refused_here() -> bool {
    if containment_available() {
        return false;
    }
    let launch = Launch::new(server_argv(&marker("refused"), free_port()), 8080);
    let error = launch
        .launch()
        .err()
        .expect("an uncontainable candidate is refused, not run on the host");
    assert!(format!("{error:#}").contains("contain"), "{error:#}");
    assert_gone(&marker("refused"));
    true
}

// ── the endpoint ABI, and exact execution ───────────────────────────────────

#[test]
fn an_endpoint_aware_candidate_runs_on_the_runtime_port_when_its_guest_port_is_taken() {
    if refused_here() {
        return;
    }
    // Somebody else already listens on the Derivation's guest port.
    let occupant = TcpListener::bind("127.0.0.1:0").expect("occupant");
    let guest_port = occupant.local_addr().expect("addr").port();
    let marker = marker("endpoint-aware");
    let launch = Launch::new(server_argv(&marker, guest_port), guest_port);

    let realization = launch.launch().expect("launched");
    let endpoint = realization.endpoints()[0].clone();
    assert_eq!(endpoint.guest_port, guest_port);
    assert_ne!(endpoint.host_port, guest_port);
    assert_eq!(get(&realization, "/health"), (200, "ok".to_owned()));
    // The ABI carried the Runtime's port.
    let (_, injected) = get(&realization, "/env?name=ATO_ENDPOINT_APP_HTTP_PORT");
    assert_eq!(injected, endpoint.host_port.to_string());
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
    drop(occupant);
}

#[test]
fn the_derivation_argv_reaches_the_candidate_unchanged() {
    if refused_here() {
        return;
    }
    // Even with the guest port taken — the case a rewrite would have
    // "helped" — every value that looks like a port arrives as written.
    let occupant = TcpListener::bind("127.0.0.1:0").expect("occupant");
    let guest_port = occupant.local_addr().expect("addr").port();
    let marker = marker("argv");
    let argv = server_argv(&marker, guest_port);
    let launch = Launch::new(argv.clone(), guest_port);
    let realization = launch.launch().expect("launched");
    let (_, reported) = get(&realization, "/argv");
    let reported: Vec<String> = serde_json::from_str(&reported).expect("argv json");
    // sys.argv drops the interpreter; everything after it is verbatim.
    assert_eq!(reported, argv[1..].to_vec());
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
    drop(occupant);
}

#[test]
fn a_fixed_port_candidate_runs_on_its_own_port_when_it_is_free() {
    if refused_here() {
        return;
    }
    let guest_port = free_port();
    let marker = marker("fixed-free");
    let launch = Launch::new(
        vec![
            python().to_owned(),
            "/app/fixed.py".to_owned(),
            marker.clone(),
            guest_port.to_string(),
        ],
        guest_port,
    );
    let realization = launch.launch().expect("launched");
    assert_eq!(realization.endpoints()[0].host_port, guest_port);
    assert_eq!(get(&realization, "/health").0, 200);
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
}

#[test]
fn a_fixed_port_candidate_fails_visibly_when_its_port_is_taken() {
    if refused_here() {
        return;
    }
    let occupant = TcpListener::bind("127.0.0.1:0").expect("occupant");
    let guest_port = occupant.local_addr().expect("addr").port();
    let marker = marker("fixed-taken");
    let launch = Launch::new(
        vec![
            python().to_owned(),
            "/app/fixed.py".to_owned(),
            marker.clone(),
            guest_port.to_string(),
        ],
        guest_port,
    );
    let error = launch
        .launch()
        .err()
        .expect("a literal port that is taken is not moved elsewhere");
    let text = format!("{error:#}");
    assert!(text.contains("cannot run here"), "{text}");
    assert!(text.contains("ATO_ENDPOINT_APP_HTTP_PORT"), "{text}");
    assert!(!launch.realization_scratch().exists());
    assert_gone(&marker);
    drop(occupant);
}

// ── cwd ─────────────────────────────────────────────────────────────────────

#[test]
fn a_candidate_starts_in_its_derivation_cwd() {
    if refused_here() {
        return;
    }
    let marker = marker("cwd");
    let mut launch = Launch::new(
        vec![python().to_owned(), "app.py".to_owned(), marker.clone()],
        free_port(),
    );
    launch.intent.cwd_relative = "server".to_owned();
    let realization = launch.launch().expect("launched from /app/server");
    assert_eq!(get(&realization, "/health").0, 200);
    assert_eq!(get(&realization, "/cwd").1, "/app/server");
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
}

#[test]
fn a_cwd_outside_the_workspace_is_refused() {
    let mut launch = Launch::new(server_argv(&marker("escape"), 8080), 8080);
    launch.intent.cwd_relative = "../escape".to_owned();
    assert!(launch.launch().is_err());
    assert!(!launch.realization_scratch().exists());
}

// ── containment and disposal ────────────────────────────────────────────────

#[test]
fn verification_side_effects_stay_in_the_disposable_copy() {
    if refused_here() {
        return;
    }
    let marker = marker("side-effects");
    let port = free_port();
    let launch = Launch::new(server_argv(&marker, port), port);
    let realization = launch.launch().expect("launched");
    assert_eq!(get(&realization, "/health").0, 200);
    realization.destroy().expect("destroyed");

    assert!(!launch.source.path().join("runtime-created.txt").exists());
    assert!(!launch.realization_scratch().exists());
    assert_gone(&marker);
}

#[test]
fn the_candidate_cannot_read_host_paths_it_was_not_given() {
    if refused_here() {
        return;
    }
    let secret_dir = tempfile::tempdir().expect("tempdir");
    let secret = secret_dir.path().join("secret.txt");
    std::fs::write(&secret, "host secret").expect("secret");
    let home_file = std::env::var("HOME")
        .map(|home| format!("{home}/.bashrc"))
        .unwrap_or_else(|_| "/root/.bashrc".to_owned());

    let marker = marker("filesystem");
    let port = free_port();
    let launch = Launch::new(server_argv(&marker, port), port);
    let realization = launch.launch().expect("launched");
    for path in [
        secret.to_string_lossy().into_owned(),
        home_file,
        launch
            .source
            .path()
            .join("server.py")
            .to_string_lossy()
            .into_owned(),
    ] {
        let (status, body) = get(&realization, &format!("/read?path={path}"));
        assert_ne!(status, 200, "{path} was readable: {body}");
    }
    // What it WAS given is readable, at its guest path.
    assert_eq!(get(&realization, "/read?path=/app/server.py").0, 200);
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
}

#[test]
fn the_candidate_has_no_egress() {
    if refused_here() {
        return;
    }
    let marker = marker("egress");
    let port = free_port();
    let launch = Launch::new(server_argv(&marker, port), port);
    let realization = launch.launch().expect("launched");
    let (status, body) = get(&realization, "/egress");
    assert_eq!(status, 200);
    assert!(
        body.starts_with("blocked"),
        "the candidate reached 1.1.1.1:443: {body}"
    );
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
}

#[test]
fn a_chatty_candidate_cannot_fill_the_disk() {
    if refused_here() {
        return;
    }
    let dir = workspace();
    // ~32 MiB of output before it starts serving, against an 8 MiB bound.
    std::fs::write(
        dir.path().join("chatty.py"),
        "import os, sys\nfor _ in range(4096):\n    sys.stdout.write('x' * 8192 + '\\n')\n\
             sys.stdout.flush()\nexec(open('/app/server.py').read())\n",
    )
    .expect("chatty");
    let marker = marker("chatty");
    let port = free_port();
    let mut launch = Launch::new(
        vec![
            python().to_owned(),
            "/app/chatty.py".to_owned(),
            marker.clone(),
        ],
        port,
    );
    launch.source = dir;
    let realization = launch.launch().expect("launched");
    assert_eq!(get(&realization, "/health").0, 200);
    let kept: u64 = std::fs::read_dir(launch.realization_scratch())
        .expect("scratch")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("candidate.log")
        })
        .map(|entry| entry.metadata().map(|meta| meta.len()).unwrap_or(0))
        .sum();
    assert!(
        kept <= 8 * 1024 * 1024,
        "kept {kept} bytes of candidate output"
    );
    realization.destroy().expect("destroyed");
    assert_gone(&marker);
}

#[test]
fn a_failing_observation_still_ends_with_the_candidate_gone() {
    if refused_here() {
        return;
    }
    let marker = marker("fail");
    let port = free_port();
    let launch = Launch::new(server_argv(&marker, port), port);
    {
        let realization = launch.launch().expect("launched");
        assert_eq!(get(&realization, "/fail").0, 500);
        // No destroy: the error path drops it.
    }
    assert!(!launch.realization_scratch().exists());
    assert_gone(&marker);
}

#[test]
fn a_candidate_that_never_listens_times_out_and_is_gone() {
    if refused_here() {
        return;
    }
    let marker = marker("timeout");
    let port = free_port();
    let launch = Launch::new(
        vec![
            python().to_owned(),
            "-c".to_owned(),
            "import time; time.sleep(600)".to_owned(),
            marker.clone(),
        ],
        port,
    );
    let started = Instant::now();
    let error = launch
        .launch()
        .err()
        .expect("a candidate that never becomes ready is an error");
    assert!(format!("{error:#}").contains("ready"), "{error:#}");
    assert!(started.elapsed() >= Duration::from_secs(25));
    assert!(!launch.realization_scratch().exists());
    assert_gone(&marker);
}

#[test]
fn a_candidate_that_exits_during_startup_is_reported_with_its_output() {
    if refused_here() {
        return;
    }
    let marker = marker("exits");
    let port = free_port();
    let launch = Launch::new(
        vec![
            python().to_owned(),
            "-c".to_owned(),
            "import sys; print('boom from the candidate'); sys.exit(3)".to_owned(),
            marker.clone(),
        ],
        port,
    );
    let error = launch
        .launch()
        .err()
        .expect("an exiting candidate is an error");
    let text = format!("{error:#}");
    assert!(text.contains("exited before becoming ready"), "{text}");
    assert!(text.contains("boom from the candidate"), "{text}");
    assert!(!launch.realization_scratch().exists());
    assert_gone(&marker);
}

#[test]
fn a_relative_scratch_path_still_realizes() {
    if refused_here() {
        return;
    }
    // `ato form --work-root work` hands the realization a relative path;
    // bwrap binds from a working directory of its own, so it must not leak
    // through as relative.
    let launch = Launch::new(server_argv(&marker("relative"), free_port()), free_port());
    let absolute = launch.realization_scratch();
    let cwd = std::env::current_dir().expect("cwd");
    let mut relative = PathBuf::new();
    for _ in cwd.components().skip(1) {
        relative.push("..");
    }
    relative.push(absolute.strip_prefix("/").expect("absolute scratch"));
    assert!(relative.is_relative());

    let realization = TemporaryRealization::launch(&TemporaryRealizationRequest {
        workspace: launch.source.path(),
        scratch: &relative,
        intent: &launch.intent,
        ports: &launch.ports,
        shim: &shim(),
        attempt_id: "relative",
    })
    .expect("launched from a relative scratch path");
    assert_eq!(get(&realization, "/health").0, 200);
    realization.destroy().expect("destroyed");
    assert!(!absolute.exists());
    assert_gone(&marker("relative"));
}
