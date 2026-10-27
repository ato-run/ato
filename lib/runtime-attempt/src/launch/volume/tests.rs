use std::cell::Cell;

use super::*;

const VOLUME: &str = "svol_01M2SVCGR0VP000000000000V1";
const OTHER: &str = "svol_01M2SVCGR0VP000000000000V2";
const CAPACITY: u64 = 1024 * 1024;

fn store(root: &Path) -> VolumeStore {
    VolumeStore::open(root, "runner-a").expect("store opens")
}

fn no_seed(_: &Path) -> Result<()> {
    Ok(())
}

fn attach(store: &VolumeStore, status: VolumeStatus) -> Result<AttachedVolume, VolumeError> {
    store.attach(VOLUME, CAPACITY, status, None, &no_seed)
}

#[test]
fn a_new_volume_is_provisioned_once_and_then_reused_as_is() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let seeded = Cell::new(0);
    let seed = |data: &Path| {
        seeded.set(seeded.get() + 1);
        fs::create_dir_all(data)?;
        fs::write(data.join("app.db"), "seed")?;
        Ok(())
    };

    let first = store
        .attach(
            VOLUME,
            CAPACITY,
            VolumeStatus::Provisioning,
            Some("isrev_1"),
            &seed,
        )
        .unwrap();
    assert!(first.provisioned);
    assert_eq!(
        fs::read_to_string(first.data_dir().join("app.db")).unwrap(),
        "seed"
    );
    // The app writes in place; the Run ends.
    fs::write(first.data_dir().join("app.db"), "B").unwrap();
    drop(first);

    // A retry that still says "provisioning" finds the finished volume and
    // does not seed over it.
    let again = store
        .attach(
            VOLUME,
            CAPACITY,
            VolumeStatus::Provisioning,
            Some("isrev_1"),
            &seed,
        )
        .unwrap();
    assert!(!again.provisioned);
    drop(again);
    let ready = attach(&store, VolumeStatus::Ready).unwrap();
    assert_eq!(
        fs::read_to_string(ready.data_dir().join("app.db")).unwrap(),
        "B"
    );
    assert_eq!(seeded.get(), 1, "a volume is seeded exactly once");
}

#[test]
fn a_ready_volume_that_is_gone_is_missing_and_never_reseeded() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let seeded = Cell::new(false);
    let seed = |data: &Path| {
        seeded.set(true);
        fs::create_dir_all(data)?;
        Ok(())
    };
    let error = store
        .attach(
            VOLUME,
            CAPACITY,
            VolumeStatus::Ready,
            Some("isrev_1"),
            &seed,
        )
        .unwrap_err();
    assert_eq!(error.code(), "state_volume_missing");
    assert!(!seeded.get());
    assert!(!store.data_dir(VOLUME).unwrap().exists());

    let error = attach(&store, VolumeStatus::Missing).unwrap_err();
    assert_eq!(error.code(), "state_volume_missing");
}

#[test]
fn a_crash_mid_provisioning_leaves_nothing_a_retry_trusts() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let failing = |data: &Path| {
        fs::create_dir_all(data)?;
        fs::write(data.join("partial"), "x")?;
        anyhow::bail!("seed download failed")
    };
    let error = store
        .attach(
            VOLUME,
            CAPACITY,
            VolumeStatus::Provisioning,
            Some("isrev_1"),
            &failing,
        )
        .unwrap_err();
    assert_eq!(error.code(), "state_volume_io");
    assert!(!store.data_dir(VOLUME).unwrap().exists());

    // The retry clears its own staging directory and completes.
    let volume = attach(&store, VolumeStatus::Provisioning).unwrap();
    assert!(volume.provisioned);
    assert!(!volume.data_dir().join("partial").exists());
    let leftovers = fs::read_dir(root.path().join("runner-a/volumes"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn a_second_attachment_on_the_same_host_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let held = attach(&store, VolumeStatus::Provisioning).unwrap();
    let error = attach(&store, VolumeStatus::Ready).unwrap_err();
    assert_eq!(error.code(), "state_volume_locked");
    // Another volume is independent.
    store
        .attach(OTHER, CAPACITY, VolumeStatus::Provisioning, None, &no_seed)
        .unwrap();
    drop(held);
    attach(&store, VolumeStatus::Ready).unwrap();
}

#[test]
fn forged_references_never_become_paths() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    for forged in [
        "svol_../../../etc",
        "../svol_01M2SVCGR0VP000000000000V1",
        "svol_01M2SVCGR0VP000000000000V1/../x",
        "svol_01m2svcgr0vp000000000000v1",
    ] {
        let error = store
            .attach(forged, CAPACITY, VolumeStatus::Provisioning, None, &no_seed)
            .unwrap_err();
        assert_eq!(error.code(), "state_volume_invalid", "{forged}");
    }
    assert!(VolumeStore::open(root.path(), "../runner").is_err());
    assert!(VolumeStore::open(root.path(), "").is_err());
}

#[test]
fn another_runners_volume_is_not_this_one() {
    let root = tempfile::tempdir().unwrap();
    let a = store(root.path());
    drop(attach(&a, VolumeStatus::Provisioning).unwrap());
    // Runner B on the same root has its own namespace: A's volume is not there.
    let b = VolumeStore::open(root.path(), "runner-b").unwrap();
    assert_eq!(
        attach(&b, VolumeStatus::Ready).unwrap_err().code(),
        "state_volume_missing"
    );
    // And a directory copied across keeps its owner's metadata, so B refuses it.
    let from = root.path().join("runner-a/volumes").join(VOLUME);
    let to = root.path().join("runner-b/volumes").join(VOLUME);
    fs::create_dir_all(to.join("data")).unwrap();
    fs::copy(from.join("metadata.json"), to.join("metadata.json")).unwrap();
    assert_eq!(
        attach(&b, VolumeStatus::Ready).unwrap_err().code(),
        "state_volume_missing"
    );
}

#[test]
fn a_volume_over_its_capacity_is_not_started() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let volume = attach(&store, VolumeStatus::Provisioning).unwrap();
    fs::write(
        volume.data_dir().join("big"),
        vec![1u8; 2 * CAPACITY as usize],
    )
    .unwrap();
    File::open(volume.data_dir().join("big"))
        .unwrap()
        .sync_all()
        .unwrap();
    assert!(volume.usage().unwrap() > CAPACITY);
    drop(volume);
    assert_eq!(
        attach(&store, VolumeStatus::Ready).unwrap_err().code(),
        "state_volume_capacity_exceeded"
    );
}

#[test]
fn a_volume_the_disk_cannot_hold_is_not_started() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let error = store
        .attach(
            VOLUME,
            u64::MAX / 2,
            VolumeStatus::Provisioning,
            None,
            &no_seed,
        )
        .unwrap_err();
    assert_eq!(error.code(), "state_volume_insufficient_space");
    // Refused before anything was created.
    assert!(!store.data_dir(VOLUME).unwrap().exists());
}

#[test]
fn maintenance_never_provisions_a_volume_that_is_not_here() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    assert_eq!(
        store.open_existing(VOLUME).unwrap_err().code(),
        "state_volume_missing"
    );
    assert!(!store.data_dir(VOLUME).unwrap().exists());
}

#[test]
fn a_replacement_is_all_or_nothing() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let volume = attach(&store, VolumeStatus::Provisioning).unwrap();
    fs::write(volume.data_dir().join("app.db"), "B").unwrap();
    drop(volume);

    // A fill that fails part-way leaves the live data exactly as it was.
    let volume = store.open_existing(VOLUME).unwrap();
    let failing = |staging: &Path| {
        fs::create_dir_all(staging)?;
        fs::write(staging.join("app.db"), "partial")?;
        anyhow::bail!("download failed")
    };
    assert!(store.replace_data(&volume, &failing).is_err());
    assert_eq!(
        fs::read_to_string(volume.data_dir().join("app.db")).unwrap(),
        "B"
    );
    drop(volume);

    // A complete fill replaces it, and the old tree is gone.
    let volume = store.open_existing(VOLUME).unwrap();
    let restore = |staging: &Path| {
        fs::create_dir_all(staging)?;
        fs::write(staging.join("app.db"), "A")?;
        Ok(())
    };
    let result = store.replace_data(&volume, &restore);
    if !cfg!(unix) {
        let error = result.unwrap_err();
        assert!(format!("{error}").contains("atomic volume replacement is unavailable"));
        assert_eq!(
            fs::read_to_string(volume.data_dir().join("app.db")).unwrap(),
            "B"
        );
        return;
    }
    result.unwrap();
    assert_eq!(
        fs::read_to_string(volume.data_dir().join("app.db")).unwrap(),
        "A"
    );
    let leftovers: Vec<_> = fs::read_dir(volume.data_dir().parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers.len(), 2, "{leftovers:?}"); // metadata.json + data
}

#[test]
fn a_delete_removes_only_its_own_volume() {
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    drop(attach(&store, VolumeStatus::Provisioning).unwrap());
    let other = store
        .attach(OTHER, CAPACITY, VolumeStatus::Provisioning, None, &no_seed)
        .unwrap();
    fs::write(other.data_dir().join("keep"), "x").unwrap();
    drop(other);

    let volume = store.open_existing(VOLUME).unwrap();
    store.delete(volume).unwrap();
    assert_eq!(
        store.open_existing(VOLUME).unwrap_err().code(),
        "state_volume_missing"
    );
    let remaining: Vec<_> = fs::read_dir(root.path().join("runner-a/volumes"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(remaining, vec![OTHER.to_owned()]);
    let other = store.open_existing(OTHER).unwrap();
    assert_eq!(
        fs::read_to_string(other.data_dir().join("keep")).unwrap(),
        "x"
    );
}

/// A candidate volume is seeded from a packed state artifact. This is the #1572
/// P1-A/P1-B path for memos: a stopped app's WAL-mode database is checkpointed
/// into a revision, and the revision is unpacked into a NEW volume on another
/// Runner, which must read back every committed row.
#[test]
fn a_new_volume_is_seeded_from_a_packed_wal_state_artifact() {
    use crate::launch::state_artifact::{pack_state_tree, sqlite_wal, unpack_state_tree};

    let Some(sqlite3) = sqlite_wal::cli() else {
        eprintln!(
            "skipping a_new_volume_is_seeded_from_a_packed_wal_state_artifact: this host has no sqlite3 CLI"
        );
        return;
    };

    // The seed revision the control plane would hand this Runner.
    let source = tempfile::tempdir().unwrap();
    sqlite_wal::create(
        &sqlite3,
        &source.path().join("memos_prod.db"),
        &[("m1", "hello"), ("m2", "world")],
    );
    assert!(
        !fs::read(source.path().join("memos_prod.db-wal"))
            .unwrap()
            .is_empty(),
        "the seed revision must carry its rows in the WAL"
    );
    let artifact = pack_state_tree(source.path()).expect("packs");

    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());
    let seeded = Cell::new(0);
    let seed = |data: &Path| {
        seeded.set(seeded.get() + 1);
        unpack_state_tree(artifact.bytes(), artifact.digest(), data)
    };

    // A NEW volume, still Provisioning, is filled exactly once.
    let volume = store
        .attach(
            VOLUME,
            CAPACITY,
            VolumeStatus::Provisioning,
            Some("isrev_1"),
            &seed,
        )
        .unwrap();
    assert!(volume.provisioned);
    assert_eq!(seeded.get(), 1);
    let database = volume.data_dir().join("memos_prod.db");
    assert_eq!(sqlite_wal::rows(&sqlite3, &database), "m1|hello\nm2|world");
    assert_eq!(sqlite_wal::integrity(&sqlite3, &database), "ok");
    drop(volume);

    // A second attachment, now Ready, must NOT seed over what the volume holds:
    // an existing volume is newer than any revision.
    let reseeded = Cell::new(false);
    let must_not_seed = |_: &Path| -> Result<()> {
        reseeded.set(true);
        Ok(())
    };
    let ready = store
        .attach(
            VOLUME,
            CAPACITY,
            VolumeStatus::Ready,
            Some("isrev_1"),
            &must_not_seed,
        )
        .unwrap();
    assert!(!ready.provisioned);
    assert!(
        !reseeded.get(),
        "a volume that already exists must never be seeded again"
    );
    assert_eq!(
        sqlite_wal::rows(&sqlite3, &ready.data_dir().join("memos_prod.db")),
        "m1|hello\nm2|world"
    );
    drop(ready);

    // Maintenance (checkpoint, restore, delete) opens the same bytes: this is
    // the read path a later restore of the candidate volume takes.
    let existing = store.open_existing(VOLUME).unwrap();
    assert_eq!(
        sqlite_wal::rows(&sqlite3, &existing.data_dir().join("memos_prod.db")),
        "m1|hello\nm2|world"
    );
}
