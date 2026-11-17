//! A record file that did not become durable is invisible to the cursor-based
//! recovery, and next_sequence (max+1) reuses the lost sequence number, so the
//! append-only chain silently becomes discontinuous.
//!
//! The lost rename is simulated by deleting records/main/2.json: atomic_create
//! syncs the file but never the parent directory, so the rename that publishes a
//! Record can be lost by a power failure while the runs/active.json cursor
//! (written by atomic_write into a different directory) survives.
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_objects::{
    ActiveRun, Direction, LocalCapsuleRepository, RecordEnvelope, RecordId, RepositoryError,
};
use std::path::PathBuf;

fn reference(c: char) -> ComputationRef {
    ComputationRef::parse(format!("blake3:{}", c.to_string().repeat(64))).unwrap()
}

fn record(stream: &str, before: &ComputationRef, after: &ComputationRef) -> RecordEnvelope {
    RecordEnvelope {
        id: RecordId::new(stream, 0),
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

fn lease(head: &ComputationRef) -> ActiveRun {
    ActiveRun {
        token: "test-token".into(),
        branch: "main".into(),
        branch_base: head.clone(),
        head: head.clone(),
        record_seq: 0,
        pid: 0,
        process_start_time: String::new(),
        process_group: 0,
        boot_session: String::new(),
        status: "starting".into(),
    }
}

fn main() {
    let root = PathBuf::from(std::env::var_os("OUT").expect("OUT is required"))
        .join(format!("record-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let repo = LocalCapsuleRepository::open(&root).unwrap();
    let (c0, c1, c2, c3, c4) = (
        reference('a'),
        reference('b'),
        reference('c'),
        reference('e'),
        reference('f'),
    );
    repo.create_branch("main", &c0, None).unwrap();
    repo.claim_active_run(&lease(&c0)).unwrap();
    let first = repo
        .commit_observation("test-token", &c0, record("main", &c0, &c1))
        .unwrap();
    let second = repo
        .commit_observation("test-token", &c1, record("main", &c1, &c2))
        .unwrap();
    assert_eq!((first.id.seq, second.id.seq), (1, 2));

    // Simulate the lost publication of Record 2.
    let lost = repo.root().join("records/main/2.json");
    assert!(lost.is_file());
    std::fs::remove_file(&lost).unwrap();

    // The cursor still says (head = c2, seq = 2) and the recovery agrees,
    // because it only compares records with seq > cursor and the suffix is empty.
    let recovered = repo.active_run().unwrap().unwrap();
    assert_eq!((recovered.record_seq, recovered.head.clone()), (2, c2.clone()));
    println!("cursor recovery does not notice the missing Record 2");

    // next_sequence = max(existing) + 1 = 2, so the lost sequence number is reused
    // for a Record whose head_before is c2 (not the c1 that Record 1 recorded).
    let reused = repo
        .commit_observation("test-token", &c2, record("main", &c2, &c3))
        .unwrap();
    assert_eq!(reused.id.seq, 2, "sequence 2 was reused");
    let third = repo
        .commit_observation("test-token", &c3, record("main", &c3, &c4))
        .unwrap();
    assert_eq!(third.id.seq, 3);
    assert_eq!(repo.active_run().unwrap().unwrap().record_seq, 3);

    // The cursor path still succeeds...
    assert!(repo.active_run().is_ok());
    // ...while the causal replay of the same branch is now permanently broken.
    let causal = repo.records_for_causal_branch("main", None);
    let mismatch = matches!(causal, Err(RepositoryError::CausalHeadMismatch(_)));
    println!(
        "records_for_causal_branch(main) -> {:?}",
        causal.as_ref().err().map(|error| error.to_string())
    );
    assert!(
        mismatch,
        "the reused sequence must break the causal chain"
    );
    println!("reused sequence 2 with head_before=c2 after Record 1 recorded head_after=c1; branch history is unreplayable");
}
