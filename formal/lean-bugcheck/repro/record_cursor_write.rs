//! commit_observation writes the Record first and the Run cursor second. When the
//! cursor write fails the Record is already durable, but the ObservationSink keeps
//! its old cached head, so every later emit is rejected with ActiveRunConflict.
//! The failure is injected by making .capsule/runs unwritable (records/ stays
//! writable), which is what a full disk or a transient EACCES looks like.
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_objects::{
    ActiveRun, Direction, LocalCapsuleRepository, RecordEnvelope, RecordId, RepositoryError,
};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn reference(c: char) -> ComputationRef {
    ComputationRef::parse(format!("blake3:{}", c.to_string().repeat(64))).unwrap()
}

fn record(before: &ComputationRef, after: &ComputationRef) -> RecordEnvelope {
    RecordEnvelope {
        id: RecordId::new("main", 0),
        adapter_id: "ato.workspace@1".into(),
        protocol_id: ProtocolId::parse("ato.workspace@1").unwrap(),
        port_id: PortId::parse("workspace.main").unwrap(),
        direction: Direction::Inbound,
        payload_ref: ContentRef::parse(format!("blake3:{}", "d".repeat(64))).unwrap(),
        head_before: before.clone(),
        head_after: after.clone(),
        caused_by: vec![],
        observed_at: "2030-01-01T00:00:00Z".into(),
    }
}

fn main() {
    let root = PathBuf::from(std::env::var_os("OUT").expect("OUT is required"))
        .join(format!("record-cursor-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let repo = LocalCapsuleRepository::open(&root).unwrap();
    let (c0, c1, c2, c3) = (
        reference('a'),
        reference('b'),
        reference('c'),
        reference('e'),
    );
    repo.create_branch("main", &c0, None).unwrap();
    repo.claim_active_run(&ActiveRun {
        token: "test-token".into(),
        branch: "main".into(),
        branch_base: c0.clone(),
        head: c0.clone(),
        record_seq: 0,
        pid: 0,
        process_start_time: String::new(),
        process_group: 0,
        boot_session: String::new(),
        status: "starting".into(),
    })
    .unwrap();
    repo.commit_observation("test-token", &c0, record(&c0, &c1))
        .unwrap();

    let runs = repo.root().join("runs");
    let previous_mode = std::fs::metadata(&runs).unwrap().permissions().mode();
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = repo.commit_observation("test-token", &c1, record(&c1, &c2));
    std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(previous_mode)).unwrap();
    println!(
        "commit with an unwritable cursor -> {:?}",
        failed.as_ref().err().map(|error| error.to_string())
    );
    assert!(failed.is_err(), "the cursor write must fail");

    // The Record is already durable and the recovery already sees the new head.
    let stream = repo.records_for_stream("main", None).unwrap();
    assert_eq!(stream.len(), 2);
    assert_eq!(stream[1].head_after, c2);
    let recovered = repo.active_run().unwrap().unwrap();
    assert_eq!(recovered.head, c2);

    // The sink's cached head is still c1, so every retry is rejected although the
    // caller does nothing wrong: the API returned an error for a committed Record.
    for retry in [record(&c1, &c2), record(&c1, &c3)] {
        let error = repo
            .commit_observation("test-token", &c1, retry)
            .expect_err("the stale cached head must be rejected");
        assert!(matches!(error, RepositoryError::ActiveRunConflict { .. }));
    }
    println!("two retries with the cached head c1 -> ActiveRunConflict");

    // Only an explicit re-read of the cursor makes progress again.
    repo.commit_observation("test-token", &c2, record(&c2, &c3))
        .unwrap();
    println!("re-reading active_run() restores progress; the sink has no such path");
}
