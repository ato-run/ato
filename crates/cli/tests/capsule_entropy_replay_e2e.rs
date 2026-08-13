//! Fresh OS entropy -> recorded boundary input -> isolated replay acceptance.
//!
//! This is the F1 PTY-boundary precursor to `ato.io.entropy@1`. It proves that
//! actual entropy bytes observed by a producer can be transported as I/O and
//! reproduce the same filesystem State without producer state or live consumer
//! entropy. Entropy request/result semantics belong to the dedicated Connector.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use capsule::protocol_bundle::{PortableCapsule, capture_workspace_state};

const ENTROPY_BYTES: usize = 1_280;
const ENTROPY_BATCHES: usize = 8;

fn scratch_dir(prefix: &str) -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).expect("create hermetic test scratch root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create hermetic test directory")
}

fn ato(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ato"));
    command.env("ATO_HOME", home.join("ato-home"));
    command.env("HOME", home.join("user-home"));
    command
}

fn digest(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn record_digest(records: &[capsule_protocol::IoRecord]) -> String {
    let mut hasher = blake3::Hasher::new();
    for record in records {
        hasher.update(&record.seq.to_be_bytes());
        hasher.update(&record.offset_ns.unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(&record.observed_at_unix_ns.unwrap_or(i64::MIN).to_be_bytes());
        for field in [record.connector.as_str(), record.kind.as_str()] {
            hasher.update(&(field.len() as u64).to_be_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.update(&[match record.direction {
            capsule_protocol::Direction::Ingress => 0,
            capsule_protocol::Direction::Egress => 1,
        }]);
        match &record.payload {
            capsule_protocol::Payload::Inline(bytes) => {
                hasher.update(&[0]);
                hasher.update(&(bytes.len() as u64).to_be_bytes());
                hasher.update(bytes);
            }
            capsule_protocol::Payload::Object(reference) => {
                hasher.update(&[1]);
                hasher.update(&(reference.as_str().len() as u64).to_be_bytes());
                hasher.update(reference.as_str().as_bytes());
            }
        }
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn real_os_entropy() -> Vec<u8> {
    let mut source = fs::File::open("/dev/urandom").expect("open OS entropy source");
    let mut entropy = Vec::with_capacity(ENTROPY_BYTES);
    for _ in 0..ENTROPY_BATCHES {
        let mut batch = [0_u8; ENTROPY_BYTES / ENTROPY_BATCHES];
        source
            .read_exact(&mut batch)
            .expect("read actual OS entropy batch");
        entropy.extend_from_slice(&batch);
    }
    entropy
}

#[test]
fn fresh_os_entropy_replays_to_identical_state_without_producer() {
    let producer = scratch_dir("entropy-producer-");
    let transfer = scratch_dir("entropy-transfer-");
    let workspace = producer.path().join("workspace");
    let state = workspace.join("state");
    fs::create_dir_all(&state).expect("create producer State");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        state.join("initial.json"),
        b"{\"alice\":10000,\"bob\":10000,\"carol\":10000}\n",
    )
    .unwrap();
    fs::set_permissions(
        state.join("initial.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/entropy-replay/randomized_ledger.rs"),
        workspace.join("randomized_ledger.rs"),
    )
    .unwrap();

    let entropy = real_os_entropy();
    let entropy_hex = hex::encode(&entropy);
    let bundle_path = transfer.path().join("fresh-entropy.capsule");
    let shell_command = format!(
        "rustc randomized_ledger.rs -o randomized-ledger && ./randomized-ledger {entropy_hex} state/result.json"
    );
    let captured = ato(producer.path())
        .args(["internal", "capsule-protocol", "capture", "--workspace"])
        .arg(&workspace)
        .arg("--output")
        .arg(&bundle_path)
        .args(["--", "/bin/sh", "-c", &shell_command])
        .output()
        .expect("capture actual entropy boundary input");
    assert!(
        captured.status.success(),
        "capture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&captured.stdout),
        String::from_utf8_lossy(&captured.stderr)
    );
    assert!(
        String::from_utf8_lossy(&captured.stdout).contains("ledger-complete"),
        "producer did not complete ledger computation"
    );

    let bundle = PortableCapsule::read(&bundle_path).expect("read entropy Capsule");
    assert_eq!(bundle.records.len(), 2);
    let recorded_input = match &bundle.records[0].payload {
        capsule_protocol::Payload::Inline(bytes) => bytes,
        capsule_protocol::Payload::Object(_) => panic!("entropy command must be inline"),
    };
    assert!(
        recorded_input
            .windows(entropy_hex.len())
            .any(|window| window == entropy_hex.as_bytes()),
        "Capsule must carry the actual observed entropy bytes"
    );

    let producer_result = fs::read(state.join("result.json")).expect("producer result");
    let (producer_state, _) = capture_workspace_state(&state).expect("capture producer State");
    let record_digest = record_digest(&bundle.records);
    let result_digest = digest(&producer_result);
    let transcript_digest = digest(&entropy);

    let producer_path = producer.path().to_path_buf();
    drop(producer);
    assert!(
        !producer_path.exists(),
        "producer workspace, CAS, and HOME must be absent before replay"
    );

    let consumer = scratch_dir("entropy-consumer-");
    let restored = consumer.path().join("restored");
    let replayed = ato(consumer.path())
        .args(["internal", "capsule-protocol", "replay"])
        .arg(&bundle_path)
        .arg("--into")
        .arg(&restored)
        .arg("--no-continue")
        .output()
        .expect("replay recorded entropy without producer");
    assert!(
        replayed.status.success(),
        "replay failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&replayed.stdout),
        String::from_utf8_lossy(&replayed.stderr)
    );

    let consumer_state_dir = restored.join("state");
    let consumer_result = fs::read(consumer_state_dir.join("result.json")).unwrap();
    let (consumer_state, _) =
        capture_workspace_state(&consumer_state_dir).expect("capture consumer State");
    assert_eq!(consumer_result, producer_result);
    assert_eq!(consumer_state.state_ref, producer_state.state_ref);
    assert_eq!(digest(&consumer_result), result_digest);

    println!(
        "entropy replay evidence: record_digest={record_digest} final_state_ref={} result_digest={result_digest} transcript_digest={transcript_digest} entropy_batches={ENTROPY_BATCHES} entropy_bytes={ENTROPY_BYTES}",
        producer_state.state_ref
    );
}
