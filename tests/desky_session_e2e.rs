#![allow(deprecated)]

#[cfg(unix)]
mod tests {
    use assert_cmd::Command;
    use serde_json::{json, Value};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::path::{Path, PathBuf};
    use std::process::Command as ProcessCommand;
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    fn capsule() -> Command {
        Command::cargo_bin("ato").expect("ato binary")
    }

    fn sample_dir(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("samples")
            .join(name)
    }

    fn parse_json_output(output: &std::process::Output) -> Value {
        assert!(
            output.status.success(),
            "stderr={} stdout={}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).expect("valid json output")
    }

    fn post_json(url: &str, payload: &Value) -> Value {
        let (port, path) = parse_local_http_url(url);
        let body = serde_json::to_vec(payload).expect("serialize payload");
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect invoke url");
        write!(
            stream,
            "POST {} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            path,
            body.len()
        )
        .expect("write request headers");
        stream.write_all(&body).expect("write request body");
        stream.flush().expect("flush request");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");
        let (_, body) = response.split_once("\r\n\r\n").expect("http response body");
        serde_json::from_str(body).expect("json response body")
    }

    fn parse_local_http_url(url: &str) -> (u16, String) {
        let trimmed = url
            .strip_prefix("http://127.0.0.1:")
            .expect("local http url");
        let (port, path) = trimmed.split_once('/').expect("port/path pair");
        (port.parse().expect("port"), format!("/{}", path))
    }

    fn session_test_env(home: &Path, session_root: &Path) -> Vec<(&'static str, String)> {
        vec![
            ("HOME", home.display().to_string()),
            ("DESKY_SESSION_ROOT", session_root.display().to_string()),
        ]
    }

    fn process_state(pid: i64) -> Option<String> {
        let output = ProcessCommand::new("ps")
            .args(["-p", &pid.to_string(), "-o", "stat="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if status.is_empty() {
            None
        } else {
            Some(status)
        }
    }

    fn wait_for_process_absence(pid: i64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match process_state(pid) {
                None => return,
                Some(state) if state.starts_with('Z') => {}
                Some(_) => {}
            }
            thread::sleep(Duration::from_millis(100));
        }

        panic!("process {} still exists after stop: {:?}", pid, process_state(pid));
    }

    #[test]
    fn desky_session_roundtrip_for_tauri_mock() {
        let home = TempDir::new().expect("temp home");
        let session_root = home.path().join("desky-sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let sample = sample_dir("desky-mock-tauri");

        let mut start = capsule();
        start.args(["app", "session", "start"])
            .arg(&sample)
            .arg("--json");
        for (key, value) in session_test_env(home.path(), &session_root) {
            start.env(key, value);
        }
        let start_output = start.output().expect("run app session start");
        let start_json = parse_json_output(&start_output);
        let session_id = start_json["session"]["session_id"]
            .as_str()
            .expect("session id")
            .to_string();
        let pid = start_json["session"]["pid"].as_i64().expect("session pid");
        let invoke_url = start_json["session"]["invoke_url"]
            .as_str()
            .expect("invoke url")
            .to_string();
        let adapter = start_json["session"]["adapter"]
            .as_str()
            .expect("adapter");
        assert_eq!(adapter, "tauri");

        let response = post_json(
            &invoke_url,
            &json!({
                "jsonrpc": "2.0",
                "id": "req-1",
                "method": "capsule/invoke",
                "params": {
                    "command": "ping",
                    "payload": {
                        "message": "hello from e2e"
                    }
                }
            }),
        );
        assert_eq!(response["result"]["ok"], json!(true));
        assert_eq!(response["result"]["adapter"], json!("tauri"));
        assert_eq!(response["result"]["echo"], json!("hello from e2e"));

        let mut stop = capsule();
        stop.args(["app", "session", "stop", &session_id, "--json"]);
        for (key, value) in session_test_env(home.path(), &session_root) {
            stop.env(key, value);
        }
        let stop_output = stop.output().expect("run app session stop");
        let stop_json = parse_json_output(&stop_output);
        assert_eq!(stop_json["stopped"], json!(true));
        wait_for_process_absence(pid);
        assert!(!session_root.join(format!("{}.json", session_id)).exists());
    }

    #[test]
    fn desky_session_start_smoke_for_wails_mock() {
        let home = TempDir::new().expect("temp home");
        let session_root = home.path().join("desky-sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let sample = sample_dir("desky-mock-wails");

        let mut start = capsule();
        start.args(["app", "session", "start"])
            .arg(&sample)
            .arg("--json");
        for (key, value) in session_test_env(home.path(), &session_root) {
            start.env(key, value);
        }
        let start_output = start.output().expect("run app session start");
        let start_json = parse_json_output(&start_output);
        let session_id = start_json["session"]["session_id"]
            .as_str()
            .expect("session id")
            .to_string();
        assert_eq!(start_json["session"]["adapter"], json!("wails"));

        let mut stop = capsule();
        stop.args(["app", "session", "stop", &session_id, "--json"]);
        for (key, value) in session_test_env(home.path(), &session_root) {
            stop.env(key, value);
        }
        let stop_output = stop.output().expect("run app session stop");
        let stop_json = parse_json_output(&stop_output);
        assert_eq!(stop_json["stopped"], json!(true));
    }

    #[test]
    fn desky_session_stop_reaps_backend_process() {
        let home = TempDir::new().expect("temp home");
        let session_root = home.path().join("desky-sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let sample = sample_dir("desky-mock-tauri");

        let mut start = capsule();
        start.args(["app", "session", "start"])
            .arg(&sample)
            .arg("--json");
        for (key, value) in session_test_env(home.path(), &session_root) {
            start.env(key, value);
        }
        let start_output = start.output().expect("run app session start");
        let start_json = parse_json_output(&start_output);
        let session_id = start_json["session"]["session_id"]
            .as_str()
            .expect("session id")
            .to_string();
        let pid = start_json["session"]["pid"].as_i64().expect("session pid");
        assert!(process_state(pid).is_some(), "backend process should be visible before stop");

        let mut stop = capsule();
        stop.args(["app", "session", "stop", &session_id, "--json"]);
        for (key, value) in session_test_env(home.path(), &session_root) {
            stop.env(key, value);
        }
        let stop_output = stop.output().expect("run app session stop");
        let stop_json = parse_json_output(&stop_output);
        assert_eq!(stop_json["stopped"], json!(true));

        wait_for_process_absence(pid);
        assert!(!session_root.join(format!("{}.json", session_id)).exists());
    }
}
