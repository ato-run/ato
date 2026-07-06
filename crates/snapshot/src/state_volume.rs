//! v1.6 (ato#983) Slice 2: durable state volume lifecycle — host-side backing
//! files for durable, per-service block-device state, attached to Firecracker
//! as writable, non-root drives. Mounting the device inside the guest is a
//! later slice (Slice 3); this module only creates/locates/locks the backing
//! file and builds the drive-attach payload.
//!
//! Identity: a volume's path is keyed by `(owner_scope, state_name)` —
//! **not** by session/run/execution id, and **not** content-addressed like the
//! rootfs cache. The whole point of durable state is that the SAME backing
//! file is found again at the next restore of the same owner+state; a
//! content- or session-keyed path would silently start every run empty.
//!
//! The path is deliberately placed OUTSIDE any directory `stop()` removes
//! (the per-session `overlay_root`, the per-build `build-<pid>` scratch dir)
//! — under `<work_root>/state/...`, a sibling of `<work_root>/rootfs/...`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One durable state volume to attach as a writable, non-root Firecracker
/// drive. `size_mb` sizes the backing file at first creation (a mismatch
/// against an existing file's size is fail-closed — resize is a follow-up).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableVolumeSpec {
    pub state_name: String,
    pub size_mb: u32,
}

/// Deterministic drive_id for the Nth (sorted-by-state_name) volume:
/// `state0`, `state1`, ... — distinct from the `rootfs` drive_id, never
/// `is_root_device`.
pub fn drive_id(index: usize) -> String {
    format!("state{index}")
}

/// A short, deterministic ext4 volume label (max 16 bytes — the on-disk ext4
/// label limit), derived from `(owner_scope, state_name)` so a later slice's
/// guest-side `blkid -L <label>` resolves the SAME device across every
/// restore of the same durable state, and never collides with a different
/// owner/state's label. `"AS"` (Ato State) + 14 hex chars of the digest = 16
/// bytes exactly.
pub fn volume_label(owner_scope: &str, state_name: &str) -> String {
    let digest = blake3::hash(identity_key(owner_scope, state_name).as_bytes());
    format!("AS{}", &digest.to_hex()[..14])
}

fn identity_key(owner_scope: &str, state_name: &str) -> String {
    format!("{owner_scope}\u{0}{state_name}")
}

/// The stable backing-file path for a durable state volume.
pub fn volume_path(work_root: &Path, owner_scope: &str, state_name: &str) -> PathBuf {
    let owner_hash = blake3::hash(owner_scope.as_bytes()).to_hex();
    work_root.join("state").join(&owner_hash.to_string()[..16]).join(format!("{state_name}.img"))
}

/// The lock-file path guarding concurrent writable attach of the same volume
/// (a second build/restore of the SAME owner+state must not attach the same
/// backing file read-write while the first is live — that is state
/// corruption, not a race to tolerate).
pub fn lock_path(work_root: &Path, owner_scope: &str, state_name: &str) -> PathBuf {
    let key = blake3::hash(identity_key(owner_scope, state_name).as_bytes()).to_hex();
    work_root.join("state").join(format!("{}.lock", &key.to_string()[..16]))
}

/// Formats a freshly `set_len`'d file as ext4 with the given label. Real
/// implementation shells out to `mkfs.ext4` (Linux-only — no-op/error on any
/// host without it, fail-closed). Tests inject a fake formatter so the
/// atomic-create/reuse/size-check protocol is verified without needing a
/// real Linux host.
pub trait VolumeFormatter {
    fn format_ext4(&self, path: &Path, label: &str) -> Result<(), String>;
}

/// Production formatter: `mkfs.ext4 -q -F -L <label> <path>`.
pub struct Mkfsext4Formatter;

impl VolumeFormatter for Mkfsext4Formatter {
    fn format_ext4(&self, path: &Path, label: &str) -> Result<(), String> {
        let out = std::process::Command::new("mkfs.ext4")
            .args(["-q", "-F", "-L", label])
            .arg(path)
            .output()
            .map_err(|e| format!("mkfs.ext4 not available on this host: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "mkfs.ext4 failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// Ensure the backing file at `path` exists and is exactly `size_mb` MiB.
///
/// - Missing: create it — a sparse file (`set_len`), formatted ext4, then
///   `rename`d into place atomically (via a `.tmp` sibling) so a crash mid-
///   creation can never promote a half-formatted file; any failure removes
///   the `.tmp` and leaves no partial artifact at `path`.
/// - Existing with the SAME size: reused as-is (never reformatted/truncated
///   — that would destroy the durable data this mechanism exists to keep).
/// - Existing with a DIFFERENT size: fail-closed (resize/migration is an
///   explicit follow-up, not silently handled here).
pub fn ensure_state_volume(
    formatter: &dyn VolumeFormatter,
    path: &Path,
    size_mb: u32,
    label: &str,
) -> Result<(), String> {
    let size_bytes = u64::from(size_mb) * 1024 * 1024;
    if path.exists() {
        let meta = std::fs::metadata(path).map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.len() != size_bytes {
            return Err(format!(
                "state volume {} already exists at {} bytes, but this build requests {size_mb} MiB \
                 ({size_bytes} bytes) — resize/migration is not supported yet; fix the manifest's \
                 size_mb or remove the file to let it be recreated at the new size",
                path.display(),
                meta.len()
            ));
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    let result = (|| -> Result<(), String> {
        let f = std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        f.set_len(size_bytes).map_err(|e| format!("set_len {}: {e}", tmp.display()))?;
        drop(f);
        formatter.format_ext4(&tmp, label)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    commit_or_cleanup(&tmp, path)
}

/// The final atomic-create step: promote `tmp` to `path` via `rename`, or —
/// review fix (ato#990) — clean up `tmp` if the rename itself fails. The doc
/// comment on `ensure_state_volume` promises "any failure removes the .tmp";
/// before this fix that only covered the create/set_len/format failures
/// above it, not this last step. Split out so the rename-failure cleanup is
/// unit-testable directly (constructing a real OS-level rename failure via
/// `ensure_state_volume`'s public entry point isn't possible: its own
/// `path.exists()` check intercepts a directory at `path` first, by design,
/// before ever reaching this step).
fn commit_or_cleanup(tmp: &Path, path: &Path) -> Result<(), String> {
    if let Err(e) = std::fs::rename(tmp, path) {
        let _ = std::fs::remove_file(tmp);
        return Err(format!("rename {} -> {}: {e}", tmp.display(), path.display()));
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Acquire the exclusive per-volume lock (atomic `create_new`). A second
/// concurrent acquire for the SAME path fails closed — two writable attaches
/// of the same backing file is state corruption, not a race to allow.
pub fn acquire_volume_lock(lock_path: &Path) -> Result<(), String> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(format!(
            "state volume busy: another session already holds the lock at {} — a durable volume \
             must not be attached read-write to two VMs at once",
            lock_path.display()
        )),
        Err(e) => Err(format!("acquire lock {}: {e}", lock_path.display())),
    }
}

/// Release a previously-acquired volume lock. Idempotent (a missing lock
/// file is not an error — matches the existing tap/netns lock's
/// `release_lock` convention in `firecracker.rs`).
pub fn release_volume_lock(lock_path: &Path) {
    let _ = std::fs::remove_file(lock_path);
}

/// Releases every held durable-state-volume lock when dropped.
#[derive(Debug)]
pub struct VolumeLockGuard(pub Vec<PathBuf>);
impl Drop for VolumeLockGuard {
    fn drop(&mut self) {
        for p in &self.0 {
            release_volume_lock(p);
        }
    }
}

/// Ensure + lock every volume in `volumes`, sorted by `state_name` for
/// deterministic `drive_id` assignment, returning the ordered backing-file
/// paths and a guard holding every lock acquired.
///
/// The guard is built INCREMENTALLY: each lock is pushed into it the instant
/// it is acquired, not collected into a plain `Vec` and wrapped in a guard
/// only after the whole loop finishes. That distinction is the fix for
/// ato#990's review finding — if a LATER volume's `acquire_volume_lock` or
/// `ensure_state_volume` fails and this function returns early via `?`, the
/// (local, already-partially-filled) guard goes out of scope right there and
/// its `Drop` releases every lock already acquired. Building the guard only
/// at the end would have left an earlier volume's lock held forever whenever
/// a later one failed — exactly the leak this shape prevents by construction
/// rather than by remembering to clean up on every error path by hand.
///
/// Shared by both `build_ready_state` (guard dropped when that call returns —
/// a build is a temporary boot-to-snapshot) and `restore` (the returned guard
/// is `mem::forget`-ed on success so the locks survive for the live session,
/// released later by `stop()` from paths recorded in `.fc-session.json`).
pub fn prepare_volumes(
    formatter: &dyn VolumeFormatter,
    work_root: &Path,
    owner_scope: &str,
    volumes: &[DurableVolumeSpec],
) -> Result<(Vec<PathBuf>, VolumeLockGuard), String> {
    let mut paths = Vec::new();
    let mut guard = VolumeLockGuard(Vec::new());
    let mut sorted = volumes.to_vec();
    sorted.sort_by(|a, b| a.state_name.cmp(&b.state_name));
    for vol in &sorted {
        let vpath = volume_path(work_root, owner_scope, &vol.state_name);
        let lpath = lock_path(work_root, owner_scope, &vol.state_name);
        acquire_volume_lock(&lpath)?;
        guard.0.push(lpath);
        let label = volume_label(owner_scope, &vol.state_name);
        ensure_state_volume(formatter, &vpath, vol.size_mb, &label)?;
        paths.push(vpath);
    }
    Ok((paths, guard))
}

/// The `PUT /drives/<id>` JSON payload for each volume, in the SAME
/// deterministic order as `drive_id()` — a pure function so the drive-list
/// shape is unit-testable without a real Firecracker process.
pub fn state_drive_configs(paths_in_order: &[PathBuf]) -> Vec<serde_json::Value> {
    paths_in_order
        .iter()
        .enumerate()
        .map(|(i, path)| {
            serde_json::json!({
                "drive_id": drive_id(i),
                "path_on_host": path.to_string_lossy(),
                "is_root_device": false,
                "is_read_only": false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records format calls instead of shelling out to `mkfs.ext4` (which
    /// doesn't exist on non-Linux dev/CI hosts) — lets the atomic-create /
    /// reuse / size-mismatch protocol be tested everywhere.
    #[derive(Default)]
    struct FakeFormatter {
        calls: Mutex<Vec<(PathBuf, String)>>,
        fail: bool,
    }
    impl VolumeFormatter for FakeFormatter {
        fn format_ext4(&self, path: &Path, label: &str) -> Result<(), String> {
            if self.fail {
                return Err("fake formatter: injected failure".to_string());
            }
            self.calls.lock().unwrap().push((path.to_path_buf(), label.to_string()));
            Ok(())
        }
    }

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tmpdir")
    }

    #[test]
    fn volume_path_is_stable_for_the_same_identity_and_differs_across_identities() {
        let root = Path::new("/work");
        let a1 = volume_path(root, "owner-a", "dbdata");
        let a2 = volume_path(root, "owner-a", "dbdata");
        assert_eq!(a1, a2, "same owner+state must resolve to the same path every time");

        let b = volume_path(root, "owner-b", "dbdata");
        assert_ne!(a1, b, "different owner must resolve to a different path");

        let other_state = volume_path(root, "owner-a", "cache");
        assert_ne!(a1, other_state, "different state_name must resolve to a different path");
    }

    #[test]
    fn volume_path_is_not_keyed_by_anything_run_scoped() {
        // The whole point: no session_id/run_id/execution_id parameter exists on
        // volume_path()'s signature at all — this test exists to make that
        // contract explicit and break loudly if the signature ever grows one.
        let root = Path::new("/work");
        let p1 = volume_path(root, "owner-a", "dbdata");
        let p2 = volume_path(root, "owner-a", "dbdata");
        assert_eq!(p1, p2);
    }

    #[test]
    fn volume_label_is_16_bytes_and_deterministic() {
        let l1 = volume_label("owner-a", "dbdata");
        let l2 = volume_label("owner-a", "dbdata");
        assert_eq!(l1, l2);
        assert_eq!(l1.len(), 16, "ext4 label must fit the 16-byte on-disk limit");
        assert!(l1.starts_with("AS"));
        assert_ne!(l1, volume_label("owner-b", "dbdata"));
    }

    #[test]
    fn drive_ids_are_deterministic_and_distinct_from_rootfs() {
        assert_eq!(drive_id(0), "state0");
        assert_eq!(drive_id(1), "state1");
        assert_ne!(drive_id(0), "rootfs");
    }

    #[test]
    fn missing_volume_is_created_at_the_requested_size() {
        let dir = tmpdir();
        let path = dir.path().join("dbdata.img");
        let fmt = FakeFormatter::default();
        ensure_state_volume(&fmt, &path, 64, "ASlabel").unwrap();
        assert!(path.exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024 * 1024);
        assert_eq!(fmt.calls.lock().unwrap().len(), 1);
        assert!(!path.with_file_name("dbdata.img.tmp").exists(), "tmp file must not remain");
    }

    #[test]
    fn existing_correct_size_volume_is_reused_without_reformatting() {
        let dir = tmpdir();
        let path = dir.path().join("dbdata.img");
        let fmt = FakeFormatter::default();
        ensure_state_volume(&fmt, &path, 64, "ASlabel").unwrap();
        assert_eq!(fmt.calls.lock().unwrap().len(), 1);

        // Write a marker at offset 0 WITHOUT truncating (mirrors how ext4 file
        // content would be modified in place) so we can prove reuse survives it.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.write_all(b"durable-marker").unwrap();
        }
        // Second ensure with the SAME size must not reformat (data survives).
        ensure_state_volume(&fmt, &path, 64, "ASlabel").unwrap();
        assert_eq!(fmt.calls.lock().unwrap().len(), 1, "reuse must not call format again");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024 * 1024, "size preserved");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..14], b"durable-marker", "reuse must not truncate/reformat existing data");
    }

    #[test]
    fn existing_wrong_size_volume_is_fail_closed_not_resized() {
        let dir = tmpdir();
        let path = dir.path().join("dbdata.img");
        let fmt = FakeFormatter::default();
        ensure_state_volume(&fmt, &path, 64, "ASlabel").unwrap();

        let err = ensure_state_volume(&fmt, &path, 128, "ASlabel").unwrap_err();
        assert!(err.contains("resize/migration is not supported"), "{err}");
        // Untouched: still the original size, not silently resized.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024 * 1024);
    }

    #[test]
    fn create_failure_cleans_the_tmp_file_and_leaves_no_partial_artifact() {
        let dir = tmpdir();
        let path = dir.path().join("dbdata.img");
        let fmt = FakeFormatter { fail: true, ..Default::default() };
        let err = ensure_state_volume(&fmt, &path, 64, "ASlabel").unwrap_err();
        assert!(err.contains("injected failure"));
        assert!(!path.exists(), "no partial .img must be promoted on format failure");
        assert!(!path.with_file_name("dbdata.img.tmp").exists(), "tmp must be cleaned up");
    }

    #[test]
    fn rename_failure_also_cleans_the_tmp_file() {
        // Review fix (ato#990): `commit_or_cleanup` is the extracted final
        // rename-or-cleanup step. Force a real OS-level rename failure by
        // making the destination an existing DIRECTORY (renaming a regular
        // file onto a directory fails on every platform) — a real tmp FILE is
        // created first so this proves the cleanup, not just the error path.
        let dir = tmpdir();
        let tmp = dir.path().join("dbdata.img.tmp");
        std::fs::write(&tmp, b"contents").unwrap();
        let path = dir.path().join("dbdata.img");
        std::fs::create_dir_all(&path).unwrap(); // destination exists as a directory

        let err = commit_or_cleanup(&tmp, &path).unwrap_err();
        assert!(err.contains("rename"), "{err}");
        assert!(!tmp.exists(), "tmp must not remain after a rename failure");
        assert!(path.is_dir(), "the pre-existing destination directory is untouched");
    }

    #[test]
    fn zero_size_mb_produces_a_zero_length_file_bounds_are_the_manifest_layers_job() {
        // state_volume.rs trusts its caller (the shared capsule-crate bounds
        // check already rejects size_mb=0 at the manifest/builder layer); this
        // module only asserts it does not itself silently substitute a default.
        let dir = tmpdir();
        let path = dir.path().join("dbdata.img");
        let fmt = FakeFormatter::default();
        ensure_state_volume(&fmt, &path, 0, "ASlabel").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn volume_lock_rejects_a_second_concurrent_acquire() {
        let dir = tmpdir();
        let lock = dir.path().join("dbdata.lock");
        acquire_volume_lock(&lock).unwrap();
        let err = acquire_volume_lock(&lock).unwrap_err();
        assert!(err.contains("busy"), "{err}");
        release_volume_lock(&lock);
        // After release, a fresh acquire succeeds again.
        acquire_volume_lock(&lock).unwrap();
        release_volume_lock(&lock);
    }

    #[test]
    fn volume_lock_release_is_idempotent() {
        let dir = tmpdir();
        let lock = dir.path().join("dbdata.lock");
        release_volume_lock(&lock); // never acquired — must not panic/error
        acquire_volume_lock(&lock).unwrap();
        release_volume_lock(&lock);
        release_volume_lock(&lock); // double release — must not panic/error
    }

    #[test]
    fn prepare_volumes_releases_earlier_locks_when_a_later_one_fails() {
        // Review fix (ato#990) regression test: with 2 volumes, the FIRST
        // volume's lock acquire succeeds; the SECOND's fails because its lock
        // file already exists (simulating another session already holding
        // it). `prepare_volumes` must return an error AND the first volume's
        // lock file must be gone — not leaked because the guard was built
        // "too late" (only after the whole loop, from a plain Vec).
        let dir = tmpdir();
        let work_root = dir.path().to_path_buf();
        let fmt = FakeFormatter::default();
        let volumes = vec![
            DurableVolumeSpec { state_name: "aaa".to_string(), size_mb: 64 },
            DurableVolumeSpec { state_name: "bbb".to_string(), size_mb: 64 },
        ];
        // Pre-acquire "bbb"'s lock (sorted after "aaa") so its acquire fails.
        let bbb_lock = lock_path(&work_root, "owner-x", "bbb");
        acquire_volume_lock(&bbb_lock).unwrap();

        let aaa_lock = lock_path(&work_root, "owner-x", "aaa");
        assert!(!aaa_lock.exists(), "sanity: aaa's lock is not held yet");

        let err = prepare_volumes(&fmt, &work_root, "owner-x", &volumes).unwrap_err();
        assert!(err.contains("busy"), "{err}");
        assert!(!aaa_lock.exists(), "aaa's lock (acquired before bbb failed) must be released, not leaked");

        release_volume_lock(&bbb_lock);
    }

    #[test]
    fn prepare_volumes_holds_every_lock_on_success_until_the_guard_drops() {
        let dir = tmpdir();
        let work_root = dir.path().to_path_buf();
        let fmt = FakeFormatter::default();
        let volumes = vec![
            DurableVolumeSpec { state_name: "aaa".to_string(), size_mb: 64 },
            DurableVolumeSpec { state_name: "bbb".to_string(), size_mb: 64 },
        ];
        let aaa_lock = lock_path(&work_root, "owner-x", "aaa");
        let bbb_lock = lock_path(&work_root, "owner-x", "bbb");

        let (paths, guard) = prepare_volumes(&fmt, &work_root, "owner-x", &volumes).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(aaa_lock.exists() && bbb_lock.exists(), "both locks held while the guard is alive");
        drop(guard);
        assert!(!aaa_lock.exists() && !bbb_lock.exists(), "both locks released once the guard drops");
    }

    #[test]
    fn state_drive_configs_is_empty_for_no_volumes() {
        assert!(state_drive_configs(&[]).is_empty());
    }

    #[test]
    fn state_drive_configs_are_never_root_and_never_read_only() {
        let paths = vec![PathBuf::from("/work/state/a/dbdata.img"), PathBuf::from("/work/state/a/cache.img")];
        let cfgs = state_drive_configs(&paths);
        assert_eq!(cfgs.len(), 2);
        assert_eq!(cfgs[0]["drive_id"], "state0");
        assert_eq!(cfgs[1]["drive_id"], "state1");
        for c in &cfgs {
            assert_eq!(c["is_root_device"], false);
            assert_eq!(c["is_read_only"], false);
        }
        assert_eq!(cfgs[0]["path_on_host"], "/work/state/a/dbdata.img");
    }
}
