//! v1.6 (ato#983) Slice 3: mounts durable state block devices (attached by the
//! host in Slice 2) at their declared target paths, BEFORE any service starts.
//!
//! The device is resolved by its ext4 FILESYSTEM LABEL (`blkid -L <label>`),
//! never by a `/dev/vdN` index — device enumeration order inside the guest is
//! not a contract Slice 2 makes. All mounting happens once, at agent boot,
//! for the whole VM (not tied to a particular service's lifecycle).
//!
//! Mirrors the host-side `state_volume.rs` split: mount-PLANNING (target
//! validation, ordering) is pure and unit-tested everywhere; the actual
//! `blkid`/`mount` shell-outs are Linux-only and injected behind a trait so
//! tests never need a real Linux mount syscall.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One durable state volume to mount before any service starts. VM-wide, not
/// tied to a service — holds no secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSpec {
    pub state_name: String,
    /// Absolute in-guest mount path. Already validated + lexically normalized
    /// under `/ato/state/` at build time (`capsule::...::validate_and_normalize_state_mount_target`)
    /// — re-validated here anyway (defense in depth: this agent never trusts
    /// a config it did not produce itself).
    pub target: String,
    /// ext4 label the device is resolved by.
    pub fs_label: String,
    /// Diagnostic only (not needed to mount) — logged to correlate with the
    /// host-side attach.
    #[serde(default)]
    pub drive_id: String,
    #[serde(default)]
    pub size_mb: u32,
}

/// Resolves a block device by filesystem label and mounts it. Split from the
/// pure planning logic below so tests never need a real Linux mount syscall
/// or the `blkid` binary — only `RealVolumeMounter` (used in production) is
/// Linux-only.
pub trait VolumeMounter {
    fn resolve_device(&self, fs_label: &str) -> Result<PathBuf, String>;
    fn mount(&self, device: &Path, target: &Path) -> Result<(), String>;
    /// Best-effort: `sync` then `umount`. Called on shutdown; a failure is
    /// logged, never fatal (VM termination proceeds regardless).
    fn sync_and_umount(&self, target: &Path) -> Result<(), String>;
}

/// Production mounter: `blkid -L <label>` to resolve the device, then
/// `mount -o rw,nodev,nosuid,noatime <device> <target>` (after `mkdir -p
/// target`). `noexec` is deliberately NOT included — an app may legitimately
/// keep executables/plugin caches under its durable state dir; adding
/// `noexec` is a policy option left for a follow-up, not a silent default.
pub struct RealVolumeMounter;

const MOUNT_OPTIONS: &str = "rw,nodev,nosuid,noatime";

impl VolumeMounter for RealVolumeMounter {
    fn resolve_device(&self, fs_label: &str) -> Result<PathBuf, String> {
        let out = std::process::Command::new("blkid")
            .args(["-L", fs_label])
            .output()
            .map_err(|e| format!("blkid not available: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "no block device with label {fs_label:?} found (blkid exit {:?})",
                out.status.code()
            ));
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() {
            return Err(format!("blkid -L {fs_label:?} returned no device path"));
        }
        Ok(PathBuf::from(path))
    }

    fn mount(&self, device: &Path, target: &Path) -> Result<(), String> {
        std::fs::create_dir_all(target)
            .map_err(|e| format!("mkdir -p {}: {e}", target.display()))?;
        let out = std::process::Command::new("mount")
            .args(["-o", MOUNT_OPTIONS])
            .arg(device)
            .arg(target)
            .output()
            .map_err(|e| format!("mount not available: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "mount {} -> {} failed: {}",
                device.display(),
                target.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    fn sync_and_umount(&self, target: &Path) -> Result<(), String> {
        let _ = std::process::Command::new("sync").status();
        let out = std::process::Command::new("umount")
            .arg(target)
            .output()
            .map_err(|e| format!("umount not available: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "umount {} failed: {}",
                target.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// The one prefix every durable-mount target must be a strict subpath of.
const STATE_ROOT: &str = "/ato/state";

/// Fail-closed target validation, independent of any real mount call —
/// defense in depth even though the builder already restricts `target` to
/// under `/ato/state/`: this agent never trusts a config it didn't produce
/// itself.
///
/// Review fix (ato#991): the prefix check uses `strip_prefix` on a
/// COMPONENT basis (not a string comparison) — `/ato/state` must be a whole
/// path component, so `/ato/stateevil/db` or `/ato/state2/db` are rejected,
/// and the empty remainder case (`target == "/ato/state"` exactly) is
/// rejected too, not just "shares the prefix". And the symlink check now
/// walks EVERY ancestor from `/ato/state` down to the target itself, not
/// only the leaf: `create_dir_all` + `mount` follow symlinks at any
/// intermediate component, so an attacker-controlled rootfs making
/// `/ato/state` (or any directory under it) a symlink could otherwise
/// redirect the mount to an arbitrary path even though the leaf target
/// itself is a plain, never-before-seen directory.
pub fn validate_mount_target(target: &str) -> Result<PathBuf, String> {
    let path = Path::new(target);
    if !path.is_absolute() {
        return Err(format!("volume target {target:?} must be an absolute path"));
    }
    for component in path.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(format!(
                "volume target {target:?} must not contain '.' or '..'"
            ));
        }
    }
    match path.strip_prefix(STATE_ROOT) {
        Ok(rest) if rest.as_os_str().is_empty() => {
            return Err(format!(
                "volume target {target:?} must be a subpath under '{STATE_ROOT}/', not '{STATE_ROOT}' itself"
            ));
        }
        Ok(_) => {}
        Err(_) => {
            return Err(format!(
                "volume target {target:?} must be under '{STATE_ROOT}/'"
            ));
        }
    }
    reject_if_any_ancestor_is_not_a_plain_directory(path)?;
    Ok(path.to_path_buf())
}

/// Walks every ancestor of `path` from `/ato/state` down to (and including)
/// `path` itself, rejecting the first one that is a symlink (mounting
/// through it could redirect the mount to whatever it points at — e.g.
/// `/app` or `/etc`) or — for the leaf only — an existing regular file
/// (mounting there fails confusingly instead of cleanly; an intermediate
/// ancestor being a file would already fail `create_dir_all`, so it isn't
/// specially distinguished here). A missing ancestor is fine — it gets
/// created by `mkdir -p` at mount time.
fn reject_if_any_ancestor_is_not_a_plain_directory(path: &Path) -> Result<(), String> {
    // `ancestors()` yields `path` itself first, then each parent up to `/` —
    // collect + reverse so we walk top-down (`/ato`, then `/ato/state`, then
    // deeper), matching the order an attacker-controlled symlink would
    // actually be resolved in. `/` itself is skipped (always exists, never a
    // symlink, not ours to gate) — everything under it, INCLUDING `/ato`
    // itself, is checked: if `/ato` were a symlink, `/ato/state/...` would
    // resolve through it just as much as if `/ato/state` were.
    let mut chain: Vec<&Path> = path.ancestors().collect();
    chain.reverse();
    for ancestor in chain {
        if ancestor == Path::new("/") {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!(
                    "volume target {}: ancestor {} is a symlink — refusing to mount through it",
                    path.display(),
                    ancestor.display()
                ));
            }
            Ok(meta) if meta.is_file() && ancestor == path => {
                return Err(format!(
                    "volume target {} already exists as a regular file, not a directory",
                    path.display()
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Mount every declared volume, sorted by `state_name` for a deterministic
/// order, BEFORE any service starts. Fail-closed: the first failure aborts —
/// the workload never starts half-mounted.
pub fn mount_all_volumes(
    mounter: &dyn VolumeMounter,
    volumes: &[VolumeSpec],
) -> Result<(), String> {
    let mut sorted: Vec<&VolumeSpec> = volumes.iter().collect();
    sorted.sort_by(|a, b| a.state_name.cmp(&b.state_name));
    // Review fix (ato#991): track every volume ALREADY mounted so a LATER
    // volume's failure rolls them back before returning — otherwise volume A
    // stays mounted while the agent reports the whole mount step failed
    // (main() exits(2) on that failure, but a fresh init/supervisor restart
    // on the SAME guest could then see A already mounted from a half-applied
    // previous attempt; a durable-state agent must never leave a partially
    // applied mount set lying around).
    let mut mounted: Vec<&VolumeSpec> = Vec::new();
    for vol in sorted {
        let target = match validate_mount_target(&vol.target) {
            Ok(t) => t,
            Err(e) => {
                rollback_mounted(mounter, &mounted);
                return Err(format!("state '{}': {e}", vol.state_name));
            }
        };
        let device = match mounter.resolve_device(&vol.fs_label) {
            Ok(d) => d,
            Err(e) => {
                rollback_mounted(mounter, &mounted);
                return Err(format!("state '{}': {e}", vol.state_name));
            }
        };
        if let Err(e) = mounter.mount(&device, &target) {
            rollback_mounted(mounter, &mounted);
            return Err(format!("state '{}': {e}", vol.state_name));
        }
        eprintln!(
            "ato-guest-agent: mounted state '{}' ({}) at {} [drive {}]",
            vol.state_name,
            vol.fs_label,
            target.display(),
            vol.drive_id
        );
        mounted.push(vol);
    }
    Ok(())
}

/// Best-effort unmount, in REVERSE (most-recently-mounted first) order, of
/// every volume already mounted before a later one failed. Logged, never
/// panics — the caller is already returning the ORIGINAL failure; a rollback
/// hiccup must not mask it or abort partway through the rest of the rollback.
fn rollback_mounted(mounter: &dyn VolumeMounter, mounted: &[&VolumeSpec]) {
    for vol in mounted.iter().rev() {
        if let Err(e) = mounter.sync_and_umount(Path::new(&vol.target)) {
            eprintln!(
                "ato-guest-agent: rollback umount state '{}' at {}: {e}",
                vol.state_name, vol.target
            );
        }
    }
}

/// Best-effort unmount of every volume on shutdown — logged, never fatal (VM
/// termination proceeds regardless of a umount failure).
pub fn unmount_all_volumes(mounter: &dyn VolumeMounter, volumes: &[VolumeSpec]) {
    for vol in volumes {
        if let Err(e) = mounter.sync_and_umount(Path::new(&vol.target)) {
            eprintln!(
                "ato-guest-agent: umount state '{}' at {}: {e}",
                vol.state_name, vol.target
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeMounter {
        /// device path returned per label (label -> device), or absent = "not found"
        devices: std::collections::BTreeMap<String, String>,
        mount_calls: Mutex<Vec<(PathBuf, PathBuf)>>,
        umount_calls: Mutex<Vec<PathBuf>>,
        fail_mount_for: Option<PathBuf>,
        fail_resolve_for: Option<String>,
    }
    impl VolumeMounter for FakeMounter {
        fn resolve_device(&self, fs_label: &str) -> Result<PathBuf, String> {
            if self.fail_resolve_for.as_deref() == Some(fs_label) {
                return Err(format!("fake: no device for {fs_label}"));
            }
            self.devices
                .get(fs_label)
                .map(PathBuf::from)
                .ok_or_else(|| format!("fake: label {fs_label} not found"))
        }
        fn mount(&self, device: &Path, target: &Path) -> Result<(), String> {
            if self.fail_mount_for.as_deref() == Some(target) {
                return Err("fake: injected mount failure".to_string());
            }
            self.mount_calls
                .lock()
                .unwrap()
                .push((device.to_path_buf(), target.to_path_buf()));
            Ok(())
        }
        fn sync_and_umount(&self, target: &Path) -> Result<(), String> {
            self.umount_calls.lock().unwrap().push(target.to_path_buf());
            Ok(())
        }
    }

    fn vol(state_name: &str, target: &str, label: &str) -> VolumeSpec {
        VolumeSpec {
            state_name: state_name.to_string(),
            target: target.to_string(),
            fs_label: label.to_string(),
            drive_id: "state0".to_string(),
            size_mb: 64,
        }
    }

    #[test]
    fn validate_mount_target_accepts_a_plain_ato_state_subpath() {
        assert!(validate_mount_target("/ato/state/dbdata").is_ok());
    }

    #[test]
    fn validate_mount_target_rejects_relative_and_dotdot_and_outside_prefix() {
        assert!(validate_mount_target("ato/state/dbdata").is_err());
        assert!(validate_mount_target("/ato/state/../etc").is_err());
        assert!(validate_mount_target("/etc/passwd").is_err());
        assert!(validate_mount_target("/app").is_err());
        assert!(validate_mount_target("/tmp/x").is_err());
    }

    #[test]
    fn validate_mount_target_rejects_exact_root_and_component_boundary_lookalikes() {
        // Review fix (ato#991): `/ato/state` itself is not a valid mount
        // target (needs a real subpath), and a naive STRING prefix check
        // would wrongly accept "/ato/stateevil/..." / "/ato/state2/..." —
        // these must be rejected on a COMPONENT boundary, not a string match.
        assert!(validate_mount_target("/ato/state/dbdata").is_ok());
        assert!(
            validate_mount_target("/ato/state").is_err(),
            "exact /ato/state must be rejected"
        );
        assert!(
            validate_mount_target("/ato/stateevil/db").is_err(),
            "component lookalike must be rejected"
        );
        assert!(
            validate_mount_target("/ato/state2/db").is_err(),
            "component lookalike must be rejected"
        );
    }

    /// `tempfile::tempdir()`'s path can itself sit under an OS-level symlink
    /// unrelated to anything the test sets up (e.g. macOS's `/var` ->
    /// `/private/var`) — canonicalize it first so the ancestor walk only
    /// ever sees symlinks THIS test deliberately created.
    fn canonical_tmpdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let resolved = std::fs::canonicalize(dir.path()).unwrap();
        (dir, resolved)
    }

    #[test]
    #[cfg(unix)]
    fn rejects_a_symlinked_leaf_or_an_existing_regular_file_at_the_leaf() {
        let (_dir, root) = canonical_tmpdir();
        let real_dir = root.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();
        let err = reject_if_any_ancestor_is_not_a_plain_directory(&link).unwrap_err();
        assert!(err.contains("symlink"), "{err}");

        let regular_file = root.join("file");
        std::fs::write(&regular_file, b"x").unwrap();
        let err = reject_if_any_ancestor_is_not_a_plain_directory(&regular_file).unwrap_err();
        assert!(err.contains("regular file"), "{err}");
    }

    #[test]
    #[cfg(unix)]
    fn rejects_an_intermediate_ancestor_that_is_a_symlink_even_when_the_leaf_itself_is_fine() {
        // Review fix (ato#991): mounting/mkdir follows symlinks at ANY
        // component, not just the leaf — if an ancestor directory were a
        // symlink, the mount could land somewhere entirely different even
        // though the leaf path string looks like a fresh, never-before-seen
        // directory.
        let (_dir, root) = canonical_tmpdir();
        let real_parent = root.join("real-parent");
        std::fs::create_dir_all(&real_parent).unwrap();
        let symlinked_ancestor = root.join("state-link");
        std::os::unix::fs::symlink(&real_parent, &symlinked_ancestor).unwrap();
        // The LEAF itself doesn't exist yet (a fresh subdir under the
        // symlinked ancestor) — only the ancestor is a symlink.
        let leaf = symlinked_ancestor.join("dbdata");
        let err = reject_if_any_ancestor_is_not_a_plain_directory(&leaf).unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(
            err.contains("state-link"),
            "the error must name the offending ancestor: {err}"
        );
    }

    #[test]
    fn accepts_an_existing_plain_directory_or_a_fully_missing_path() {
        let (_dir, root) = canonical_tmpdir();
        let existing_dir = root.join("real");
        std::fs::create_dir_all(&existing_dir).unwrap();
        assert!(reject_if_any_ancestor_is_not_a_plain_directory(&existing_dir).is_ok());
        assert!(
            reject_if_any_ancestor_is_not_a_plain_directory(&root.join("does-not-exist-yet"))
                .is_ok()
        );
    }

    #[test]
    fn mount_all_volumes_mounts_in_state_name_sorted_order() {
        let mut m = FakeMounter::default();
        m.devices
            .insert("LBL_AAA".to_string(), "/dev/vdb".to_string());
        m.devices
            .insert("LBL_BBB".to_string(), "/dev/vdc".to_string());
        let volumes = vec![
            vol("bbb", "/ato/state/bbb", "LBL_BBB"),
            vol("aaa", "/ato/state/aaa", "LBL_AAA"),
        ];
        mount_all_volumes(&m, &volumes).unwrap();
        let calls = m.mount_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            (PathBuf::from("/dev/vdb"), PathBuf::from("/ato/state/aaa")),
            "aaa mounted first"
        );
        assert_eq!(
            calls[1],
            (PathBuf::from("/dev/vdc"), PathBuf::from("/ato/state/bbb")),
            "bbb mounted second"
        );
    }

    #[test]
    fn mount_all_volumes_fails_closed_when_a_device_cannot_be_resolved() {
        let m = FakeMounter {
            fail_resolve_for: Some("LBL_MISSING".to_string()),
            ..Default::default()
        };
        let volumes = vec![vol("dbdata", "/ato/state/dbdata", "LBL_MISSING")];
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("dbdata"), "{err}");
    }

    #[test]
    fn mount_all_volumes_fails_closed_on_invalid_target_and_rolls_back_the_earlier_success() {
        let mut m = FakeMounter::default();
        m.devices
            .insert("LBL_GOOD".to_string(), "/dev/vdb".to_string());
        // "agood" < "zbad" lexicographically, so "agood" sorts (and mounts)
        // first, THEN "zbad"'s invalid target fails.
        let volumes = vec![
            vol("agood", "/ato/state/good", "LBL_GOOD"),
            vol("zbad", "/etc/passwd", "LBL_BAD"),
        ];
        // Review fix (ato#991): "agood" must be rolled back, not left mounted.
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("zbad"), "{err}");
        assert_eq!(
            m.mount_calls.lock().unwrap().len(),
            1,
            "agood WAS mounted before zbad failed"
        );
        assert_eq!(
            m.umount_calls.lock().unwrap().as_slice(),
            [PathBuf::from("/ato/state/good")],
            "agood must be rolled back"
        );
    }

    #[test]
    fn mount_all_volumes_stops_at_the_first_mount_failure_with_nothing_to_roll_back() {
        let mut m = FakeMounter {
            fail_mount_for: Some(PathBuf::from("/ato/state/aaa")),
            ..Default::default()
        };
        m.devices
            .insert("LBL_AAA".to_string(), "/dev/vdb".to_string());
        m.devices
            .insert("LBL_BBB".to_string(), "/dev/vdc".to_string());
        let volumes = vec![
            vol("aaa", "/ato/state/aaa", "LBL_AAA"),
            vol("bbb", "/ato/state/bbb", "LBL_BBB"),
        ];
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("aaa"), "{err}");
        assert!(
            m.mount_calls.lock().unwrap().is_empty(),
            "aaa failed before any successful mount call recorded"
        );
        assert!(
            m.umount_calls.lock().unwrap().is_empty(),
            "nothing succeeded yet, so nothing to roll back"
        );
    }

    #[test]
    fn mount_all_volumes_rolls_back_an_earlier_success_when_a_later_devices_resolve_fails() {
        // The exact scenario from review: aaa mounts successfully, bbb's
        // device resolution (blkid) then fails — aaa must be rolled back.
        let mut m = FakeMounter {
            fail_resolve_for: Some("LBL_BBB".to_string()),
            ..Default::default()
        };
        m.devices
            .insert("LBL_AAA".to_string(), "/dev/vdb".to_string());
        let volumes = vec![
            vol("aaa", "/ato/state/aaa", "LBL_AAA"),
            vol("bbb", "/ato/state/bbb", "LBL_BBB"),
        ];
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("bbb"), "{err}");
        assert_eq!(
            m.mount_calls.lock().unwrap().len(),
            1,
            "aaa mounted before bbb's resolve failed"
        );
        assert_eq!(
            m.umount_calls.lock().unwrap().as_slice(),
            [PathBuf::from("/ato/state/aaa")],
            "aaa must be rolled back"
        );
    }

    #[test]
    fn mount_all_volumes_rolls_back_in_reverse_order_for_multiple_earlier_successes() {
        let mut m = FakeMounter {
            fail_resolve_for: Some("LBL_CCC".to_string()),
            ..Default::default()
        };
        m.devices
            .insert("LBL_AAA".to_string(), "/dev/vdb".to_string());
        m.devices
            .insert("LBL_BBB".to_string(), "/dev/vdc".to_string());
        let volumes = vec![
            vol("aaa", "/ato/state/aaa", "LBL_AAA"),
            vol("bbb", "/ato/state/bbb", "LBL_BBB"),
            vol("ccc", "/ato/state/ccc", "LBL_CCC"),
        ];
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("ccc"), "{err}");
        assert_eq!(
            m.umount_calls.lock().unwrap().as_slice(),
            [
                PathBuf::from("/ato/state/bbb"),
                PathBuf::from("/ato/state/aaa")
            ],
            "rollback unwinds most-recently-mounted first"
        );
    }

    #[test]
    fn unmount_all_volumes_attempts_every_volume_even_if_logged() {
        let m = FakeMounter::default();
        let volumes = vec![
            vol("aaa", "/ato/state/aaa", "LBL_AAA"),
            vol("bbb", "/ato/state/bbb", "LBL_BBB"),
        ];
        unmount_all_volumes(&m, &volumes);
        let calls = m.umount_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn no_volumes_is_a_no_op() {
        let m = FakeMounter::default();
        mount_all_volumes(&m, &[]).unwrap();
        assert!(m.mount_calls.lock().unwrap().is_empty());
        unmount_all_volumes(&m, &[]);
        assert!(m.umount_calls.lock().unwrap().is_empty());
    }
}
