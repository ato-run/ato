//! Runner-local persistent volumes: state that lives on this Runner and is
//! mounted as-is, Run after Run.
//!
//! A revision-backed slot is restored into a per-Run working copy and packed
//! back afterwards; its truth is the State Revision. A volume-backed slot's
//! truth is the directory here. It is written in place, survives the Run that
//! wrote it, and is never rebuilt from an older revision behind anyone's back:
//! a volume the control plane believes exists but this Runner cannot find is
//! reported missing and the Run refused.
//!
//! Layout, per physical host and shared by every slot worker on it:
//!
//! ```text
//! <root>/<runner_id>/volumes/<volume_ref>/metadata.json
//! <root>/<runner_id>/volumes/<volume_ref>/data/        <- mounted
//! <root>/<runner_id>/locks/<volume_ref>.lock
//! ```
//!
//! None of these paths leaves the Runner: not in a spec, a receipt, an API
//! call or a log line. A volume is named by its `volume_ref` only.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_ipc::runtime_launch_v3::is_volume_ref;

/// Advertised only by a build that understands launch spec v3 AND has a
/// usable volume root.
pub const RUNNER_PERSISTENT_VOLUME_FEATURE: &str = "runtime_feature=runner_persistent_volume_v1";

const METADATA_SCHEMA: &str = "ato.runner-volume/1";
const METADATA_FILE: &str = "metadata.json";
const DATA_DIR: &str = "data";
/// Kept free on the volume filesystem beyond what a volume may still grow by,
/// so one volume reaching its capacity does not fill the disk for the others.
pub const VOLUME_FREE_SPACE_RESERVE: u64 = 256 * 1024 * 1024;

/// What the control plane says about a volume when it grants the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeStatus {
    /// Assigned to this Runner, not yet confirmed created.
    Provisioning,
    /// Confirmed created. Its directory must exist.
    Ready,
    /// Already known lost. Nothing is created in its place.
    Missing,
}

impl VolumeStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "provisioning" => Some(Self::Provisioning),
            "ready" => Some(Self::Ready),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }
}

/// Typed refusals. The code is what reaches the control plane and the owner.
#[derive(Debug)]
pub enum VolumeError {
    /// The volume should exist here and does not, or is not the one named.
    Missing {
        reason: String,
    },
    /// Another attachment on this host holds it.
    Locked,
    /// The filesystem cannot offer what the volume may still grow by.
    InsufficientSpace {
        needed: u64,
        available: u64,
    },
    /// The volume already uses more than its capacity.
    CapacityExceeded {
        usage: u64,
        capacity: u64,
    },
    /// A reference or Runner id this store will not turn into a path.
    Invalid {
        reason: String,
    },
    Io(anyhow::Error),
}

impl VolumeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "state_volume_missing",
            Self::Locked => "state_volume_locked",
            Self::InsufficientSpace { .. } => "state_volume_insufficient_space",
            Self::CapacityExceeded { .. } => "state_volume_capacity_exceeded",
            Self::Invalid { .. } => "state_volume_invalid",
            Self::Io(_) => "state_volume_io",
        }
    }
}

impl std::fmt::Display for VolumeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { reason } => write!(formatter, "{}: {reason}", self.code()),
            Self::Locked => write!(
                formatter,
                "{}: attached elsewhere on this host",
                self.code()
            ),
            Self::InsufficientSpace { needed, available } => write!(
                formatter,
                "{}: needs {needed} bytes free, {available} available",
                self.code()
            ),
            Self::CapacityExceeded { usage, capacity } => write!(
                formatter,
                "{}: uses {usage} of {capacity} bytes",
                self.code()
            ),
            Self::Invalid { reason } => write!(formatter, "{}: {reason}", self.code()),
            Self::Io(error) => write!(formatter, "{}: {error:#}", self.code()),
        }
    }
}

impl std::error::Error for VolumeError {}

impl From<anyhow::Error> for VolumeError {
    fn from(error: anyhow::Error) -> Self {
        Self::Io(error)
    }
}

impl From<std::io::Error> for VolumeError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.into())
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct VolumeMetadata {
    schema: String,
    volume_ref: String,
    runner_id: String,
    /// The revision it was seeded from, if any. Provenance only.
    seeded_from: Option<String>,
}

/// A volume attached to one Run. Holding it holds the host-local lock.
///
/// The lock only keeps two attachments on THIS host apart. It is not proof
/// that a previous workload stopped — a container outlives the Runner process
/// that locked for it — and nothing treats it as such.
#[derive(Debug)]
pub struct AttachedVolume {
    volume_ref: String,
    data_dir: PathBuf,
    /// True when this attachment created the volume.
    pub provisioned: bool,
    _lock: File,
}

impl AttachedVolume {
    pub fn volume_ref(&self) -> &str {
        &self.volume_ref
    }

    /// Runtime-private. Never reported.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Bytes the volume's data occupies on disk.
    pub fn usage(&self) -> Result<u64> {
        directory_usage(&self.data_dir)
    }
}

/// The host's volume store for one Runner identity.
#[derive(Debug, Clone)]
pub struct VolumeStore {
    volumes: PathBuf,
    locks: PathBuf,
    runner_id: String,
}

fn is_runner_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl VolumeStore {
    /// Open (creating if needed) the store under `root`, and prove it is
    /// writable. A store that cannot be written must not be advertised.
    pub fn open(root: &Path, runner_id: &str) -> Result<Self> {
        anyhow::ensure!(is_runner_id(runner_id), "runner id is not a path-safe id");
        let base = root.join(runner_id);
        let volumes = base.join("volumes");
        let locks = base.join("locks");
        for dir in [&volumes, &locks] {
            fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let probe = volumes.join(format!(".probe-{}", std::process::id()));
        File::create(&probe)
            .and_then(|file| file.sync_all())
            .with_context(|| format!("volume root {} is not writable", volumes.display()))?;
        let _ = fs::remove_file(&probe);
        Ok(Self {
            volumes,
            locks,
            runner_id: runner_id.to_owned(),
        })
    }

    fn volume_dir(&self, volume_ref: &str) -> Result<PathBuf, VolumeError> {
        if !is_volume_ref(volume_ref) {
            return Err(VolumeError::Invalid {
                reason: "volume_ref is not svol_<ULID>".to_owned(),
            });
        }
        Ok(self.volumes.join(volume_ref))
    }

    /// Where the volume's data is mounted from. Exists only once attached.
    pub fn data_dir(&self, volume_ref: &str) -> Result<PathBuf, VolumeError> {
        Ok(self.volume_dir(volume_ref)?.join(DATA_DIR))
    }

    /// Attach a volume for one Run: lock it, create it if the control plane
    /// says it is still being provisioned, verify it otherwise, and admit it
    /// against its capacity.
    ///
    /// `seed` fills a NEW volume's data directory and is never called for one
    /// that exists: an existing volume is newer than any revision.
    pub fn attach(
        &self,
        volume_ref: &str,
        capacity_bytes: u64,
        status: VolumeStatus,
        seed_revision: Option<&str>,
        seed: &dyn Fn(&Path) -> Result<()>,
    ) -> Result<AttachedVolume, VolumeError> {
        let dir = self.volume_dir(volume_ref)?;
        if status == VolumeStatus::Missing {
            return Err(VolumeError::Missing {
                reason: "the control plane already recorded this volume as missing".to_owned(),
            });
        }
        let lock = self.lock(volume_ref)?;

        let data_dir = dir.join(DATA_DIR);
        let exists = dir.exists();
        if exists {
            self.verify(&dir, volume_ref)?;
        } else if status != VolumeStatus::Provisioning {
            // Ready, and not here: whatever was here is gone. Rebuilding it
            // from a revision would roll the data back and call it success.
            return Err(VolumeError::Missing {
                reason: "the volume is recorded ready on this Runner but is not present".to_owned(),
            });
        }

        // Admitted before anything is created: a volume this disk cannot hold
        // is never provisioned, only refused.
        let usage = if exists {
            directory_usage(&data_dir)?
        } else {
            0
        };
        if usage > capacity_bytes {
            return Err(VolumeError::CapacityExceeded {
                usage,
                capacity: capacity_bytes,
            });
        }
        let needed = (capacity_bytes - usage).saturating_add(VOLUME_FREE_SPACE_RESERVE);
        let available = available_bytes(&self.volumes)?;
        if available < needed {
            return Err(VolumeError::InsufficientSpace { needed, available });
        }

        let provisioned = !exists;
        if provisioned {
            self.provision(&dir, volume_ref, seed_revision, seed)?;
        }
        Ok(AttachedVolume {
            volume_ref: volume_ref.to_owned(),
            data_dir,
            provisioned,
            _lock: lock,
        })
    }

    fn lock(&self, volume_ref: &str) -> Result<File, VolumeError> {
        let path = self.locks.join(format!("{volume_ref}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        if let Err(error) = fs2::FileExt::try_lock_exclusive(&file) {
            return Err(
                if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                    VolumeError::Locked
                } else {
                    VolumeError::Io(error.into())
                },
            );
        }
        Ok(file)
    }

    fn verify(&self, dir: &Path, volume_ref: &str) -> Result<(), VolumeError> {
        let missing = |reason: &str| VolumeError::Missing {
            reason: reason.to_owned(),
        };
        let raw = fs::read(dir.join(METADATA_FILE))
            .map_err(|_| missing("the volume has no readable metadata"))?;
        let metadata: VolumeMetadata = serde_json::from_slice(&raw)
            .map_err(|_| missing("the volume metadata is malformed"))?;
        if metadata.schema != METADATA_SCHEMA
            || metadata.volume_ref != volume_ref
            || metadata.runner_id != self.runner_id
        {
            return Err(missing("the directory holds a different volume"));
        }
        let data = fs::symlink_metadata(dir.join(DATA_DIR))
            .map_err(|_| missing("the volume has no data directory"))?;
        if !data.is_dir() {
            return Err(missing("the volume data is not a directory"));
        }
        Ok(())
    }

    /// Temporary directory, seed, fsync, atomic rename. A Runner that dies
    /// part-way leaves only a `.tmp-<volume_ref>-*` directory, which the next
    /// attempt for the same volume removes; a finished volume is either fully
    /// there or not there at all.
    fn provision(
        &self,
        dir: &Path,
        volume_ref: &str,
        seed_revision: Option<&str>,
        seed: &dyn Fn(&Path) -> Result<()>,
    ) -> Result<(), VolumeError> {
        let prefix = format!(".tmp-{volume_ref}-");
        for entry in fs::read_dir(&self.volumes)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(&prefix) {
                fs::remove_dir_all(entry.path())?;
            }
        }
        let staging = self.volumes.join(format!(
            "{prefix}{}",
            super::recovery::incarnation().replace(|c: char| !c.is_ascii_alphanumeric(), "")
        ));
        fs::create_dir(&staging)?;
        let data = staging.join(DATA_DIR);
        seed(&data).map_err(VolumeError::Io)?;
        fs::create_dir_all(&data)?;
        let metadata = VolumeMetadata {
            schema: METADATA_SCHEMA.to_owned(),
            volume_ref: volume_ref.to_owned(),
            runner_id: self.runner_id.clone(),
            seeded_from: seed_revision.map(str::to_owned),
        };
        let mut file = File::create(staging.join(METADATA_FILE))?;
        file.write_all(&serde_json::to_vec(&metadata).map_err(anyhow::Error::from)?)?;
        file.sync_all()?;
        sync_tree(&staging)?;
        fs::rename(&staging, dir)?;
        File::open(&self.volumes)?.sync_all()?;
        Ok(())
    }
}

/// fsync every file and directory under `root`, without following symlinks.
fn sync_tree(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else if kind.is_file() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(root)?.sync_all()?;
    Ok(())
}

/// Allocated bytes under `root`, not following symlinks.
fn directory_usage(root: &Path) -> Result<u64> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    let mut total = 0u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            #[cfg(unix)]
            {
                total = total.saturating_add(metadata.blocks().saturating_mul(512));
            }
            #[cfg(not(unix))]
            {
                total = total.saturating_add(metadata.len());
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(total)
}

fn available_bytes(path: &Path) -> Result<u64> {
    fs2::available_space(path).with_context(|| format!("statvfs {}", path.display()))
}

#[cfg(test)]
mod tests;
