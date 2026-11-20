#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

fn ato(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ato"));
    command.env("ATO_HOME", home);
    command.stdin(Stdio::null());
    command
}

fn require_success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Session {
    child: Option<Child>,
    reader: Option<thread::JoinHandle<Vec<u8>>>,
    url: String,
}

impl Session {
    fn start(home: &Path, capsule: &Path, options: &[&str]) -> Self {
        let mut arguments = vec!["--bind-env", "access_key=TEST_NOTES_KEY"];
        arguments.extend_from_slice(options);
        Self::launch(home, capsule, &arguments)
    }

    fn launch(home: &Path, capsule: &Path, options: &[&str]) -> Self {
        let mut child = ato(home)
            .arg("run")
            .arg(capsule)
            .arg("--headless")
            .env("TEST_NOTES_KEY", "test-notes-key")
            .args(options)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut captured = Vec::new();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = line.strip_prefix("URL: ") {
                    let _ = send.send(url.to_owned());
                }
                captured.extend_from_slice(line.as_bytes());
                captured.push(b'\n');
            }
            captured
        });
        let url = match receive.recv_timeout(Duration::from_secs(25)) {
            Ok(url) => url,
            Err(error) => {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                reader.join().unwrap();
                panic!(
                    "App did not become ready: {error}. {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        };
        Self {
            child: Some(child),
            reader: Some(reader),
            url,
        }
    }

    fn request(&self, body: Option<&str>) -> String {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        match body {
            Some(body) => client.post(&self.url).body(body.to_owned()).send(),
            None => client.get(&self.url).send(),
        }
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .unwrap()
    }

    fn stop(self) -> Output {
        // SAFETY: the PID is for the still-owned child of this test.
        assert_eq!(
            unsafe { libc::kill(self.child.as_ref().unwrap().id() as i32, libc::SIGTERM) },
            0
        );
        self.wait()
    }

    fn crash(mut self) -> Output {
        self.child.as_mut().unwrap().kill().unwrap();
        self.wait()
    }

    fn wait(mut self) -> Output {
        let mut output = self.child.take().unwrap().wait_with_output().unwrap();
        output.stdout = self.reader.take().unwrap().join().unwrap();
        output
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn ready_fixture() -> Option<(tempfile::TempDir, PathBuf)> {
    #[cfg(target_os = "linux")]
    if !ato_sandbox::bubblewrap_containment_available() {
        eprintln!("The managed Run test requires bubblewrap");
        return None;
    }
    #[cfg(target_os = "macos")]
    if !ato_host_facts::capability::NativeFacts::probe().sandbox_tool {
        eprintln!("The managed Run test requires Seatbelt");
        return None;
    }

    let home = tempfile::tempdir().unwrap();
    let capsule = home.path().join("notes.capsule");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/run-data");
    let output = ato(home.path())
        .arg("pack")
        .arg(fixture)
        .arg("-o")
        .arg(&capsule)
        .output()
        .unwrap();
    require_success(&output);
    let missing = ato(home.path())
        .arg("run")
        .arg(&capsule)
        .arg("--headless")
        .output()
        .unwrap();
    let message = String::from_utf8_lossy(&missing.stderr);
    if message.contains("install that runtime with `uv python install 3.12`") {
        eprintln!("{message}");
        return None;
    }
    assert!(!missing.status.success());
    assert!(message.contains("--bind-env access_key="), "{message}");
    assert!(!home.path().join("run-data").exists());
    Some((home, capsule))
}

fn state_ids(home: &Path) -> Vec<String> {
    fs::read_dir(home.join("run-data/entries"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[test]
fn a_foreground_run_saves_reopens_discards_and_recovers_local_data() {
    let Some((home, capsule)) = ready_fixture() else {
        return;
    };
    let home = home.path();

    let first = Session::start(home, &capsule, &["--on-exit", "save"]);
    assert_eq!(first.request(None), "");
    first.request(Some("original saved note"));
    let output = first.stop();
    assert!(require_success(&output).contains("Saved local data"));
    let id = state_ids(home).pop().unwrap();

    let original = fs::read(&capsule).unwrap();
    fs::write(&capsule, b"the mutable path now points at another revision").unwrap();
    let resumed = Session::start(home, &capsule, &["--data-id", &id, "--on-exit", "discard"]);
    assert_eq!(resumed.request(None), "original saved note");
    resumed.request(Some("discard this change"));
    let output = resumed.stop();
    assert!(require_success(&output).contains("Bundle cache retained"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Using the verified bundle cache"));
    fs::write(&capsule, original).unwrap();

    let crashed = Session::start(home, &capsule, &["--fresh"]);
    assert_eq!(crashed.request(None), "");
    crashed.request(Some("recover after disconnect"));
    assert!(!crashed.crash().status.success());
    let recovered_id = state_ids(home)
        .into_iter()
        .find(|other| other != &id)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = ato(home)
            .args(["app", "recover", &recovered_id, "save"])
            .output()
            .unwrap();
        if output.status.success() {
            break;
        }
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("still stopping") && Instant::now() < deadline,
            "{error}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    require_success(&ato(home).args(["app", "cache-clear"]).output().unwrap());
    let reopened = Session::start(
        home,
        &capsule,
        &["--data-id", &recovered_id, "--on-exit", "discard"],
    );
    assert_eq!(reopened.request(None), "recover after disconnect");
    assert!(require_success(&reopened.stop()).contains("The previous saved state is available."));

    let pending = Session::start(home, &capsule, &["--data-id", &id]);
    assert_eq!(pending.request(None), "original saved note");
    pending.request(Some("undecided note"));
    let outcome = pending.stop();
    require_success(&outcome);
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("Kept undecided local data"));
    assert!(!String::from_utf8_lossy(&outcome.stderr).contains("Save, Discard, or Later"));
    require_success(
        &ato(home)
            .args(["app", "recover", &id, "discard"])
            .output()
            .unwrap(),
    );
    require_success(
        &ato(home)
            .args(["app", "data-remove", &id])
            .output()
            .unwrap(),
    );
    assert!(!state_ids(home).contains(&id));
    assert!(state_ids(home).contains(&recovered_id));
}

#[test]
fn a_browser_snapshot_can_be_updated_and_reopened_through_run() {
    use std::collections::BTreeMap;

    use ato_objects::{
        CapsuleBundleDocument, PortableDependencyProfile, decode_capsule_bundle_document,
    };
    use ato_portable_application::instance_snapshot::{
        BROWSER_INSTANCE_STATE_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotResourceV1,
        InstanceSnapshotV1, attach_instance_snapshot, encode_browser_state,
    };
    use ato_portable_application::{
        bundle_sha256, portability_export::repack_portable_dependencies,
    };
    use base64::Engine as _;

    let home = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule");
    let CapsuleBundleDocument::PortableApplicationV3(source) =
        decode_capsule_bundle_document(&fs::read(source).unwrap()).unwrap()
    else {
        panic!("the static fixture must remain wire v3");
    };
    let (_, source) =
        repack_portable_dependencies(&source, PortableDependencyProfile::Cached, &BTreeMap::new())
            .unwrap();
    let initial =
        encode_browser_state(&BTreeMap::from([("note".into(), "initial".into())])).unwrap();
    let content_ref = bundle_sha256(&initial);
    let snapshot = InstanceSnapshotV1 {
        schema: INSTANCE_SNAPSHOT_SCHEMA.into(),
        resources: vec![InstanceSnapshotResourceV1 {
            slot: "browser".into(),
            protocol: BROWSER_INSTANCE_STATE_PROTOCOL.into(),
            content_ref: content_ref.clone(),
            save: None,
        }],
        assets: vec![],
        asset_bindings: vec![],
    };
    let (bundle, _) =
        attach_instance_snapshot(&source, snapshot, &BTreeMap::from([(content_ref, initial)]))
            .unwrap();
    let capsule = home.path().join("browser.capsule");
    fs::write(&capsule, &bundle).unwrap();

    let browser = Session::launch(home.path(), &capsule, &["--on-exit", "save"]);
    let html = browser.request(None);
    let (_, token) = html.split_once("'x-ato-state-token':'").unwrap();
    let token = token.split_once('\'').unwrap().0;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    client
        .post(format!(
            "{}/__ato/instance-state/local-storage",
            browser.url
        ))
        .header("x-ato-state-token", token)
        .json(&serde_json::json!({"local_storage":{"note": "updated"}}))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    require_success(&browser.stop());

    let id = state_ids(home.path()).pop().unwrap();
    let browser = Session::launch(
        home.path(),
        &capsule,
        &["--data-id", &id, "--on-exit", "discard"],
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(br#"{"note":"updated"}"#);
    assert!(browser.request(None).contains(&encoded));
    require_success(&browser.stop());
    assert_eq!(fs::read(&capsule).unwrap(), bundle);
}
