#![cfg(target_os = "linux")]

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

fn read_request(stream: &mut TcpStream) -> Option<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut header = Vec::new();
    let mut byte = [0u8];
    while !header.ends_with(b"\r\n\r\n") && header.len() < 16384 {
        stream.read_exact(&mut byte).ok()?;
        header.push(byte[0]);
    }
    let header = String::from_utf8(header).ok()?;
    let size = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    if size > 65536 {
        return None;
    }
    let mut body = vec![0u8; size];
    stream.read_exact(&mut body).ok()?;
    Some(header.lines().next()?.split_whitespace().nth(1)?.to_owned())
}

#[test]
fn supervised_worker_polls_both_queues_and_honours_source_drain() {
    check_supervised_queues(false);
}

#[test]
fn dedicated_source_worker_never_polls_normal_queue_and_honours_source_drain() {
    check_supervised_queues(true);
}

fn check_supervised_queues(source_only: bool) {
    assert!(
        ato_formation_worker::sandbox::containment_available(),
        "a real worker requires bubblewrap containment"
    );
    std::fs::create_dir_all(".tmp").unwrap();
    let work = tempfile::tempdir_in(".tmp").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = calls.clone();
    let done = Arc::new(AtomicBool::new(false));
    let finish = done.clone();
    let server = thread::spawn(move || {
        let mut source_polls = 0;
        while !finish.load(Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let Some(path) = read_request(&mut stream) else {
                continue;
            };
            observed.lock().unwrap().push(path.clone());
            let (status, body) = if path.starts_with("/v1/internal/formation/source-archives/claim")
            {
                source_polls += 1;
                if source_polls == 1 {
                    ("200 OK", r#"{"job_id":null}"#)
                } else {
                    ("409 Conflict", r#"{"error":"runner_drained"}"#)
                }
            } else if path.starts_with("/v1/internal/formation/claim") {
                ("200 OK", r#"{"job_id":null}"#)
            } else if path == "/v1/internal/formation/workers/self" {
                (
                    "200 OK",
                    r#"{"ok":true,"drain":false,"target_version":null,"target_digest":null,"min_version":null,"next_heartbeat_seconds":1}"#,
                )
            } else {
                ("404 Not Found", "{}")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_ato-formation-worker"));
    command.args([
        "serve",
        "--api-base",
        &base,
        "--work-root",
        work.path().to_str().unwrap(),
        "--worker-id",
        "test-worker",
        "--poll-ms",
        "20",
        "--heartbeat-ms",
        "1000",
    ]);
    if source_only {
        command.args(["--queue", "source-only"]);
    }
    let mut worker = command
        .env("ATO_FORMATION_TOKEN", "fixture-token")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = worker.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = worker.kill();
            let _ = worker.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    done.store(true, Ordering::SeqCst);
    server.join().unwrap();
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(75),
        "worker must stop on source claim drain"
    );
    let trace = calls.lock().unwrap();
    let source = trace
        .iter()
        .enumerate()
        .filter(|(_, path)| path.starts_with("/v1/internal/formation/source-archives/claim"))
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let normal = trace
        .iter()
        .enumerate()
        .filter(|(_, path)| path.starts_with("/v1/internal/formation/claim"))
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    assert_eq!(source.len(), 2, "{trace:?}");
    if source_only {
        assert!(normal.is_empty(), "{trace:?}");
    } else {
        assert!(
            normal.iter().any(|i| *i > source[0] && *i < source[1]),
            "{trace:?}"
        );
    }
    assert!(normal.iter().all(|i| *i < source[1]), "{trace:?}");
}
