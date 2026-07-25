//! Fail-closed **lexical** presence of the canonical lock control files.
//!
//! Two code paths decide which lock file is authoritative at a capsule root:
//! the input resolver (`routing::input_resolver`, spec §5 Amendment) and the
//! program source projection (`contract::program_source_projection`, ADR-014
//! §1). They must agree on every input, so the decision lives here — in
//! Layer 1 — instead of in either consumer: putting it in `routing` would make
//! `contract` depend upward on Layer 3, and putting it in `contract` would do
//! the same to `routing`.
//!
//! Two properties this helper exists to guarantee:
//!
//! * **Presence is lexical.** A dangling symlink, a directory, or a FIFO at a
//!   lock NAME counts as present. `Path::exists()` follows links and reports a
//!   dangling `capsule.lock` as absent, which silently downgrades a split-brain
//!   root to "alias only" — a divergence from the projection's
//!   `symlink_metadata` view. Coexistence is judged on NAMES; the *selected*
//!   lock is then required to be a regular file.
//! * **Only `NotFound` means absent.** Every other `symlink_metadata` error
//!   (`PermissionDenied`, `NotADirectory`, …) propagates. Collapsing them into
//!   "absent" would let an unreadable `capsule.lock` be silently ignored.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Canonical lock file name (see CAPSULE_V1_EXECUTION_MODEL_SPEC §5).
pub const CAPSULE_LOCK_FILE_NAME: &str = "capsule.lock";
/// Deprecated pre-amendment name for the canonical lock. Still accepted as a
/// read-compatible alias (with a rename warning); writers must never produce
/// new files under this name.
pub const DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME: &str = "ato.lock.json";

/// The kind of a node that is present under a name but is not a regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonRegularNode {
    Directory,
    /// A symlink — dangling or not. The link is never followed.
    Symlink,
    /// Device, socket, or FIFO.
    Other,
}

impl NonRegularNode {
    /// `None` for a regular file; otherwise the node kind.
    fn classify(file_type: fs::FileType) -> Option<Self> {
        if file_type.is_file() {
            None
        } else if file_type.is_dir() {
            Some(Self::Directory)
        } else if file_type.is_symlink() {
            Some(Self::Symlink)
        } else {
            Some(Self::Other)
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "a directory",
            Self::Symlink => "a symlink",
            Self::Other => "an unsupported node type",
        }
    }
}

impl fmt::Display for NonRegularNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether one filesystem NAME is occupied, and by what.
///
/// The distinction between [`Self::PresentRegularFile`] and
/// [`Self::PresentInvalidNode`] never affects *presence*: both are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexicalEntryState {
    Absent,
    PresentRegularFile,
    PresentInvalidNode(NonRegularNode),
}

impl LexicalEntryState {
    pub fn is_present(self) -> bool {
        !matches!(self, Self::Absent)
    }
}

/// The lexical state of `path`: `symlink_metadata` (never follows a link),
/// with **only** [`io::ErrorKind::NotFound`] mapped to
/// [`LexicalEntryState::Absent`]. Every other error propagates.
pub fn lexical_entry_state(path: &Path) -> io::Result<LexicalEntryState> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(match NonRegularNode::classify(metadata.file_type()) {
            None => LexicalEntryState::PresentRegularFile,
            Some(kind) => LexicalEntryState::PresentInvalidNode(kind),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(LexicalEntryState::Absent),
        Err(source) => Err(source),
    }
}

/// Which canonical lock name is authoritative at a root (spec §5 Amendment /
/// ADR-014 §1): `capsule.lock` only, the deprecated `ato.lock.json` alias
/// only, or neither. Both present is [`CanonicalLockSelectionError::Coexistence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalLockSelection {
    Absent,
    Canonical(PathBuf),
    DeprecatedAlias(PathBuf),
}

impl CanonicalLockSelection {
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Absent => None,
            Self::Canonical(path) | Self::DeprecatedAlias(path) => Some(path),
        }
    }

    pub fn into_path(self) -> Option<PathBuf> {
        match self {
            Self::Absent => None,
            Self::Canonical(path) | Self::DeprecatedAlias(path) => Some(path),
        }
    }

    pub fn is_deprecated_alias(&self) -> bool {
        matches!(self, Self::DeprecatedAlias(_))
    }
}

/// Why a root has no single authoritative lock path. Each consumer maps these
/// onto its own error type; the discrimination is shared so both paths reach
/// the same verdict for the same tree.
#[derive(Debug, Error)]
pub enum CanonicalLockSelectionError {
    #[error(
        "both {CAPSULE_LOCK_FILE_NAME} and {DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME} exist at {root}"
    )]
    Coexistence { root: PathBuf },
    #[error("{path} must be a regular file, found {kind}")]
    NotRegularFile { path: PathBuf, kind: NonRegularNode },
    #[error("failed to inspect {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Selects the one authoritative lock path at `root`, fail closed:
///
/// ```text
/// capsule.lock name occupied only    → Canonical
/// ato.lock.json name occupied only   → DeprecatedAlias (caller warns)
/// both names occupied                → Coexistence (split-brain)
/// neither                            → Absent
/// ```
///
/// Occupancy is lexical, so a dangling `capsule.lock` symlink beside a valid
/// `ato.lock.json` is split-brain, not "alias only". The selected path is then
/// required to be a regular file — a directory or symlink under the selected
/// lock name is [`CanonicalLockSelectionError::NotRegularFile`], never
/// silently accepted (in the projection it would otherwise be excluded from
/// the digest wholesale).
pub fn select_canonical_lock_path(
    root: &Path,
) -> Result<CanonicalLockSelection, CanonicalLockSelectionError> {
    let canonical = root.join(CAPSULE_LOCK_FILE_NAME);
    let alias = root.join(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME);
    let canonical_state = entry_state(&canonical)?;
    let alias_state = entry_state(&alias)?;

    match (canonical_state.is_present(), alias_state.is_present()) {
        (true, true) => Err(CanonicalLockSelectionError::Coexistence {
            root: root.to_path_buf(),
        }),
        (true, false) => {
            require_regular_file(&canonical, canonical_state)?;
            Ok(CanonicalLockSelection::Canonical(canonical))
        }
        (false, true) => {
            require_regular_file(&alias, alias_state)?;
            Ok(CanonicalLockSelection::DeprecatedAlias(alias))
        }
        (false, false) => Ok(CanonicalLockSelection::Absent),
    }
}

/// [`lexical_entry_state`] with the path attached to any I/O failure. Callers
/// outside the lock-selection rule use [`lexical_entry_state`] and attach their
/// own error context.
fn entry_state(path: &Path) -> Result<LexicalEntryState, CanonicalLockSelectionError> {
    lexical_entry_state(path).map_err(|source| CanonicalLockSelectionError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn require_regular_file(
    path: &Path,
    state: LexicalEntryState,
) -> Result<(), CanonicalLockSelectionError> {
    match state {
        LexicalEntryState::PresentInvalidNode(kind) => {
            Err(CanonicalLockSelectionError::NotRegularFile {
                path: path.to_path_buf(),
                kind,
            })
        }
        LexicalEntryState::Absent | LexicalEntryState::PresentRegularFile => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(root: &Path, name: &str) {
        fs::write(root.join(name), b"{}\n").expect("write file");
    }

    #[test]
    fn absent_only_for_not_found() {
        let dir = TempDir::new().expect("tempdir");
        assert_eq!(
            lexical_entry_state(&dir.path().join("missing")).expect("state"),
            LexicalEntryState::Absent
        );
    }

    #[test]
    fn regular_file_and_directory_are_distinguished() {
        let dir = TempDir::new().expect("tempdir");
        write(dir.path(), "file");
        fs::create_dir(dir.path().join("subdir")).expect("mkdir");

        assert_eq!(
            lexical_entry_state(&dir.path().join("file")).expect("state"),
            LexicalEntryState::PresentRegularFile
        );
        assert_eq!(
            lexical_entry_state(&dir.path().join("subdir")).expect("state"),
            LexicalEntryState::PresentInvalidNode(NonRegularNode::Directory)
        );
    }

    /// The divergence the shared helper exists to kill: `exists()` follows the
    /// link and calls a dangling `capsule.lock` absent, so the resolver used to
    /// pick the alias while the projection rejected the pair.
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_present_and_makes_coexistence() {
        let dir = TempDir::new().expect("tempdir");
        std::os::unix::fs::symlink("nowhere", dir.path().join(CAPSULE_LOCK_FILE_NAME))
            .expect("symlink");
        write(dir.path(), DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME);

        assert!(!dir.path().join(CAPSULE_LOCK_FILE_NAME).exists());
        assert_eq!(
            lexical_entry_state(&dir.path().join(CAPSULE_LOCK_FILE_NAME)).expect("state"),
            LexicalEntryState::PresentInvalidNode(NonRegularNode::Symlink)
        );
        assert!(matches!(
            select_canonical_lock_path(dir.path()),
            Err(CanonicalLockSelectionError::Coexistence { .. })
        ));
    }

    #[test]
    fn selects_canonical_alias_or_neither() {
        let neither = TempDir::new().expect("tempdir");
        assert_eq!(
            select_canonical_lock_path(neither.path()).expect("select"),
            CanonicalLockSelection::Absent
        );

        let canonical = TempDir::new().expect("tempdir");
        write(canonical.path(), CAPSULE_LOCK_FILE_NAME);
        let selection = select_canonical_lock_path(canonical.path()).expect("select");
        assert!(!selection.is_deprecated_alias());
        assert_eq!(
            selection.path(),
            Some(canonical.path().join(CAPSULE_LOCK_FILE_NAME).as_path())
        );

        let alias = TempDir::new().expect("tempdir");
        write(alias.path(), DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME);
        let selection = select_canonical_lock_path(alias.path()).expect("select");
        assert!(selection.is_deprecated_alias());
        assert_eq!(
            selection.path(),
            Some(
                alias
                    .path()
                    .join(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME)
                    .as_path()
            )
        );
    }

    #[test]
    fn selected_lock_that_is_not_a_regular_file_is_rejected() {
        let dir = TempDir::new().expect("tempdir");
        fs::create_dir(dir.path().join(CAPSULE_LOCK_FILE_NAME)).expect("mkdir");

        let error = select_canonical_lock_path(dir.path()).expect_err("must reject");
        assert!(
            matches!(
                error,
                CanonicalLockSelectionError::NotRegularFile {
                    kind: NonRegularNode::Directory,
                    ..
                }
            ),
            "{error}"
        );
    }

    /// A permission error must never read as "absent". Skipped when the test
    /// process can read a `0o000` directory anyway (running as root).
    #[cfg(unix)]
    #[test]
    fn permission_denied_propagates_instead_of_absent() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("tempdir");
        let sealed = dir.path().join("sealed");
        fs::create_dir(&sealed).expect("mkdir");
        write(&sealed, CAPSULE_LOCK_FILE_NAME);
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o000)).expect("chmod");

        let readable_anyway = fs::read_dir(&sealed).is_ok();
        let outcome = lexical_entry_state(&sealed.join(CAPSULE_LOCK_FILE_NAME));
        // Restore before asserting so the TempDir can always be cleaned up.
        fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700)).expect("restore");

        if readable_anyway {
            return;
        }
        let error = outcome.expect_err("an unreadable parent must not read as absent");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    }
}
