//! Owner-only persistence for preparation records and verification provenance.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

use super::{Approval, Attempt, Phase, Plan, read_limited};
use crate::{ato_home, run_data, unix_millis};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub(super) id: String,
    pub(super) plan: Plan,
    pub(super) phase: Phase,
    pub(super) issue: Option<String>,
    pub(super) changed_at_ms: u64,
    pub(super) attempts: Vec<Attempt>,
    pub(super) approval: Option<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) native_artifact_authorization: Option<String>,
    pub(super) publication: Option<Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Signed {
    record: Entry,
    mac: String,
}

pub(super) struct Store {
    root: PathBuf,
    key: [u8; 32],
}

impl Store {
    pub(super) fn open() -> Result<Self> {
        let home = ato_home()?;
        fs::create_dir_all(&home)?;
        let root = home.join("publication");
        match create_private(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("Create the publication store"),
        }
        let metadata = fs::symlink_metadata(&root)?;
        ensure!(metadata.is_dir(), "Invalid publication store directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "The publication store must be owner-only"
            );
        }
        let key = owner_key(&root)?;
        Ok(Self { root, key })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn directory(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "Invalid preparation ID"
        );
        let path = self.root.join(id);
        ensure!(
            fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()),
            "Unknown preparation ID"
        );
        Ok(path)
    }

    pub(super) fn create(&self, plan: Plan, bytes: &[u8]) -> Result<Entry> {
        let mut id = run_data::token()?;
        let directory = loop {
            let directory = self.root.join(&id);
            match create_private(&directory) {
                Ok(()) => break directory,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => id = run_data::token()?,
                Err(error) => return Err(error).context("Allocate a new preparation"),
            }
        };
        let mut artifact = tempfile::NamedTempFile::new_in(&directory)?;
        artifact.write_all(bytes)?;
        artifact.as_file().sync_all()?;
        artifact.persist_noclobber(directory.join("artifact.capsule"))?;
        let entry = Entry {
            id,
            plan,
            phase: Phase::Prepared,
            issue: None,
            changed_at_ms: unix_millis(),
            attempts: Vec::new(),
            approval: None,
            native_artifact_authorization: None,
            publication: None,
        };
        self.save(&entry)?;
        Ok(entry)
    }

    pub(super) fn read(&self, id: &str) -> Result<Entry> {
        let path = self.directory(id)?.join("state.json");
        let bytes = read_limited(File::open(&path)?, 2 * 1024 * 1024)?;
        let signed: Signed =
            serde_json::from_slice(&bytes).context("Invalid preparation record")?;
        let mut mac = self.mac()?;
        mac.update(&serde_jcs::to_vec(&signed.record)?);
        let tag = base64::engine::general_purpose::STANDARD
            .decode(signed.mac)
            .context("Invalid preparation authentication tag")?;
        mac.verify_slice(&tag)
            .map_err(|_| anyhow::anyhow!("Preparation authentication failed"))?;
        ensure!(
            signed.record.id == id,
            "Preparation ID does not match its signed record"
        );
        Ok(signed.record)
    }

    pub(super) fn save(&self, entry: &Entry) -> Result<()> {
        let path = self.directory(&entry.id)?.join("state.json");
        let mut mac = self.mac()?;
        mac.update(&serde_jcs::to_vec(entry)?);
        let tag = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        run_data::write_json(&path, &serde_json::json!({"record":entry,"mac":tag}))
    }

    fn mac(&self) -> Result<Hmac<Sha256>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key)
            .map_err(|_| anyhow::anyhow!("Invalid publication authentication key"))?;
        mac.update(b"ato.publication.state/1\0");
        Ok(mac)
    }

    pub(super) fn artifact(&self, entry: &Entry) -> Result<Vec<u8>> {
        let path = self.directory(&entry.id)?.join("artifact.capsule");
        ensure!(
            fs::symlink_metadata(&path)?.is_file(),
            "Frozen artifact must be a regular file"
        );
        let bytes = read_limited(File::open(path)?, super::MAX_ARTIFACT)?;
        ensure!(
            bytes.len() as u64 == entry.plan.artifact_size
                && super::bundle_sha256(&bytes) == entry.plan.artifact_sha256,
            "The frozen artifact changed. Start a new preparation"
        );
        Ok(bytes)
    }

    pub(super) fn edit(&self, id: &str) -> Result<File> {
        let file = lock_file(&self.directory(id)?.join("edit.lock"))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    }

    pub(super) fn job(&self, id: &str) -> Result<File> {
        let file = lock_file(&self.directory(id)?.join("job.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)?;
        Ok(file)
    }

    pub(super) fn job_idle(&self, id: &str) -> Result<bool> {
        idle(&self.directory(id)?.join("job.lock"))
    }

    pub(super) fn edit_idle(&self, id: &str) -> Result<bool> {
        idle(&self.directory(id)?.join("edit.lock"))
    }

    pub(super) fn cancel(&self, id: &str) -> Result<()> {
        run_data::write_json(&self.directory(id)?.join("cancel.json"), &true)
    }
}

fn lock_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

fn idle(path: &Path) -> Result<bool> {
    match fs2::FileExt::try_lock_exclusive(&lock_file(path)?) {
        Ok(()) => Ok(true),
        Err(error) if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

fn owner_key(root: &Path) -> Result<[u8; 32]> {
    let path = root.join("owner.key");
    if !path.try_exists()? {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key)
            .map_err(|_| anyhow::anyhow!("Could not generate a publication authentication key"))?;
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.write_all(&key)?;
        file.as_file().sync_all()?;
        match file.persist_noclobber(&path) {
            Ok(_) => crate::hub::sync_directory_best_effort(root),
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(metadata.is_file(), "Invalid publication authentication key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "The publication authentication key must be owner-only"
        );
    }
    read_limited(File::open(path)?, 32)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Invalid publication authentication key"))
}

fn create_private(path: &Path) -> std::io::Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path)
}
