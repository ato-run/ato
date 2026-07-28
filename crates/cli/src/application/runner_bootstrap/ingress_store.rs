//! The durable half of ingress activation: a [`GenerationStore`] over a real
//! directory tree.
//!
//! Every update here has to survive a crash at any instruction, because the
//! state machine above it ([`super::ingress_activation`]) reasons about exactly
//! that. Two rules make it possible:
//!
//! 1. **Nothing becomes visible half-written.** A generation is built in a
//!    temporary directory and renamed into place; a marker is written to a
//!    temporary file and renamed over its name. `rename` within one directory
//!    is atomic, so a reader sees the old value or the new one, never a splice.
//! 2. **A rename is not durable until its parent directory is.** The file's own
//!    `fsync` persists its bytes; only the directory's `fsync` persists the
//!    NAME pointing at them. Skipping it is the classic crash where a file
//!    exists with the right contents under the wrong name — or not at all.
//!
//! ```text
//! <root>/
//!   lock
//!   current -> generations/<generation_id>
//!   activated-generation
//!   activation.pending
//!   receipts/<generation_id>.json
//!   generations/<generation_id>/
//!     generation-manifest.json
//!     <known fragments>
//! ```
//!
//! Temporary files and directories are created in the SAME parent as their
//! final name, so the rename never crosses a filesystem (which would make it a
//! copy, and no longer atomic).
//!
//! # The lock protects a Caddy instance, not a builder
//!
//! Several builders can contribute fragments to one Caddy configuration, so a
//! per-`builder_id` lock would let two of them swap `current` concurrently and
//! still each believe it had exclusive access. The lock is therefore one per
//! store root — one root per Caddy instance — and it lives at a fixed path that
//! the `current` swap never touches.

#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::ingress_activation::{GenerationStore, PendingJournal};
use super::official_preview::{GENERATION_FRAGMENTS, GeneratedFragment};

const LOCK_FILE: &str = "lock";
const CURRENT_LINK: &str = "current";
const ACTIVATED_FILE: &str = "activated-generation";
const PENDING_FILE: &str = "activation.pending";
const RECEIPTS_DIR: &str = "receipts";
const GENERATIONS_DIR: &str = "generations";
const MANIFEST_FILE: &str = "generation-manifest.json";
const MANIFEST_SCHEMA: &str = "ato.runner-ingress-generation-manifest/v1";
/// Temporary names carry this prefix so a crash leaves something identifiable
/// to reclaim rather than a plausible-looking generation.
const TEMP_PREFIX: &str = ".tmp-";

/// What a generation directory claims to contain.
///
/// Completeness is judged against THIS plus the known-fragment list, never
/// against a directory listing: a listing cannot distinguish "this generation
/// has no wizard fragment" from "the wizard fragment has not been written yet",
/// and those are the two states a crash mid-publish sits between.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct GenerationManifest {
    schema: String,
    generation_id: String,
    fragments: Vec<ManifestEntry>,
}

/// One fragment's claim. `present: false` is an explicit absence — recorded so
/// it can never be confused with an empty file, which is a different
/// configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ManifestEntry {
    name: String,
    present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

fn manifest_for(generation_id: &str, fragments: &[GeneratedFragment]) -> GenerationManifest {
    GenerationManifest {
        schema: MANIFEST_SCHEMA.to_string(),
        generation_id: generation_id.to_string(),
        fragments: GENERATION_FRAGMENTS
            .iter()
            .map(
                |name| match fragments.iter().find(|f| f.file_name == *name) {
                    Some(fragment) => ManifestEntry {
                        name: (*name).to_string(),
                        present: true,
                        len: Some(fragment.content.len() as u64),
                        digest: Some(
                            blake3::hash(fragment.content.as_bytes())
                                .to_hex()
                                .to_string(),
                        ),
                    },
                    None => ManifestEntry {
                        name: (*name).to_string(),
                        present: false,
                        len: None,
                        digest: None,
                    },
                },
            )
            .collect(),
    }
}

/// A generation id is joined onto a path, so it is validated before it can be.
///
/// The dangerous inputs are not exotic: `..` walks out of the store, a leading
/// `/` escapes it entirely, and a NUL truncates the path at the syscall
/// boundary — so the check is a strict allowlist rather than a list of things
/// to reject.
fn validate_generation_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 {
        bail!(
            "generation id must be 1..=128 bytes (got {} bytes)",
            id.len()
        );
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("generation id {id:?} may only contain [a-z0-9-]");
    }
    if id.starts_with('-') || id.ends_with('-') {
        bail!("generation id {id:?} must not start or end with '-'");
    }
    Ok(())
}

#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("open {} to fsync it", path.display()))?
        .sync_all()
        .with_context(|| format!("fsync {}", path.display()))
}

#[cfg(windows)]
fn fsync_dir(_path: &Path) -> Result<()> {
    // Windows does not support opening a directory as a File. File contents
    // are flushed before rename, but there is no portable directory fsync
    // equivalent to perform here.
    Ok(())
}

/// A temporary name in the same directory as its final one. Unique per process
/// and per call, so two concurrent stores (or a leftover from a crash) cannot
/// collide on it.
fn temp_name(stem: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{TEMP_PREFIX}{stem}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Replace `dir/name` with `contents`, durably.
///
/// `create_new` on the temporary file is what keeps this from ever writing
/// THROUGH a symlink: an attacker-planted link at the temporary name makes the
/// open fail rather than redirect the write.
fn atomic_write(dir: &Path, name: &str, contents: &[u8]) -> Result<()> {
    let temp = dir.join(temp_name(name));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("create {}", temp.display()))?;
        file.write_all(contents)?;
        file.sync_all()
            .with_context(|| format!("fsync {}", temp.display()))?;
    }
    fs::rename(&temp, dir.join(name))
        .with_context(|| format!("rename {} into place", temp.display()))?;
    // The bytes are durable; this makes the NAME durable.
    fsync_dir(dir)
}

fn atomic_remove(dir: &Path, name: &str) -> Result<()> {
    match fs::remove_file(dir.join(name)) {
        Ok(()) => fsync_dir(dir),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}/{name}", dir.display())),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// A [`GenerationStore`] rooted at a directory.
pub(crate) struct FsGenerationStore {
    root: PathBuf,
    /// Held for the life of the activation. Dropping it releases the lock, so
    /// it is kept here rather than returned to a caller that might not.
    lock: Option<File>,
}

impl FsGenerationStore {
    /// Create the layout if needed and return a store. Does NOT take the lock —
    /// [`GenerationStore::lock`] does, so the ordering the state machine
    /// documents stays visible at its call site.
    pub(crate) fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for dir in [
            root.clone(),
            root.join(GENERATIONS_DIR),
            root.join(RECEIPTS_DIR),
        ] {
            fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        }
        Ok(Self { root, lock: None })
    }

    fn generations_dir(&self) -> PathBuf {
        self.root.join(GENERATIONS_DIR)
    }

    fn generation_dir(&self, id: &str) -> Result<PathBuf> {
        validate_generation_id(id)?;
        Ok(self.generations_dir().join(id))
    }

    fn read_manifest(&self, id: &str) -> Result<Option<GenerationManifest>> {
        let path = self.generation_dir(id)?.join(MANIFEST_FILE);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        let manifest: GenerationManifest =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if manifest.schema != MANIFEST_SCHEMA {
            bail!(
                "{} declares schema {:?}, expected {MANIFEST_SCHEMA}",
                path.display(),
                manifest.schema
            );
        }
        Ok(Some(manifest))
    }

    /// Publish a generation directory atomically, or accept an identical one
    /// that is already there.
    fn publish_generation(&self, id: &str, fragments: &[GeneratedFragment]) -> Result<()> {
        let final_dir = self.generation_dir(id)?;
        let wanted = manifest_for(id, fragments);

        // A generation id names its own contents, so an existing directory
        // under the same id must BE those contents. Same digest: nothing to do.
        // Different digest: two different configurations are claiming one
        // identity, and silently keeping either one would make the id a lie.
        if let Some(existing) = self.read_manifest(id)? {
            if existing == wanted {
                return Ok(());
            }
            bail!(
                "generation {id} already exists on disk with different contents — \
                 the id names the contents, so this is two configurations claiming one identity"
            );
        }
        if final_dir.exists() {
            bail!(
                "generation directory {} exists without a readable manifest — \
                 refusing to overwrite it; move it aside to reclaim the id",
                final_dir.display()
            );
        }

        let temp_dir = self.generations_dir().join(temp_name(id));
        fs::create_dir(&temp_dir).with_context(|| format!("create {}", temp_dir.display()))?;

        // Fixed order, known names only. An unknown name here would land a file
        // in a published generation that nothing ever checks for.
        for name in GENERATION_FRAGMENTS {
            let Some(fragment) = fragments.iter().find(|f| f.file_name == *name) else {
                continue;
            };
            write_and_sync(&temp_dir.join(name), fragment.content.as_bytes())?;
        }
        for fragment in fragments {
            if !GENERATION_FRAGMENTS.contains(&fragment.file_name) {
                bail!(
                    "fragment {:?} is not a known generation fragment",
                    fragment.file_name
                );
            }
        }
        let manifest = serde_json::to_vec_pretty(&wanted)?;
        write_and_sync(&temp_dir.join(MANIFEST_FILE), &manifest)?;
        fsync_dir(&temp_dir)?;

        // `rename` onto an existing non-empty directory fails, which is exactly
        // the no-overwrite guarantee wanted here — the race with another writer
        // resolves to an error rather than to a clobber.
        fs::rename(&temp_dir, &final_dir).with_context(|| {
            format!("publish {} as {}", temp_dir.display(), final_dir.display())
        })?;
        fsync_dir(&self.generations_dir())
    }
}

fn write_and_sync(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(contents)?;
    file.sync_all()
        .with_context(|| format!("fsync {}", path.display()))
}

impl GenerationStore for FsGenerationStore {
    fn lock(&mut self) -> Result<()> {
        use fs2::FileExt;
        // Already held by THIS store: the lock lives as long as the store does.
        // Re-acquiring would open a second file description and block on our own
        // exclusive lock — flock is per-description, so a store that activates
        // twice would deadlock against itself.
        if self.lock.is_some() {
            return Ok(());
        }
        let path = self.root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("take the exclusive activation lock at {}", path.display()))?;
        self.lock = Some(file);
        Ok(())
    }

    /// The generation `current` points at.
    ///
    /// Read with `read_link`, never by following it: a `current` that resolves
    /// outside the store is refused rather than obeyed, because obeying it
    /// would let a planted link redirect every later operation.
    fn read_current(&self) -> Result<Option<String>> {
        let path = self.root.join(CURRENT_LINK);
        let target = match fs::read_link(&path) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("read link {}", path.display()));
            }
        };
        let expected_parent = Path::new(GENERATIONS_DIR);
        let Some(id) = target.file_name().and_then(|name| name.to_str()) else {
            bail!(
                "{} points at {:?}, which names no generation",
                path.display(),
                target
            );
        };
        if target.parent() != Some(expected_parent) || target.is_absolute() {
            bail!(
                "{} points at {:?} — it may only point at a relative {GENERATIONS_DIR}/<id>",
                path.display(),
                target
            );
        }
        validate_generation_id(id)
            .with_context(|| format!("{} points at an invalid generation id", path.display()))?;
        Ok(Some(id.to_string()))
    }

    fn read_activated(&self) -> Result<Option<String>> {
        read_optional(&self.root.join(ACTIVATED_FILE))
    }

    fn read_pending(&self) -> Result<Option<PendingJournal>> {
        let path = self.root.join(PENDING_FILE);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        let journal: PendingJournal =
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(Some(journal))
    }

    fn write_generation(&mut self, digest: &str, fragments: &[GeneratedFragment]) -> Result<()> {
        self.publish_generation(digest, fragments)
    }

    fn generation_complete(&self, digest: &str) -> Result<bool> {
        let Some(manifest) = self.read_manifest(digest)? else {
            return Ok(false);
        };
        if manifest.generation_id != digest {
            return Ok(false);
        }
        let dir = self.generation_dir(digest)?;
        for entry in &manifest.fragments {
            let path = dir.join(&entry.name);
            match (entry.present, path.symlink_metadata()) {
                // Declared present: it must be a regular file of the declared
                // length. A symlink is refused — it would make the generation's
                // contents depend on something outside it.
                (true, Ok(meta)) => {
                    if !meta.is_file() || Some(meta.len()) != entry.len {
                        return Ok(false);
                    }
                }
                (true, Err(_)) => return Ok(false),
                // Declared absent: it must genuinely not be there. A file where
                // the manifest says none is a different configuration.
                (false, Ok(_)) => return Ok(false),
                (false, Err(_)) => {}
            }
        }
        Ok(true)
    }

    fn generation_matches(&self, digest: &str, fragments: &[GeneratedFragment]) -> Result<bool> {
        if !self.generation_complete(digest)? {
            return Ok(false);
        }
        let Some(on_disk) = self.read_manifest(digest)? else {
            return Ok(false);
        };
        Ok(on_disk == manifest_for(digest, fragments))
    }

    fn write_pending(&mut self, journal: &PendingJournal) -> Result<()> {
        atomic_write(
            &self.root,
            PENDING_FILE,
            &serde_json::to_vec_pretty(journal)?,
        )
    }

    fn clear_pending(&mut self) -> Result<()> {
        atomic_remove(&self.root, PENDING_FILE)
    }

    fn read_receipt(&self) -> Result<Option<String>> {
        let Some(activated) = self.read_activated()? else {
            return Ok(None);
        };
        validate_generation_id(&activated)?;
        let path = self
            .root
            .join(RECEIPTS_DIR)
            .join(format!("{activated}.json"));
        Ok(read_optional(&path)?.map(|_| activated))
    }

    fn write_receipt(&mut self, digest: Option<&str>) -> Result<()> {
        let Some(digest) = digest else {
            return Ok(());
        };
        validate_generation_id(digest)?;
        let body = serde_json::json!({
            "schema": "ato.runner-ingress-activation-receipt/v1",
            "generation_id": digest,
        });
        atomic_write(
            &self.root.join(RECEIPTS_DIR),
            &format!("{digest}.json"),
            &serde_json::to_vec_pretty(&body)?,
        )
    }

    /// Point `current` at `digest`, or remove it.
    ///
    /// Refuses to publish a generation that is not complete: `current` is what
    /// the next reload reads, so pointing it at a half-written directory would
    /// hand Caddy a configuration nobody finished writing.
    fn set_current(&mut self, digest: Option<&str>) -> Result<()> {
        let link = self.root.join(CURRENT_LINK);
        let Some(digest) = digest else {
            // A first-install rollback: there is no generation to point at, and
            // leaving the link on the candidate would keep publishing something
            // that was never confirmed.
            match fs::remove_file(&link) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("remove {}", link.display()));
                }
            }
            return fsync_dir(&self.root);
        };
        if !self.generation_complete(digest)? {
            bail!("refusing to point {CURRENT_LINK} at incomplete generation {digest}");
        }
        // Relative, so the store can be moved and so the link can never be made
        // to name anything outside it.
        let target = Path::new(GENERATIONS_DIR).join(digest);
        let temp = self.root.join(temp_name(CURRENT_LINK));
        symlink(&target, &temp)?;
        fs::rename(&temp, &link).with_context(|| format!("swap {} into place", link.display()))?;
        fsync_dir(&self.root)
    }

    fn write_activated(&mut self, digest: Option<&str>) -> Result<()> {
        match digest {
            Some(digest) => {
                validate_generation_id(digest)?;
                atomic_write(&self.root, ACTIVATED_FILE, digest.as_bytes())
            }
            None => atomic_remove(&self.root, ACTIVATED_FILE),
        }
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
        .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::runner_bootstrap::official_preview::{
        PREVIEW_FRAGMENT, WIZARD_FRAGMENT,
    };

    fn store() -> (tempfile::TempDir, FsGenerationStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FsGenerationStore::open(dir.path()).expect("open");
        (dir, store)
    }

    fn both() -> Vec<GeneratedFragment> {
        vec![
            GeneratedFragment {
                file_name: PREVIEW_FRAGMENT,
                content: "preview-a".into(),
            },
            GeneratedFragment {
                file_name: WIZARD_FRAGMENT,
                content: "wizard-a".into(),
            },
        ]
    }

    fn preview_only(content: &str) -> Vec<GeneratedFragment> {
        vec![GeneratedFragment {
            file_name: PREVIEW_FRAGMENT,
            content: content.to_string(),
        }]
    }

    #[test]
    fn a_generation_is_published_atomically_and_reads_back_complete() {
        let (dir, mut store) = store();
        store.write_generation("gen-a", &both()).expect("publish");
        assert!(store.generation_complete("gen-a").expect("complete"));

        let published = dir.path().join(GENERATIONS_DIR).join("gen-a");
        assert!(published.join(PREVIEW_FRAGMENT).is_file());
        assert!(published.join(WIZARD_FRAGMENT).is_file());
        assert!(published.join(MANIFEST_FILE).is_file());
        // No temporary directory is left behind by a successful publish.
        let leftovers: Vec<_> = fs::read_dir(dir.path().join(GENERATIONS_DIR))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temporary directory was not renamed");
    }

    /// Re-publishing identical contents is a no-op success — the id names the
    /// contents, and they are the contents.
    #[test]
    fn republishing_the_same_id_with_the_same_contents_succeeds() {
        let (_dir, mut store) = store();
        store.write_generation("gen-a", &both()).expect("first");
        store.write_generation("gen-a", &both()).expect("second");
        assert!(store.generation_complete("gen-a").unwrap());
    }

    /// The same id with DIFFERENT contents is two configurations claiming one
    /// identity. Keeping either silently would make the id a lie.
    #[test]
    fn republishing_the_same_id_with_different_contents_is_refused() {
        let (_dir, mut store) = store();
        store.write_generation("gen-a", &both()).expect("first");
        let error = store
            .write_generation("gen-a", &preview_only("different"))
            .expect_err("must refuse");
        assert!(
            format!("{error:#}").contains("claiming one identity"),
            "{error:#}"
        );
    }

    /// A missing wizard fragment and an empty one are different generations,
    /// and the manifest is what keeps them apart.
    #[test]
    fn an_absent_fragment_is_distinguishable_from_an_empty_one() {
        let (dir, mut store) = store();
        store
            .write_generation("gen-absent", &preview_only("p"))
            .unwrap();
        store
            .write_generation(
                "gen-empty",
                &[
                    GeneratedFragment {
                        file_name: PREVIEW_FRAGMENT,
                        content: "p".into(),
                    },
                    GeneratedFragment {
                        file_name: WIZARD_FRAGMENT,
                        content: String::new(),
                    },
                ],
            )
            .unwrap();

        assert!(
            !dir.path()
                .join(GENERATIONS_DIR)
                .join("gen-absent")
                .join(WIZARD_FRAGMENT)
                .exists()
        );
        assert!(
            dir.path()
                .join(GENERATIONS_DIR)
                .join("gen-empty")
                .join(WIZARD_FRAGMENT)
                .is_file()
        );
        assert!(store.generation_complete("gen-absent").unwrap());
        assert!(store.generation_complete("gen-empty").unwrap());

        let absent = store.read_manifest("gen-absent").unwrap().unwrap();
        let empty = store.read_manifest("gen-empty").unwrap().unwrap();
        assert_ne!(absent.fragments, empty.fragments);
    }

    /// A fragment the manifest declares present but which is missing (or the
    /// wrong length) makes the generation incomplete — this is the state a
    /// crash mid-write leaves, and it must never be activated.
    #[test]
    fn a_missing_or_truncated_fragment_makes_the_generation_incomplete() {
        let (dir, mut store) = store();
        store.write_generation("gen-a", &both()).unwrap();
        let wizard = dir
            .path()
            .join(GENERATIONS_DIR)
            .join("gen-a")
            .join(WIZARD_FRAGMENT);

        fs::write(&wizard, "truncated").unwrap();
        assert!(
            !store.generation_complete("gen-a").unwrap(),
            "length differs"
        );

        fs::remove_file(&wizard).unwrap();
        assert!(!store.generation_complete("gen-a").unwrap(), "missing");
    }

    /// A file where the manifest declares an absence is a different
    /// configuration, so the generation is not the one the id names.
    #[test]
    fn an_unexpected_fragment_makes_the_generation_incomplete() {
        let (dir, mut store) = store();
        store
            .write_generation("gen-absent", &preview_only("p"))
            .unwrap();
        fs::write(
            dir.path()
                .join(GENERATIONS_DIR)
                .join("gen-absent")
                .join(WIZARD_FRAGMENT),
            "surprise",
        )
        .unwrap();
        assert!(!store.generation_complete("gen-absent").unwrap());
    }

    /// A crash before the rename leaves an identifiable temporary directory and
    /// no generation — never a partially-visible one.
    #[test]
    fn a_crash_before_the_rename_leaves_only_a_reclaimable_temporary() {
        let (dir, store) = store();
        let temp = dir.path().join(GENERATIONS_DIR).join(temp_name("gen-a"));
        fs::create_dir(&temp).unwrap();
        fs::write(temp.join(PREVIEW_FRAGMENT), "half").unwrap();

        assert!(!store.generation_complete("gen-a").unwrap());
        assert!(
            temp.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(TEMP_PREFIX),
            "the leftover is identifiable as temporary"
        );
    }

    #[test]
    fn current_swaps_atomically_and_reads_back() {
        let (_dir, mut store) = store();
        store.write_generation("gen-a", &both()).unwrap();
        store.write_generation("gen-b", &preview_only("b")).unwrap();

        assert_eq!(store.read_current().unwrap(), None);
        store.set_current(Some("gen-a")).unwrap();
        assert_eq!(store.read_current().unwrap().as_deref(), Some("gen-a"));
        store.set_current(Some("gen-b")).unwrap();
        assert_eq!(store.read_current().unwrap().as_deref(), Some("gen-b"));
    }

    /// A first-install rollback removes `current` rather than leaving it on a
    /// generation nobody confirmed.
    #[test]
    fn a_first_install_rollback_removes_current() {
        let (dir, mut store) = store();
        store.write_generation("gen-a", &both()).unwrap();
        store.set_current(Some("gen-a")).unwrap();

        store.set_current(None).unwrap();
        assert_eq!(store.read_current().unwrap(), None);
        assert!(!dir.path().join(CURRENT_LINK).exists());
        // Removing it again is not an error — recovery may run twice.
        store.set_current(None).unwrap();
    }

    /// `current` must never be pointed at a generation that is not finished.
    #[test]
    fn current_refuses_an_incomplete_generation() {
        let (dir, mut store) = store();
        store.write_generation("gen-a", &both()).unwrap();
        fs::remove_file(
            dir.path()
                .join(GENERATIONS_DIR)
                .join("gen-a")
                .join(WIZARD_FRAGMENT),
        )
        .unwrap();

        let error = store.set_current(Some("gen-a")).expect_err("must refuse");
        assert!(format!("{error:#}").contains("incomplete"), "{error:#}");
    }

    /// A generation id is joined onto a path, so traversal is refused at the
    /// door — including through the id of a `current` link someone planted.
    #[test]
    fn a_traversing_generation_id_is_refused() {
        let (_dir, mut store) = store();
        for id in ["../evil", "/etc/passwd", "gen/../..", "gen\0a", "", "-lead"] {
            assert!(
                store.write_generation(id, &both()).is_err(),
                "id {id:?} must be refused"
            );
            assert!(store.generation_dir(id).is_err(), "id {id:?}");
        }
    }

    /// A `current` pointing outside the store is refused rather than obeyed.
    #[cfg(unix)]
    #[test]
    fn a_current_link_that_escapes_the_store_is_refused() {
        let (dir, store) = store();
        let link = dir.path().join(CURRENT_LINK);
        symlink(Path::new("/etc"), &link).unwrap();
        assert!(store.read_current().is_err(), "an absolute target");

        fs::remove_file(&link).unwrap();
        symlink(Path::new("../../elsewhere/gen-a"), &link).unwrap();
        assert!(
            store.read_current().is_err(),
            "a target outside generations/"
        );
    }

    /// Markers survive a replace, and clearing one is idempotent.
    #[test]
    fn markers_replace_atomically_and_clear_idempotently() {
        let (_dir, mut store) = store();
        assert_eq!(store.read_activated().unwrap(), None);
        store.write_activated(Some("gen-a")).unwrap();
        assert_eq!(store.read_activated().unwrap().as_deref(), Some("gen-a"));
        store.write_activated(Some("gen-b")).unwrap();
        assert_eq!(store.read_activated().unwrap().as_deref(), Some("gen-b"));
        store.write_activated(None).unwrap();
        assert_eq!(store.read_activated().unwrap(), None);
        store.write_activated(None).unwrap();
    }

    /// The receipt is reported only for the generation the activated marker
    /// names — the pair is what tells recovery which crash point it is at.
    #[test]
    fn a_receipt_is_reported_only_alongside_its_activated_marker() {
        let (_dir, mut store) = store();
        store.write_receipt(Some("gen-a")).unwrap();
        assert_eq!(
            store.read_receipt().unwrap(),
            None,
            "no activated marker yet, so nothing is confirmed"
        );

        store.write_activated(Some("gen-a")).unwrap();
        assert_eq!(store.read_receipt().unwrap().as_deref(), Some("gen-a"));

        // Activated moved on but its receipt has not been written: exactly the
        // crash between the two.
        store.write_activated(Some("gen-b")).unwrap();
        assert_eq!(store.read_receipt().unwrap(), None);
    }

    #[test]
    fn the_pending_journal_round_trips() {
        let (_dir, mut store) = store();
        assert_eq!(store.read_pending().unwrap(), None);
        let journal = PendingJournal {
            candidate: "gen-b".into(),
            previous: Some("gen-a".into()),
            reload_succeeded: true,
        };
        store.write_pending(&journal).unwrap();
        assert_eq!(store.read_pending().unwrap(), Some(journal));
        store.clear_pending().unwrap();
        assert_eq!(store.read_pending().unwrap(), None);
        store.clear_pending().unwrap();
    }

    /// Two stores over one root serialize: the second blocks until the first
    /// releases. The lock is per ROOT (one Caddy instance), not per builder.
    #[test]
    fn concurrent_activations_are_serialized_by_the_lock() {
        use std::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let mut first = FsGenerationStore::open(dir.path()).unwrap();
        first.lock().expect("first lock");

        let (tx, rx) = mpsc::channel();
        let root = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            let mut second = FsGenerationStore::open(&root).unwrap();
            second.lock().expect("second lock");
            tx.send(()).expect("send");
        });

        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "the second activation must block while the first holds the lock"
        );
        drop(first);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the lock must be released with the store");
        handle.join().unwrap();
    }

    // ── Integration: the real store driving the real state machine ──────────
    //
    // A fake caddy executable stands in for the binary; everything else is the
    // production path, so the orderings the state machine documents are checked
    // against durable state rather than against an in-memory double.

    #[cfg(unix)]
    mod integration {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        use super::*;
        use crate::application::runner_bootstrap::caddy_control::ProcessCaddyControl;
        use crate::application::runner_bootstrap::ingress_activation::{
            ActivationOutcome, activate,
        };

        fn fake_caddy(dir: &Path, body: &str) -> PathBuf {
            let path = dir.join("fake-caddy");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
            path
        }

        fn control(dir: &Path, root: &Path, body: &str) -> ProcessCaddyControl {
            let live = root.join("live.caddy");
            fs::write(&live, "# live\n").expect("live");
            ProcessCaddyControl::new(
                fake_caddy(dir, body),
                live,
                root,
                Duration::from_secs(5),
                Duration::from_secs(5),
            )
            .expect("control")
        }

        /// A rejected candidate leaves the box exactly as it was — no journal,
        /// no receipt, `current` untouched.
        #[test]
        fn a_validate_failure_leaves_nothing_behind() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let mut store = FsGenerationStore::open(&root).unwrap();
            let mut caddy = control(dir.path(), &root, "exit 1");

            let error = activate(&mut store, &mut caddy, "gen-a", &both()).expect_err("refused");
            assert!(
                format!("{error:#}").contains("caddy validate rejected"),
                "{error:#}"
            );
            assert_eq!(store.read_current().unwrap(), None);
            assert_eq!(store.read_activated().unwrap(), None);
            assert_eq!(store.read_pending().unwrap(), None);
        }

        /// The successful path stops at `ReloadedPendingProbe`: `current` moved,
        /// the journal records that the reload landed, and NOTHING is confirmed
        /// — confirmation belongs to the probe stage.
        #[test]
        fn a_successful_activation_stops_at_reloaded_pending_probe() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let mut store = FsGenerationStore::open(&root).unwrap();
            let mut caddy = control(dir.path(), &root, "exit 0");

            let outcome = activate(&mut store, &mut caddy, "gen-a", &both()).expect("activates");
            assert_eq!(
                outcome,
                ActivationOutcome::ReloadedPendingProbe {
                    candidate: "gen-a".into(),
                    previous: None,
                }
            );
            assert_eq!(store.read_current().unwrap().as_deref(), Some("gen-a"));
            assert_eq!(
                store.read_activated().unwrap(),
                None,
                "a reload confirms nothing"
            );
            assert_eq!(store.read_receipt().unwrap(), None);
            let journal = store.read_pending().unwrap().expect("journal");
            assert!(journal.reload_succeeded);
            assert_eq!(journal.candidate, "gen-a");
            assert_eq!(journal.previous, None);
        }

        /// A reload failure on a first install rolls back to no generation at
        /// all, and leaves the store consistent.
        #[test]
        fn a_reload_failure_rolls_back_to_a_consistent_store() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let mut store = FsGenerationStore::open(&root).unwrap();
            // validate succeeds, reload fails — the argv's second element is the
            // subcommand.
            let mut caddy = control(dir.path(), &root, "[ \"$1\" = validate ] && exit 0; exit 1");

            let error = activate(&mut store, &mut caddy, "gen-a", &both()).expect_err("fails");
            assert!(
                format!("{error:#}").contains("activation failed"),
                "{error:#}"
            );
            assert_eq!(
                store.read_current().unwrap(),
                None,
                "a first install rolls back to no current at all"
            );
            assert_eq!(
                store.read_pending().unwrap(),
                None,
                "the rollback completed"
            );
            // The generation itself stays on disk — it is content-addressed and
            // harmless, and reclaiming it is a separate concern from activation.
            assert!(store.generation_complete("gen-a").unwrap());
        }

        /// An unchanged re-run over a CONFIRMED generation touches nothing.
        #[test]
        fn an_unchanged_rerun_over_a_confirmed_generation_is_a_no_op() {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let mut store = FsGenerationStore::open(&root).unwrap();
            let mut caddy = control(dir.path(), &root, "exit 0");

            activate(&mut store, &mut caddy, "gen-a", &both()).expect("activates");
            // Stand in for the probe stage confirming it.
            store.write_activated(Some("gen-a")).unwrap();
            store.write_receipt(Some("gen-a")).unwrap();
            store.clear_pending().unwrap();

            let marker = root.join("marker");
            let mut counting = control(
                dir.path(),
                &root,
                &format!("echo x >> {}; exit 0", marker.display()),
            );
            let outcome = activate(&mut store, &mut counting, "gen-a", &both()).expect("no-op");
            assert_eq!(outcome, ActivationOutcome::NoOp);
            assert!(!marker.exists(), "caddy must not have been invoked at all");
        }
    }
}
