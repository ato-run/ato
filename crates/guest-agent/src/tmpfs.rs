//! PR 3: tmpfs binding-file delivery — `/run/ato/bindings/<name>`, mode `0600`.
//!
//! The guest-agent materializes each binding as a `0600` file under a tmpfs root
//! (nothing touches a persistent/overlay disk). Writes are atomic (tmp + rename);
//! scrub overwrites then unlinks. Binding names are already validated single path
//! components ([`BindingName`]), so there is no path traversal.

use std::io::{self, Write};
use std::path::PathBuf;

use protocol::binding_lease::BindingName;

use crate::BindingSink;

/// Default guest tmpfs root for binding files (per the contract).
pub const DEFAULT_BINDINGS_ROOT: &str = "/run/ato/bindings";

/// Writes bindings as `0600` files under `root` (a tmpfs mount in the guest; a tempdir
/// in tests).
pub struct TmpfsBindingSink {
    root: PathBuf,
}

impl TmpfsBindingSink {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        TmpfsBindingSink { root: root.into() }
    }

    /// The production sink at `/run/ato/bindings`.
    pub fn at_default() -> Self {
        TmpfsBindingSink::new(DEFAULT_BINDINGS_ROOT)
    }

    fn path(&self, name: &BindingName) -> PathBuf {
        // name is a validated single component ([a-z0-9_.-], never '.'/'..'/'/').
        self.root.join(name.as_str())
    }
}

#[cfg(unix)]
impl BindingSink for TmpfsBindingSink {
    fn deliver(&self, name: &BindingName, value: &str) -> io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let final_path = self.path(name);
        let tmp = self.root.join(format!(".{}.tmp", name.as_str()));
        {
            // Create with 0600 from the start — the secret is never briefly world-readable.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(value.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &final_path)
    }

    fn scrub(&self, name: &BindingName) -> io::Result<()> {
        let path = self.path(name);
        match std::fs::metadata(&path) {
            Ok(meta) => {
                // Best-effort wipe before unlink so the bytes don't linger.
                if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&path) {
                    let zeros = vec![0u8; meta.len() as usize];
                    let _ = f.write_all(&zeros);
                    let _ = f.sync_all();
                }
                std::fs::remove_file(&path)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(not(unix))]
impl BindingSink for TmpfsBindingSink {
    fn deliver(&self, _name: &BindingName, _value: &str) -> io::Result<()> {
        Err(io::Error::other("tmpfs binding delivery is unix-only"))
    }
    fn scrub(&self, _name: &BindingName) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn name(n: &str) -> BindingName {
        BindingName::parse(n).unwrap()
    }

    #[test]
    fn deliver_writes_0600_file_then_scrub_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let sink = TmpfsBindingSink::new(dir.path().join("bindings"));
        let n = name("db_url");
        sink.deliver(&n, "postgres://secret").unwrap();

        let path = dir.path().join("bindings").join("db_url");
        assert!(path.exists(), "binding file created");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "postgres://secret");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "binding file must be 0600, got {mode:o}");
        // no leftover tmp file.
        assert!(!dir.path().join("bindings").join(".db_url.tmp").exists());

        sink.scrub(&n).unwrap();
        assert!(!path.exists(), "scrub removed the binding file");
        // scrub is idempotent.
        sink.scrub(&n).unwrap();
    }

    #[test]
    fn deliver_is_atomic_replace_on_renew() {
        let dir = tempfile::tempdir().unwrap();
        let sink = TmpfsBindingSink::new(dir.path());
        let n = name("token");
        sink.deliver(&n, "v1").unwrap();
        sink.deliver(&n, "v2-renewed").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("token")).unwrap(),
            "v2-renewed"
        );
    }
}
