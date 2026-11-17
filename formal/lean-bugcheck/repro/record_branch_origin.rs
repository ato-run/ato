//! create_branch publishes refs/origins/<branch>.json before refs/heads/<branch>.
//! The two atomic_create calls are not one step, so a failure in the second one
//! leaves an origin for a branch that does not exist. The orphan origin then
//! blocks every later create_branch for that name (atomic_create is
//! no-overwrite), and a later create_branch without an origin silently inherits
//! the stale anchor, which makes the new branch's causal history unreplayable.
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId};
use ato_objects::{
    BranchOrigin, Direction, LocalCapsuleRepository, RecordEnvelope, RecordId, RepositoryError,
};
use std::os::unix::fs::PermissionsExt;
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

fn main() {
    let root = PathBuf::from(std::env::var_os("OUT").expect("OUT is required"))
        .join(format!("record-origin-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let repo = LocalCapsuleRepository::open(&root).unwrap();
    let (c0, c1, c2, c3) = (
        reference('a'),
        reference('b'),
        reference('c'),
        reference('e'),
    );
    repo.create_branch("main", &c0, None).unwrap();
    let origin = |computation: &ComputationRef| BranchOrigin {
        computation: computation.clone(),
        parent_record: None,
    };

    // The second atomic_create (refs/heads) fails; the first (refs/origins) stays.
    let heads = repo.root().join("refs/heads");
    let previous_mode = std::fs::metadata(&heads).unwrap().permissions().mode();
    std::fs::set_permissions(&heads, std::fs::Permissions::from_mode(0o555)).unwrap();
    let failed = repo.create_branch("feature", &c1, Some(&origin(&c1)));
    std::fs::set_permissions(&heads, std::fs::Permissions::from_mode(previous_mode)).unwrap();
    println!(
        "create_branch with an unwritable refs/heads -> {:?}",
        failed.as_ref().err().map(|error| error.to_string())
    );
    assert!(failed.is_err());

    // The orphan origin is observable and the branch does not exist.
    assert_eq!(repo.head("feature").unwrap(), None);
    assert_eq!(repo.branch_origin("feature").unwrap(), Some(origin(&c1)));
    println!("refs/origins/feature.json exists while refs/heads/feature does not");

    // Retrying the same call can never succeed: atomic_create refuses to replace.
    let retry = repo
        .create_branch("feature", &c1, Some(&origin(&c1)))
        .expect_err("the orphan origin blocks the retry");
    println!("retry -> {:?}", retry.to_string());
    assert!(matches!(&retry, RepositoryError::Io(error) if error.kind() == std::io::ErrorKind::AlreadyExists));

    // Dropping the origin argument does create the branch, but the stale origin is
    // still reported for it, so the branch is anchored at a computation it never had.
    repo.create_branch("feature", &c2, None).unwrap();
    assert_eq!(repo.branch_origin("feature").unwrap(), Some(origin(&c1)));
    repo.append_record(record("feature", &c2, &c3)).unwrap();
    let causal = repo.records_for_causal_branch("feature", None);
    println!(
        "records_for_causal_branch(feature) -> {:?}",
        causal.as_ref().err().map(|error| error.to_string())
    );
    assert!(matches!(causal, Err(RepositoryError::CausalHeadMismatch(_))));
}
