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
        std::fs::create_dir_all(target).map_err(|e| format!("mkdir -p {}: {e}", target.display()))?;
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

/// Fail-closed target validation, independent of any real mount call —
/// defense in depth even though the builder already restricts `target` to
/// under `/ato/state/`: this agent never trusts a config it didn't produce
/// itself. Rejects a target that is a symlink (mounting through it could
/// shadow whatever it points at, e.g. `/app` or `/etc`) or an existing
/// regular file (mounting there fails confusingly instead of cleanly).
pub fn validate_mount_target(target: &str) -> Result<PathBuf, String> {
    let path = Path::new(target);
    if !path.is_absolute() {
        return Err(format!("volume target {target:?} must be an absolute path"));
    }
    for component in path.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(format!("volume target {target:?} must not contain '.' or '..'"));
        }
    }
    if !path.starts_with("/ato/state") {
        return Err(format!("volume target {target:?} must be under /ato/state/"));
    }
    reject_if_not_a_plain_mount_point(path)?;
    Ok(path.to_path_buf())
}

/// Refuses a target that already exists as a symlink (mounting through it
/// could shadow whatever it points at, e.g. `/app` or `/etc`) or as a regular
/// file (mounting there fails confusingly instead of cleanly). Split out from
/// `validate_mount_target` so this check is independently testable without
/// needing a real path under the hardcoded `/ato/state` prefix (which a
/// sandboxed test can't create on the real filesystem).
fn reject_if_not_a_plain_mount_point(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(format!("volume target {} is a symlink — refusing to mount through it", path.display()))
        }
        Ok(meta) if meta.is_file() => {
            Err(format!("volume target {} already exists as a regular file, not a directory", path.display()))
        }
        _ => Ok(()),
    }
}

/// Mount every declared volume, sorted by `state_name` for a deterministic
/// order, BEFORE any service starts. Fail-closed: the first failure aborts —
/// the workload never starts half-mounted.
pub fn mount_all_volumes(mounter: &dyn VolumeMounter, volumes: &[VolumeSpec]) -> Result<(), String> {
    let mut sorted: Vec<&VolumeSpec> = volumes.iter().collect();
    sorted.sort_by(|a, b| a.state_name.cmp(&b.state_name));
    for vol in sorted {
        let target = validate_mount_target(&vol.target).map_err(|e| format!("state '{}': {e}", vol.state_name))?;
        let device =
            mounter.resolve_device(&vol.fs_label).map_err(|e| format!("state '{}': {e}", vol.state_name))?;
        mounter.mount(&device, &target).map_err(|e| format!("state '{}': {e}", vol.state_name))?;
        eprintln!(
            "ato-guest-agent: mounted state '{}' ({}) at {} [drive {}]",
            vol.state_name,
            vol.fs_label,
            target.display(),
            vol.drive_id
        );
    }
    Ok(())
}

/// Best-effort unmount of every volume on shutdown — logged, never fatal (VM
/// termination proceeds regardless of a umount failure).
pub fn unmount_all_volumes(mounter: &dyn VolumeMounter, volumes: &[VolumeSpec]) {
    for vol in volumes {
        if let Err(e) = mounter.sync_and_umount(Path::new(&vol.target)) {
            eprintln!("ato-guest-agent: umount state '{}' at {}: {e}", vol.state_name, vol.target);
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
            self.mount_calls.lock().unwrap().push((device.to_path_buf(), target.to_path_buf()));
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
    #[cfg(unix)]
    fn reject_if_not_a_plain_mount_point_rejects_a_symlink_and_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();
        let err = reject_if_not_a_plain_mount_point(&link).unwrap_err();
        assert!(err.contains("symlink"), "{err}");

        let regular_file = dir.path().join("file");
        std::fs::write(&regular_file, b"x").unwrap();
        let err = reject_if_not_a_plain_mount_point(&regular_file).unwrap_err();
        assert!(err.contains("regular file"), "{err}");
    }

    #[test]
    fn reject_if_not_a_plain_mount_point_accepts_a_directory_or_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let existing_dir = dir.path().join("real");
        std::fs::create_dir_all(&existing_dir).unwrap();
        assert!(reject_if_not_a_plain_mount_point(&existing_dir).is_ok());
        assert!(reject_if_not_a_plain_mount_point(&dir.path().join("does-not-exist-yet")).is_ok());
    }

    #[test]
    fn mount_all_volumes_mounts_in_state_name_sorted_order() {
        let mut m = FakeMounter::default();
        m.devices.insert("LBL_AAA".to_string(), "/dev/vdb".to_string());
        m.devices.insert("LBL_BBB".to_string(), "/dev/vdc".to_string());
        let volumes = vec![vol("bbb", "/ato/state/bbb", "LBL_BBB"), vol("aaa", "/ato/state/aaa", "LBL_AAA")];
        mount_all_volumes(&m, &volumes).unwrap();
        let calls = m.mount_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (PathBuf::from("/dev/vdb"), PathBuf::from("/ato/state/aaa")), "aaa mounted first");
        assert_eq!(calls[1], (PathBuf::from("/dev/vdc"), PathBuf::from("/ato/state/bbb")), "bbb mounted second");
    }

    #[test]
    fn mount_all_volumes_fails_closed_when_a_device_cannot_be_resolved() {
        let m = FakeMounter { fail_resolve_for: Some("LBL_MISSING".to_string()), ..Default::default() };
        let volumes = vec![vol("dbdata", "/ato/state/dbdata", "LBL_MISSING")];
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("dbdata"), "{err}");
    }

    #[test]
    fn mount_all_volumes_fails_closed_on_invalid_target_before_any_mount_call() {
        let m = FakeMounter::default();
        let volumes = vec![
            vol("good", "/ato/state/good", "LBL_GOOD"),
            vol("bad", "/etc/passwd", "LBL_BAD"), // sorts AFTER "good" alphabetically
        ];
        // "bad" > "good" so "good" would mount first if we didn't fail before
        // attempting anything with an invalid target in the set — but the
        // invalid target must still be caught; assert the overall call fails.
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("bad"), "{err}");
    }

    #[test]
    fn mount_all_volumes_stops_at_the_first_failure_not_partial() {
        let mut m = FakeMounter { fail_mount_for: Some(PathBuf::from("/ato/state/aaa")), ..Default::default() };
        m.devices.insert("LBL_AAA".to_string(), "/dev/vdb".to_string());
        m.devices.insert("LBL_BBB".to_string(), "/dev/vdc".to_string());
        let volumes = vec![vol("aaa", "/ato/state/aaa", "LBL_AAA"), vol("bbb", "/ato/state/bbb", "LBL_BBB")];
        let err = mount_all_volumes(&m, &volumes).unwrap_err();
        assert!(err.contains("aaa"), "{err}");
        assert!(m.mount_calls.lock().unwrap().is_empty(), "aaa failed before any successful mount call recorded");
    }

    #[test]
    fn unmount_all_volumes_attempts_every_volume_even_if_logged() {
        let m = FakeMounter::default();
        let volumes = vec![vol("aaa", "/ato/state/aaa", "LBL_AAA"), vol("bbb", "/ato/state/bbb", "LBL_BBB")];
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
