//! Whether a checked-out source is one the v1 submission lane can carry.
//!
//! Runs after a pinned checkout and BEFORE anything that would commit to the
//! source: no tree digest, no archive, no object, no receipt, no revision row.
//! An ineligible source produces a refusal and nothing else.
//!
//! # Why this reads the git tree and not the filesystem
//!
//! A submodule that was never initialised materialises as an EMPTY DIRECTORY,
//! and `hash_node` drops recursively empty directories from their parent's
//! child list. So on the filesystem an unexpanded submodule is not merely
//! unexpanded — it is **invisible**, and the tree digest is byte-identical to a
//! tree that never declared one. Inferring from the filesystem cannot detect
//! what is not there.
//!
//! The git tree still has it: a gitlink is an entry with mode `160000`,
//! regardless of whether anything was checked out for it. That is what this
//! reads, so detection does not depend on the working tree's shape.
//!
//! # Refuse, do not materialize
//!
//! Submodules and Git LFS are REFUSED here rather than resolved. That follows
//! `SOURCE_MATERIALIZATION_SPEC` §6 ("such repos are blocked rather than
//! materialized"). Refusing is a defensible v1 scope; claiming to have
//! materialized something that was skipped is not, and that is what the
//! filesystem-derived digest was silently doing.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Git's mode for a gitlink — a submodule reference.
const GITLINK_MODE: &str = "160000";

/// A source the v1 submission lane cannot carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceIneligible {
    #[error("this repository uses Git submodules, which are not supported yet ({detail})")]
    GitSubmodule { detail: String },
    #[error("this repository uses Git LFS, which is not supported yet ({detail})")]
    GitLfs { detail: String },
    #[error("the source could not be inspected: {reason}")]
    Uninspectable { reason: String },
}

impl SourceIneligible {
    /// The stable code a client renders a message from. Distinct from the
    /// human string so a UI is not parsing prose.
    pub fn code(&self) -> &'static str {
        match self {
            Self::GitSubmodule { .. } => "UNSUPPORTED_GIT_SUBMODULE",
            Self::GitLfs { .. } => "UNSUPPORTED_GIT_LFS",
            Self::Uninspectable { .. } => "SOURCE_UNINSPECTABLE",
        }
    }
}

/// Proof that a checkout carries nothing the v1 lane cannot represent.
///
/// Private field, no public constructor: the only way to hold one is
/// [`verify_source_eligibility`]. A later stage that takes this by value cannot
/// be reached with an unverified checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEligibilityVerified(());

impl SourceEligibilityVerified {
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test() -> Self {
        Self(())
    }
}

/// Verify a pinned checkout is eligible, reading the COMMIT's tree.
///
/// `checkout` must be a git working tree whose `HEAD` is the pinned commit.
pub fn verify_source_eligibility(
    checkout: &Path,
) -> Result<SourceEligibilityVerified, SourceIneligible> {
    let entries = list_tree(checkout)?;

    // 1. Gitlinks, at ANY depth. This is the authoritative submodule check —
    //    it sees a submodule that was never initialised, which the filesystem
    //    cannot.
    for entry in &entries {
        if entry.mode == GITLINK_MODE {
            return Err(SourceIneligible::GitSubmodule {
                detail: format!("gitlink at {}", entry.path),
            });
        }
    }

    // 2. `.gitmodules` at any depth and of any kind. A repository can declare
    //    submodules whose gitlinks were stripped, and a nested one is as much a
    //    declaration as a root one — the previous check looked only at the root
    //    and only at regular files.
    for entry in &entries {
        let is_gitmodules = entry.path == ".gitmodules" || entry.path.ends_with("/.gitmodules");
        if is_gitmodules {
            return Err(SourceIneligible::GitSubmodule {
                detail: format!(".gitmodules at {}", entry.path),
            });
        }
    }

    // 3. LFS declared through git attributes. Checked before the pointer scan
    //    because a repository can declare the filter while the blobs in THIS
    //    commit happen to be small enough to look ordinary.
    for entry in &entries {
        let is_attributes =
            entry.path == ".gitattributes" || entry.path.ends_with("/.gitattributes");
        if !is_attributes {
            continue;
        }
        let body = read_blob(checkout, &entry.path)?;
        if body.contains("filter=lfs") {
            return Err(SourceIneligible::GitLfs {
                detail: format!("filter=lfs declared in {}", entry.path),
            });
        }
    }

    // 4. LFS pointer blobs, with NO size shortcut. The previous scan skipped
    //    any file over 1024 bytes without reading it, so a pointer padded past
    //    that ceiling passed as an ordinary file — and the guest then received
    //    a text stub where the real object was expected.
    for entry in &entries {
        if entry.mode == GITLINK_MODE {
            continue;
        }
        let path = checkout.join(&entry.path);
        if !path.is_file() {
            continue;
        }
        if lfs_pointer_prefix(&path)? {
            return Err(SourceIneligible::GitLfs {
                detail: format!("LFS pointer at {}", entry.path),
            });
        }
    }

    Ok(SourceEligibilityVerified(()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    mode: String,
    path: String,
}

/// `git ls-tree -r -z HEAD`, which lists gitlinks WITHOUT recursing into them.
fn list_tree(checkout: &Path) -> Result<Vec<TreeEntry>, SourceIneligible> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        // No credential helper and no prompt: this must never reach the network
        // or a keyring, and a hang here would look like a slow build.
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["ls-tree", "-r", "-z", "HEAD"])
        .output()
        .map_err(|e| SourceIneligible::Uninspectable {
            reason: format!("run git ls-tree: {e}"),
        })?;
    if !out.status.success() {
        return Err(SourceIneligible::Uninspectable {
            reason: format!(
                "git ls-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }

    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for record in out.stdout.split(|b| *b == 0) {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        // `<mode> SP <type> SP <oid> TAB <path>`
        let (meta, path) = match text.split_once('\t') {
            Some(parts) => parts,
            None => continue,
        };
        let mode = match meta.split_whitespace().next() {
            Some(mode) => mode.to_string(),
            None => continue,
        };
        if seen.insert(path.to_string()) {
            entries.push(TreeEntry {
                mode,
                path: path.to_string(),
            });
        }
    }
    Ok(entries)
}

fn read_blob(checkout: &Path, rel: &str) -> Result<String, SourceIneligible> {
    let path = checkout.join(rel);
    std::fs::read_to_string(&path).map_err(|e| SourceIneligible::Uninspectable {
        reason: format!("read {rel}: {e}"),
    })
}

/// Does this file begin with the Git-LFS pointer header?
///
/// Reads only the prefix, and reads it for EVERY file regardless of size. The
/// size shortcut it replaces was the hole: a pointer padded past the ceiling
/// was never read at all.
fn lfs_pointer_prefix(path: &Path) -> Result<bool, SourceIneligible> {
    use std::io::Read;
    const MAGIC: &[u8] = b"version https://git-lfs.github.com/spec/";
    let mut file = std::fs::File::open(path).map_err(|e| SourceIneligible::Uninspectable {
        reason: format!("open {}: {e}", path.display()),
    })?;
    let mut head = [0u8; MAGIC.len()];
    let mut filled = 0usize;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => {
                return Err(SourceIneligible::Uninspectable {
                    reason: format!("read {}: {e}", path.display()),
                });
            }
        }
    }
    Ok(&head[..filled] == MAGIC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repo with one committed file, and nothing else.
    fn repo(dir: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@example.invalid"]);
        git(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("app.py"), "print('hi')\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-qm", "init"]);
    }

    fn is_git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    #[test]
    fn an_ordinary_source_is_eligible() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        assert!(verify_source_eligibility(dir.path()).is_ok());
    }

    /// THE case the filesystem cannot see.
    ///
    /// A gitlink whose submodule was never initialised leaves an EMPTY
    /// directory, which `hash_node` drops from its parent's child list — so the
    /// tree digest is identical to a tree that never declared it. Reading the
    /// commit's tree finds the `160000` entry regardless.
    #[test]
    fn an_uninitialised_gitlink_is_refused_even_though_the_filesystem_shows_nothing() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());

        // Write a gitlink straight into the index — the shape a clone without
        // `--recurse-submodules` produces.
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0000000000000000000000000000000000000001,vendor/dep",
            ],
        );
        git(dir.path(), &["commit", "-qm", "add gitlink"]);

        // The working tree shows nothing at all there.
        assert!(
            !dir.path().join("vendor/dep").exists(),
            "fixture precondition: the gitlink is not materialized"
        );

        let err = verify_source_eligibility(dir.path()).expect_err("must refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_SUBMODULE");
    }

    /// `.gitmodules` below the root — the previous check looked only at the
    /// root, and only at regular files.
    #[test]
    fn a_nested_gitmodules_is_refused() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::write(
            dir.path().join("sub/.gitmodules"),
            "[submodule \"x\"]\n\tpath = x\n",
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "nested gitmodules"]);

        let err = verify_source_eligibility(dir.path()).expect_err("must refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_SUBMODULE");
    }

    #[test]
    fn a_root_gitmodules_is_refused() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(dir.path().join(".gitmodules"), "[submodule \"x\"]\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "gitmodules"]);
        assert_eq!(
            verify_source_eligibility(dir.path())
                .expect_err("must refuse")
                .code(),
            "UNSUPPORTED_GIT_SUBMODULE"
        );
    }

    /// LFS declared by attribute, with no pointer blob in this commit at all.
    #[test]
    fn an_lfs_attribute_is_refused_even_with_no_pointer_present() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(
            dir.path().join(".gitattributes"),
            "*.bin filter=lfs diff=lfs merge=lfs -text\n",
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "lfs attrs"]);

        let err = verify_source_eligibility(dir.path()).expect_err("must refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_LFS");
    }

    /// A pointer padded past the old 1024-byte ceiling.
    ///
    /// The previous scan returned `Ok(false)` for any file over that size
    /// WITHOUT reading it, so this exact shape passed as an ordinary file and
    /// the guest received a text stub where the real object belonged.
    #[test]
    fn an_lfs_pointer_padded_past_the_old_size_ceiling_is_refused() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        let mut pointer =
            String::from("version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 1\n");
        // Comfortably past 1024 bytes.
        pointer.push_str(&"# pad\n".repeat(400));
        assert!(pointer.len() > 1024, "fixture must exceed the old ceiling");
        fs::write(dir.path().join("model.bin"), &pointer).unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "padded pointer"]);

        let err = verify_source_eligibility(dir.path()).expect_err("must refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_LFS");
    }

    #[test]
    fn a_small_lfs_pointer_is_refused() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(
            dir.path().join("model.bin"),
            "version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 1\n",
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "pointer"]);
        assert_eq!(
            verify_source_eligibility(dir.path())
                .expect_err("must refuse")
                .code(),
            "UNSUPPORTED_GIT_LFS"
        );
    }

    /// A file that merely MENTIONS the LFS URL later in its body is not a
    /// pointer — only the header position counts.
    #[test]
    fn a_file_merely_mentioning_lfs_is_not_a_pointer() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(
            dir.path().join("README.md"),
            "See version https://git-lfs.github.com/spec/v1 for details\n",
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "readme"]);
        assert!(verify_source_eligibility(dir.path()).is_ok());
    }

    /// A non-git directory cannot be inspected, so it is refused rather than
    /// assumed eligible.
    #[test]
    fn a_non_git_directory_is_refused_rather_than_assumed_clean() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), "x").unwrap();
        let err = verify_source_eligibility(dir.path()).expect_err("must refuse");
        assert_eq!(err.code(), "SOURCE_UNINSPECTABLE");
    }
}
