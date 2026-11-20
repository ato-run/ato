//! Private working and saved data for foreground Runs. The bundle cache and
//! immutable application identity are independent of the local generations.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail, ensure};
use ato_portable_application::{ValidatedPortableApplication, bundle_sha256};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    RunArgs, ato_home, display_path_argument, hub_public, hub_transfer, read_local_bundle,
};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(super) enum ExitPolicy {
    Save,
    Discard,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Target {
    Local {
        path: PathBuf,
    },
    Hub {
        name: String,
        selector: String,
        api: String,
    },
}

impl Target {
    pub(super) fn new(path: &Path, api: &str) -> Result<Self> {
        if !path.exists()
            && let Some(value) = path.to_str()
            && let Some(reference) = hub_public::HubRef::parse(value)?
        {
            return Ok(Self::Hub {
                name: reference.reference(),
                selector: match &reference.selector {
                    hub_public::HubSelector::Tag(tag) => format!(":{tag}"),
                    hub_public::HubSelector::Digest(digest) => format!("@{digest}"),
                },
                api: hub_public::api_base(api)?,
            });
        }
        let absolute = std::path::absolute(path)?;
        // The directory's canonical path is stable while the caller replaces
        // the bundle (which may itself be a moving symlink).
        let canonical = match (absolute.parent(), absolute.file_name()) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf())
                .join(name),
            _ => absolute,
        };
        Ok(Self::Local { path: canonical })
    }

    fn command(&self) -> String {
        match self {
            Self::Local { path } => format!("ato run {}", display_path_argument(path)),
            Self::Hub {
                name,
                selector,
                api,
            } => {
                let reference = format!("{name}{selector}");
                format!(
                    "ato run {} --api-url {}",
                    display_path_argument(Path::new(&reference)),
                    display_path_argument(Path::new(api))
                )
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    schema: u32,
    title: String,
    target: Target,
    digest: String,
    size: u64,
    derivation: String,
    saved: Option<String>,
    pending: Option<String>,
    #[serde(default)]
    discarding: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerResult {
    pub(super) stopped: bool,
    pub(super) error: Option<String>,
}

pub(super) struct Selection {
    pub(super) target: Target,
    pub(super) selected: Option<LockedEntry>,
}

pub(super) struct LockedEntry {
    id: String,
    root: PathBuf,
    entry: Entry,
    _lock: File,
}

pub(super) struct Draft {
    pub(super) path: PathBuf,
}

impl Draft {
    pub(super) fn runtime(&self) -> PathBuf {
        self.path.join("run")
    }

    pub(super) fn worker_result(&self) -> Result<Option<WorkerResult>> {
        read_optional(&self.path.join("worker-result.json"))
    }

    pub(super) fn finish_worker(&self, result: &WorkerResult) -> Result<()> {
        write_json(&self.path.join("worker-result.json"), result)
    }

    pub(super) fn lock_worker(&self) -> Result<File> {
        let generation = self.generation()?;
        try_lock(&root()?.join("locks").join(format!("run-{generation}")))
    }

    fn generation(&self) -> Result<&str> {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| is_hex(name, 32))
            .context("Invalid Run working copy ID")
    }

    pub(super) fn check_pending(&self) -> Result<()> {
        let directory = self.path.parent().context("Missing local data directory")?;
        ensure!(
            directory.parent() == Some(root()?.join("entries").as_path()),
            "Run data is outside the local store"
        );
        let entry = read_entry(directory)?.context("Run data no longer exists")?;
        ensure!(
            entry.pending.as_deref() == Some(self.generation()?) && !entry.discarding,
            "This working copy no longer owns a Run"
        );
        Ok(())
    }
}

pub(super) fn select(args: &RunArgs) -> Result<Selection> {
    let target = Target::new(&args.capsule, &args.api_url)?;
    if args.fresh {
        return Ok(Selection {
            target,
            selected: None,
        });
    }
    let relevant: Vec<_> = entries()?
        .into_iter()
        .filter(|(_, entry)| entry.target == target)
        .collect();
    let id = if let Some(id) = &args.data_id {
        id.clone()
    } else if relevant.is_empty() {
        return Ok(Selection {
            target,
            selected: None,
        });
    } else {
        eprintln!("Local data for this app:");
        for (number, (id, entry)) in relevant.iter().enumerate() {
            let status = match (&entry.saved, &entry.pending) {
                (Some(_), Some(_)) => "saved, with work awaiting a decision",
                (_, Some(_)) => "awaiting a decision",
                _ => "saved",
            };
            eprintln!(
                "  {}. {} ({id}) · {status} · {}",
                number + 1,
                entry.title,
                entry.digest
            );
        }
        if !crate::stdin_is_a_terminal() {
            bail!(
                "Choose --data-id <ID> to continue an existing state, or --fresh to start independently"
            );
        }
        loop {
            let answer = ask("Choose a number, N for a new state, or Q to quit")?;
            match answer.to_ascii_lowercase().as_str() {
                "n" | "new" => {
                    return Ok(Selection {
                        target,
                        selected: None,
                    });
                }
                "q" | "quit" | "" => bail!("No app was started"),
                _ => {}
            }
            if let Some(index) = answer
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1))
                && let Some((id, _)) = relevant.get(index)
            {
                break id.clone();
            }
            eprintln!("Enter a listed number, N, or Q.");
        }
    };
    let mut selected = open(&id)?;
    ensure!(
        selected.entry.target == target,
        "Data {id} belongs to a different app reference"
    );
    if let Some(reference) = &args.derivation {
        ensure!(
            reference == &selected.entry.derivation,
            "Data {id} belongs to a different route. Use --fresh to choose another route"
        );
    }
    if selected.entry.pending.is_some() {
        let draft = selected.stopped_draft()?;
        eprintln!("Recovered undecided local data for {id}.");
        let action = if crate::stdin_is_a_terminal() {
            ask_policy(
                Some("Resolve this working copy before starting the app."),
                None,
            )?
        } else {
            None
        };
        let Some(action) = action else {
            bail!(
                "Data {id} still awaits a decision. Use ato app recover {id} <save|discard>, or start with --fresh"
            );
        };
        selected.apply_policy(&draft, action)?;
        if selected.entry.saved.is_none() {
            return Ok(Selection {
                target,
                selected: None,
            });
        }
    }
    Ok(Selection {
        target,
        selected: Some(selected),
    })
}

impl LockedEntry {
    pub(super) fn derivation(&self) -> &str {
        &self.entry.derivation
    }

    pub(super) fn source(&self) -> Result<(PathBuf, Option<hub_public::ResolvedHubCapsule>)> {
        let home = ato_home()?;
        let digest = &self.entry.digest;
        eprintln!(
            "Opening saved data {} with its original bundle {digest}. Use --fresh to open a different revision.",
            self.id
        );
        let cached = hub_transfer::verified_cache_path(&home, digest, self.entry.size)?;
        if let Some(path) = cached {
            eprintln!("Using the verified bundle cache.");
            let hub = if let Target::Hub { name, api, .. } = &self.entry.target {
                let reference = hub_public::HubRef::parse(&format!("{name}@{digest}"))?
                    .context("Invalid saved Hub reference")?;
                Some(hub_public::ResolvedHubCapsule {
                    reference,
                    digest: digest.clone(),
                    api_url: api.clone(),
                    bundle_path: path.clone(),
                })
            } else {
                None
            };
            return Ok((path, hub));
        }
        eprintln!("The saved revision is missing or invalid in the bundle cache.");
        match &self.entry.target {
            Target::Local { path } => {
                let bytes = read_local_bundle(path, true).with_context(|| {
                    format!(
                        "Restore the original local bundle with digest {digest} at {}",
                        path.display()
                    )
                })?;
                ensure!(
                    bundle_sha256(&bytes) == *digest,
                    "The local file has changed. Restore the original bundle {digest} at {} to continue this data. Use --fresh for the current file",
                    path.display()
                );
                Ok((hub_public::store_bundle(&home, digest, &bytes)?, None))
            }
            Target::Hub { name, api, .. } => {
                let pinned = format!("{name}@{digest}");
                let resolved = hub_public::resolve_for_run_with_progress(
                    Path::new(&pinned),
                    api,
                    &home,
                    true,
                )?
                .context("The Hub could not provide the saved revision")?;
                ensure!(
                    resolved.digest == *digest,
                    "The Hub returned a different revision. Saved data was kept"
                );
                Ok((resolved.bundle_path.clone(), Some(resolved)))
            }
        }
    }

    pub(super) fn create(
        target: Target,
        route: &ValidatedPortableApplication,
        bytes: &[u8],
    ) -> Result<Self> {
        let root = root()?;
        fs::create_dir_all(root.join("entries"))?;
        let id = token()?;
        let lock = entry_lock(&id)?;
        let directory = root.join("entries").join(&id);
        create_private(&directory)?;
        let entry = Entry {
            schema: 1,
            title: route.application.title.clone(),
            target,
            digest: bundle_sha256(bytes),
            size: bytes.len().try_into()?,
            derivation: route.derivation_ref.as_str().to_owned(),
            saved: None,
            pending: None,
            discarding: false,
        };
        write_json(&directory.join("entry.json"), &entry)?;
        Ok(Self {
            id,
            root: directory,
            entry,
            _lock: lock,
        })
    }

    pub(super) fn begin(
        &mut self,
        route: &ValidatedPortableApplication,
        bundle: &ato_objects::PortableApplicationBundle,
        bytes: &[u8],
    ) -> Result<(Draft, PathBuf)> {
        ensure!(
            self.entry.pending.is_none(),
            "Resolve the existing work before starting another Run"
        );
        ensure!(
            bundle_sha256(bytes) == self.entry.digest,
            "The bundle changed before the Run started"
        );
        let path = hub_public::store_bundle(&ato_home()?, &self.entry.digest, bytes)?;
        let generation = token()?;
        let draft = Draft {
            path: self.root.join(&generation),
        };
        create_private(&draft.path)?;
        let prepared = (|| -> Result<()> {
            if let Some(saved) = &self.entry.saved {
                let source = self.root.join(saved);
                if source.join("snapshot").exists() {
                    copy_tree(&source.join("snapshot"), &draft.path.join("snapshot"))?;
                } else {
                    copy_tree(&source.join("state"), &draft.path.join("state"))?;
                }
            } else if bundle.index.instance_snapshot_ref.is_some() {
                let store = snapshot_store(&draft)?;
                let instance = store.import(bytes, Some(route.derivation_ref.as_str()))?;
                write_json(
                    &draft.path.join("snapshot").join("instance-id.json"),
                    &instance.instance_id,
                )?;
            }
            if !draft.path.join("snapshot").exists() {
                fs::create_dir_all(draft.path.join("state"))?;
                for slot in &route.derivation.state {
                    checked_slot(&slot.id)?;
                    let path = draft.path.join("state").join(&slot.id);
                    if self.entry.saved.is_none() {
                        fs::create_dir(&path)?;
                    }
                    ensure!(
                        fs::symlink_metadata(&path)?.is_dir(),
                        "State slot {} is not a directory",
                        slot.id
                    );
                }
            }
            self.entry.pending = Some(generation);
            self.write()?;
            Ok(())
        })();
        if let Err(error) = prepared {
            let _ = fs::remove_dir_all(&draft.path);
            self.entry.pending = None;
            return Err(error);
        }
        Ok((draft, path))
    }

    pub(super) fn pending_message(&self, draft: &Draft) {
        eprintln!(
            "Kept undecided local data {} at {}. Use ato app recover {} <save|discard> after the Run has stopped.",
            self.id,
            draft.path.display(),
            self.id
        );
    }

    pub(super) fn apply_policy(&mut self, draft: &Draft, policy: ExitPolicy) -> Result<()> {
        let _worker = draft.lock_worker()?;
        self.check_stopped(draft)?;
        let pending = self
            .entry
            .pending
            .clone()
            .context("There is no pending data")?;
        ensure!(
            draft.path == self.root.join(&pending),
            "Working copy changed"
        );
        match policy {
            ExitPolicy::Save => {
                ensure!(
                    !self.entry.discarding,
                    "Deletion already started. Retry with ato app recover {} discard",
                    self.id
                );
                seal_snapshot(draft)
                    .context("Could not save this snapshot. The working copy was kept")?;
                let contents = if draft.path.join("snapshot").exists() {
                    draft.path.join("snapshot")
                } else {
                    draft.path.join("state")
                };
                sync_tree(&contents)
                    .context("Could not save local files. The working copy was kept")?;
                let mut updated = self.entry.clone();
                let previous = updated.saved.replace(pending);
                updated.pending = None;
                write_json(&self.root.join("entry.json"), &updated)
                    .context("Could not publish saved data. The working copy was kept")?;
                self.entry = updated;
                if let Some(previous) = previous
                    && let Err(error) = fs::remove_dir_all(self.root.join(previous))
                {
                    eprintln!(
                        "Saved the new state. The previous local copy could not be removed: {error}"
                    );
                }
                println!("Saved local data for {} ({}).", self.entry.title, self.id);
                println!(
                    "Open it again with {} --data-id {}",
                    self.entry.target.command(),
                    self.id
                );
                println!("See saved data with ato app storage.");
            }
            ExitPolicy::Discard => {
                self.entry.discarding = true;
                self.write()?;
                match fs::remove_dir_all(&draft.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "Could not finish deleting local data at {}",
                                draft.path.display()
                            )
                        });
                    }
                }
                self.entry.pending = None;
                self.entry.discarding = false;
                if self.entry.saved.is_some() {
                    self.write()?;
                } else {
                    fs::remove_dir_all(&self.root)?;
                }
                println!(
                    "Deleted the working copy {}. Bundle cache retained.",
                    self.id
                );
                if self.entry.saved.is_some() {
                    println!("The previous saved state is available.");
                }
            }
        }
        Ok(())
    }

    pub(super) fn confirm_stopped(&self, draft: &Draft) -> Result<()> {
        let _worker = draft.lock_worker().with_context(|| {
            format!(
                "Run {} is still stopping or its owner is active. Retry after it stops",
                self.id
            )
        })?;
        self.check_stopped(draft)
    }

    fn check_stopped(&self, draft: &Draft) -> Result<()> {
        if self.entry.discarding {
            return Ok(());
        }
        if draft.worker_result()?.is_some_and(|result| result.stopped)
            || !draft
                .runtime()
                .join(ato_portable_application::local_instance::LOCAL_RUN_STOP_PENDING_FILE)
                .try_exists()?
                && !draft.path.join("worker-started.json").try_exists()?
        {
            return Ok(());
        }
        bail!(
            "Cannot confirm that Run {} stopped. Its data stays at {}. See Run evidence at {}",
            self.id,
            draft.path.display(),
            draft.runtime().display()
        );
    }

    fn stopped_draft(&self) -> Result<Draft> {
        let pending = self
            .entry
            .pending
            .as_ref()
            .context("There is no pending data")?;
        let draft = Draft {
            path: self.root.join(pending),
        };
        self.confirm_stopped(&draft)?;
        Ok(draft)
    }

    fn write(&self) -> Result<()> {
        write_json(&self.root.join("entry.json"), &self.entry)
    }
}

pub(super) fn ask_policy(
    prefix: Option<&str>,
    cancelled: Option<&AtomicBool>,
) -> Result<Option<ExitPolicy>> {
    if let Some(prefix) = prefix {
        eprintln!("{prefix}");
    }
    eprintln!("Save keeps this local working copy for the next Run.");
    eprintln!(
        "Discard deletes this working copy. Previous saves, the bundle cache, and data in external services are unaffected."
    );
    loop {
        let prompt = "Save, Discard, or Later [s/d/L]";
        let answer = if let Some(cancelled) = cancelled {
            eprint!("{prompt} ");
            io::stderr().flush()?;
            crate::run_bindings::read_visible(cancelled)?
                .unwrap_or_default()
                .trim()
                .to_owned()
        } else {
            ask(prompt)?
        };
        match answer.to_ascii_lowercase().as_str() {
            "s" | "save" => return Ok(Some(ExitPolicy::Save)),
            "d" | "discard" => return Ok(Some(ExitPolicy::Discard)),
            "l" | "later" | "" => return Ok(None),
            _ => eprintln!("Enter S, D, or L."),
        }
    }
}

pub(super) fn recover(id: &str, policy: ExitPolicy) -> Result<()> {
    let mut entry = open(id)?;
    let draft = entry.stopped_draft()?;
    entry.apply_policy(&draft, policy)
}

pub(super) fn remove(id: &str) -> Result<()> {
    let entry = open(id)?;
    ensure!(
        entry.entry.pending.is_none(),
        "Resolve pending work first with ato app recover {id} <save|discard>"
    );
    fs::remove_dir_all(&entry.root)
        .with_context(|| format!("Could not finish deleting data {id}"))?;
    println!("Deleted local data {id}. Bundle cache retained.");
    Ok(())
}

pub(super) fn storage() -> Result<()> {
    println!("Data saved or held by ato run:");
    let mut total = 0u64;
    for (id, entry) in entries()? {
        let bytes = tree_bytes(&root()?.join("entries").join(&id))?;
        total = total.saturating_add(bytes);
        let status = match (&entry.saved, &entry.pending) {
            (Some(_), Some(_)) => "saved and pending",
            (None, Some(_)) => "pending",
            (Some(_), None) => "saved",
            _ => "empty",
        };
        println!("  {id}  {bytes} bytes  {status}  {}", entry.title);
    }
    println!("Local app data total: {total} bytes");
    println!(
        "Bundle cache: {} bytes",
        cache_files()?.iter().map(|(_, bytes)| *bytes).sum::<u64>()
    );
    println!(
        "Delete saved data with ato app data-remove <DATA_ID>. Resolve pending data with ato app recover <DATA_ID> <save|discard>."
    );
    println!("Clear downloaded bundles with ato app cache-clear.");
    Ok(())
}

pub(super) fn clear_cache() -> Result<()> {
    let mut total = 0u64;
    for (path, bytes) in cache_files()? {
        fs::remove_file(&path)
            .with_context(|| format!("Could not remove cache file {}", path.display()))?;
        total += bytes;
    }
    println!("Removed {total} bytes from the bundle cache. Saved data retained.");
    Ok(())
}

fn cache_files() -> Result<Vec<(PathBuf, u64)>> {
    let mut paths = Vec::new();
    for entry in read_dir_if_present(&ato_home()?.join("hub"))?
        .into_iter()
        .flatten()
    {
        let entry = entry?;
        let name = entry.file_name();
        let is_bundle = name
            .to_str()
            .and_then(|value| value.strip_suffix(".capsule"))
            .is_some_and(|hex| is_hex(hex, 64));
        if is_bundle && entry.file_type()?.is_file() {
            paths.push((entry.path(), entry.metadata()?.len()));
        }
    }
    Ok(paths)
}

fn entries() -> Result<Vec<(String, Entry)>> {
    let mut result = Vec::new();
    for directory in read_dir_if_present(&root()?.join("entries"))?
        .into_iter()
        .flatten()
    {
        let directory = directory?;
        let Some(id) = directory
            .file_name()
            .to_str()
            .filter(|name| is_hex(name, 32))
            .map(str::to_owned)
        else {
            continue;
        };
        if directory.file_type()?.is_dir()
            && let Some(entry) = read_entry(&directory.path())?
            && (entry.saved.is_some() || entry.pending.is_some())
        {
            result.push((id, entry));
        }
    }
    result.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(result)
}

fn open(id: &str) -> Result<LockedEntry> {
    ensure!(is_hex(id, 32), "Invalid data ID. See ato app storage");
    let lock = entry_lock(id)?;
    let directory = root()?.join("entries").join(id);
    ensure!(
        fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()),
        "No local data with ID {id}. See ato app storage"
    );
    let entry = read_entry(&directory)?.with_context(|| {
        format!(
            "Local data {id} has missing metadata at {}",
            directory.display()
        )
    })?;
    Ok(LockedEntry {
        id: id.to_owned(),
        root: directory,
        entry,
        _lock: lock,
    })
}

fn read_entry(path: &Path) -> Result<Option<Entry>> {
    let Some(entry): Option<Entry> = read_optional(&path.join("entry.json"))? else {
        return Ok(None);
    };
    ensure!(
        entry.schema == 1
            && entry
                .saved
                .iter()
                .chain(&entry.pending)
                .all(|generation| is_hex(generation, 32))
            && !matches!((&entry.saved, &entry.pending), (Some(saved), Some(pending)) if saved == pending)
            && hub_public::parse_digest(&entry.digest).is_ok(),
        "Unrecognized or damaged local data metadata at {}. Files were kept",
        path.display()
    );
    Ok(Some(entry))
}

fn entry_lock(id: &str) -> Result<File> {
    let locks = root()?.join("locks");
    fs::create_dir_all(&locks)?;
    try_lock(&locks.join(id)).with_context(|| format!("Local data {id} is in use"))
}

fn try_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file)?;
    Ok(file)
}

fn root() -> Result<PathBuf> {
    Ok(ato_home()?.join("run-data"))
}

pub(super) fn planned_runtime_root() -> Result<PathBuf> {
    let path = root()?;
    Ok(path.canonicalize().unwrap_or(path).join("entries"))
}

fn create_private(path: &Path) -> Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder
        .create(path)
        .with_context(|| format!("Create private data directory {}", path.display()))
}

pub(super) fn token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("Could not get secure randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn checked_slot(value: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    ensure!(
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none(),
        "Invalid state slot name"
    );
    Ok(())
}

pub(super) fn write_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing data parent directory")?)?;
    serde_json::to_writer(temporary.as_file_mut(), data)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .with_context(|| format!("Publish {}", path.display()))?;
    crate::hub::sync_directory_best_effort(path.parent().expect("parent already checked"));
    Ok(())
}

pub(super) fn read_optional<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("Damaged or unrecognized data at {}", path.display())
        })?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Read {}", path.display())),
    }
}

fn read_dir_if_present(path: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(iter) => Ok(Some(iter)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("Read directory {}", path.display())),
    }
}

fn tree_bytes(path: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            bytes = bytes.saturating_add(tree_bytes(&entry.path())?);
        } else if kind.is_file() {
            bytes = bytes.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(bytes)
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    ensure!(
        metadata.is_dir(),
        "Saved data at {} is not a directory",
        source.display()
    );
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).with_context(|| format!("Copy saved file {}", from.display()))?;
        } else {
            bail!(
                "Cannot safely copy the link or special file at {}. Saved data was kept",
                from.display()
            );
        }
    }
    fs::set_permissions(destination, metadata.permissions())?;
    Ok(())
}

fn sync_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            sync_tree(&path)?;
        } else if kind.is_file() {
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            options.write(true);
            options
                .open(&path)?
                .sync_all()
                .with_context(|| format!("Flush local file {}", path.display()))?;
        } else {
            bail!("Cannot save the link or special file at {}", path.display());
        }
    }
    crate::hub::sync_directory_best_effort(path);
    Ok(())
}

fn ask(prompt: &str) -> Result<String> {
    eprint!("{prompt} ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().to_owned())
}

fn snapshot_store(
    draft: &Draft,
) -> Result<ato_portable_application::local_instance::LocalApplicationStore> {
    Ok(
        ato_portable_application::local_instance::LocalApplicationStore::open(
            draft.path.join("snapshot"),
        )?,
    )
}

fn seal_snapshot(draft: &Draft) -> Result<()> {
    use ato_portable_application::local_instance::LocalInstanceRunStatus;

    let path = draft.path.join("snapshot").join("instance-id.json");
    let Some(id): Option<String> = read_optional(&path)? else {
        return Ok(());
    };
    let store = snapshot_store(draft)?;
    let claim = match store.active_run(&id)? {
        // Only this post-stop save path creates claims in the private snapshot
        // store. A dead client may leave the claim before it can release it.
        Some(claim) if claim.status == LocalInstanceRunStatus::Starting && claim.pid.is_none() => {
            claim
        }
        Some(_) => bail!("The snapshot still has an active or unconfirmed Run"),
        None => store.claim_run(&id)?,
    };
    let saved = (|| -> Result<()> {
        if let Some(browser) =
            read_optional::<BTreeMap<String, String>>(&draft.path.join("browser.json"))?
        {
            store.save_browser_state_for_run(&id, &claim.token, &browser)?;
        }
        store.save_filesystem_state_for_run(&id, &claim.token)?;
        Ok(())
    })();
    let release = store.release_run(&id, &claim.token);
    saved?;
    release?;
    Ok(())
}

pub(super) fn snapshot_mounts(
    draft: &Draft,
) -> Result<
    Option<(
        ato_portable_application::local_instance::LocalApplicationStore,
        String,
    )>,
> {
    let Some(id) = read_optional(&draft.path.join("snapshot").join("instance-id.json"))? else {
        return Ok(None);
    };
    Ok(Some((snapshot_store(draft)?, id)))
}
