use std::cell::RefCell;
use std::fs;
use std::path::Path;

use anyhow::Result;

use super::super::state_artifact::{GrantedVolume, StateArtifact, StateWriterGrant};
use super::super::volume::{VolumeStatus, VolumeStore};
use super::*;

const VOLUME: &str = "svol_01M2SVCGR0VP000000000000V1";
const OTHER: &str = "svol_01M2SVCGR0VP000000000000V2";

/// (fence, parent, request id, artifact bytes) of one commit.
type Commit = (u64, Option<String>, String, Vec<u8>);

/// The control plane as the operation's grant describes it.
struct Plane {
    fence: u64,
    volume_ref: String,
    revision_ref: Option<String>,
    artifact: Option<StateArtifact>,
    commits: RefCell<Vec<Commit>>,
}

impl StateArtifactTransport for Plane {
    fn acquire_writer(&self, _state_key: &str) -> Result<StateWriterGrant> {
        Ok(StateWriterGrant {
            revision_ref: self.revision_ref.clone(),
            artifact_digest: self.artifact.as_ref().map(|a| a.digest().to_owned()),
            writer_fence: self.fence,
            volume: Some(GrantedVolume {
                volume_ref: self.volume_ref.clone(),
                status: VolumeStatus::Ready,
            }),
        })
    }
    fn download(&self, _digest: &str) -> Result<Vec<u8>> {
        Ok(self.artifact.as_ref().unwrap().bytes().to_vec())
    }
    fn commit(
        &self,
        _state_key: &str,
        writer_fence: u64,
        parent: Option<&str>,
        request_id: &str,
        artifact: &StateArtifact,
    ) -> Result<String> {
        self.commits.borrow_mut().push((
            writer_fence,
            parent.map(str::to_owned),
            request_id.to_owned(),
            artifact.bytes().to_vec(),
        ));
        Ok("isrev_checkpoint".to_owned())
    }
    fn release_writer(&self, _state_key: &str, _fence: u64) -> Result<()> {
        panic!("a maintenance operation never releases its writer itself")
    }
    fn quarantine_writer(&self, _state_key: &str, _fence: u64, _reason: &str) -> Result<()> {
        panic!("a maintenance operation runs no workload to quarantine")
    }
}

fn plane(volume_ref: &str, fence: u64) -> Plane {
    Plane {
        fence,
        volume_ref: volume_ref.to_owned(),
        revision_ref: None,
        artifact: None,
        commits: RefCell::new(Vec::new()),
    }
}

fn command(operation: VolumeOperationKind, fence: u64) -> VolumeMaintenanceLeaseCommand {
    VolumeMaintenanceLeaseCommand {
        run_id: "run_vop".to_owned(),
        compute_instance_id: "cinst_1".to_owned(),
        operation_id: "vop_01M2SVCGR0VP0000000000000".to_owned(),
        operation,
        state_key: "data".to_owned(),
        volume_ref: VOLUME.to_owned(),
        writer_fence: fence,
    }
}

/// A store with VOLUME holding `value` and OTHER holding "other".
fn store_with(value: &str) -> (tempfile::TempDir, VolumeStore) {
    let root = tempfile::tempdir().unwrap();
    let store = VolumeStore::open(root.path(), "runner-a").unwrap();
    for (volume_ref, contents) in [(VOLUME, value), (OTHER, "other")] {
        let volume = store
            .attach(
                volume_ref,
                1 << 20,
                VolumeStatus::Provisioning,
                None,
                &|_: &Path| Ok(()),
            )
            .unwrap();
        fs::write(volume.data_dir().join("value"), contents).unwrap();
    }
    (root, store)
}

fn read(store: &VolumeStore, volume_ref: &str) -> String {
    fs::read_to_string(store.data_dir(volume_ref).unwrap().join("value")).unwrap()
}

#[test]
fn a_checkpoint_commits_the_volume_from_its_granted_parent() {
    let (_root, store) = store_with("A");
    let mut plane = plane(VOLUME, 7);
    plane.revision_ref = Some("isrev_parent".to_owned());
    perform(&command(VolumeOperationKind::Checkpoint, 7), &store, &plane).unwrap();
    let commits = plane.commits.borrow();
    assert_eq!(commits.len(), 1);
    let (fence, parent, request_id, bytes) = &commits[0];
    assert_eq!(*fence, 7);
    assert_eq!(parent.as_deref(), Some("isrev_parent"));
    assert_eq!(request_id, "vop:vop_01M2SVCGR0VP0000000000000");
    // The committed artifact is exactly the volume's data.
    let packed = pack_state_tree(&store.data_dir(VOLUME).unwrap()).unwrap();
    assert_eq!(bytes, packed.bytes());
    // Reading changes nothing.
    assert_eq!(read(&store, VOLUME), "A");
}

#[test]
fn a_restore_replaces_only_this_volume_with_the_revision() {
    let (_root, store) = store_with("B");
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("value"), "A").unwrap();
    let mut plane = plane(VOLUME, 3);
    plane.revision_ref = Some("isrev_c1".to_owned());
    plane.artifact = Some(pack_state_tree(source.path()).unwrap());
    perform(&command(VolumeOperationKind::Restore, 3), &store, &plane).unwrap();
    assert_eq!(read(&store, VOLUME), "A");
    assert_eq!(read(&store, OTHER), "other");
    assert!(plane.commits.borrow().is_empty());
}

#[test]
fn a_delete_removes_only_this_volume() {
    let (_root, store) = store_with("A");
    perform(
        &command(VolumeOperationKind::Delete, 2),
        &store,
        &plane(VOLUME, 2),
    )
    .unwrap();
    assert!(!store.data_dir(VOLUME).unwrap().exists());
    assert_eq!(read(&store, OTHER), "other");
}

#[test]
fn a_grant_that_disagrees_with_the_lease_touches_nothing() {
    let (_root, store) = store_with("A");
    // Another generation.
    assert!(
        perform(
            &command(VolumeOperationKind::Delete, 2),
            &store,
            &plane(VOLUME, 3)
        )
        .is_err()
    );
    // Another volume.
    assert!(
        perform(
            &command(VolumeOperationKind::Delete, 2),
            &store,
            &plane(OTHER, 2)
        )
        .is_err()
    );
    assert_eq!(read(&store, VOLUME), "A");
    assert_eq!(read(&store, OTHER), "other");
}

#[test]
fn a_volume_that_is_not_here_is_never_recreated_by_maintenance() {
    let root = tempfile::tempdir().unwrap();
    let store = VolumeStore::open(root.path(), "runner-a").unwrap();
    let error = perform(
        &command(VolumeOperationKind::Checkpoint, 1),
        &store,
        &plane(VOLUME, 1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("state_volume_missing"));
    assert!(!store.data_dir(VOLUME).unwrap().exists());
}

#[test]
fn the_command_is_validated_against_its_lease() {
    let command = command(VolumeOperationKind::Checkpoint, 1);
    command.validate("run_vop").unwrap();
    assert!(command.validate("run_other").is_err());
    let mut forged = VolumeMaintenanceLeaseCommand { ..command };
    forged.volume_ref = "svol_../../etc".to_owned();
    assert!(forged.validate("run_vop").is_err());
}
