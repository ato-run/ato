//! Reproduce the public repository API accepting a discontinuous Record.
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_objects::{
    ActiveRun, Direction, LocalCapsuleRepository, RecordEnvelope, RecordId, RepositoryError,
};
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
    let root =
        PathBuf::from(std::env::var_os("OUT").expect("OUT is required")).join("record-repro");
    std::fs::create_dir(&root).unwrap();
    let repo = LocalCapsuleRepository::open(&root).unwrap();
    let (c0, c1, c2, wrong) = (
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
    repo.commit_observation("test-token", &c1, record(&wrong, &c2))
        .expect("the inconsistent record is currently accepted");
    assert_eq!(repo.active_run().unwrap().unwrap().head, c2);
    assert!(matches!(
        repo.records_for_causal_branch("main", None),
        Err(RepositoryError::CausalHeadMismatch(_))
    ));
    println!("accepted discontinuous record; causal history now fails validation");
}
