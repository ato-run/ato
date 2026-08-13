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

/// Verify a pinned checkout is eligible, reading the tree of an EXPLICIT commit.
///
/// `expected_commit_sha` is the sha the attempt pinned. It is named rather than
/// inferred, and the checkout's own `HEAD` is then required to equal it: a
/// branch that moved, a detached state left by a previous job, or the wrong
/// worktree would otherwise have this inspect a different tree than the one the
/// build goes on to use — and the refusal would be about the wrong source.
pub fn verify_source_eligibility(
    checkout: &Path,
    expected_commit_sha: &str,
) -> Result<SourceEligibilityVerified, SourceIneligible> {
    verify_head_matches(checkout, expected_commit_sha)?;
    let entries = list_tree(checkout, expected_commit_sha)?;

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

    // 3. LFS declared through git attributes — resolved by GIT, not by reading
    //    `.gitattributes` and searching for a substring.
    //
    //    Attribute resolution is not a text search: nested `.gitattributes`
    //    override parent ones, a later pattern overrides an earlier one, and
    //    `-filter` / `!filter` UNSET the attribute. A substring match reports
    //    LFS for a path that explicitly turned it off, and misses one that
    //    inherits it from a directory above. `check-attr --cached` applies the
    //    real rules against the pinned index rather than the working tree.
    //
    //    Kept independent of the pointer scan below: a repository can declare
    //    the filter while every blob in THIS commit is small enough to look
    //    ordinary, and a padded pointer can exist with no attribute at all.
    let lfs_paths = paths_with_lfs_filter(checkout, &entries)?;
    if let Some(path) = lfs_paths.first() {
        return Err(SourceIneligible::GitLfs {
            detail: format!("filter=lfs resolves for {path}"),
        });
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

struct GitOut {
    stdout: Vec<u8>,
}

/// One place that runs git, so every invocation inherits the same refusal to
/// touch the network or a keyring.
fn git(checkout: &Path, args: &[&str]) -> Result<Vec<u8>, SourceIneligible> {
    let out = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .map_err(|e| SourceIneligible::Uninspectable {
            reason: format!("run git {}: {e}", args.first().copied().unwrap_or("?")),
        })?;
    if !out.status.success() {
        return Err(SourceIneligible::Uninspectable {
            reason: format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or("?"),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(out.stdout)
}

/// The materialization lane's entrance.
///
/// Everything that commits to a source — the tree digest, the canonical
/// archive, the upload, the receipt, the revision row, the build dispatch —
/// lives behind this. `produce` runs ONLY on an eligible source, so "an
/// ineligible source produces a refusal and nothing else" is a property of the
/// control flow rather than of every caller remembering the order.
///
/// It exists as a seam so that property is testable from the entrance: a test
/// can assert `produce` never ran, which is a stronger statement than a unit
/// test asserting the gate returned `Err`.
pub fn materialize_if_eligible<T, E, F>(
    checkout: &Path,
    expected_commit_sha: &str,
    produce: F,
) -> Result<Result<T, E>, SourceIneligible>
where
    F: FnOnce(&SourceEligibilityVerified) -> Result<T, E>,
{
    let verified = verify_source_eligibility(checkout, expected_commit_sha)?;
    Ok(produce(&verified))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    mode: String,
    path: String,
}

/// The checkout's `HEAD` must BE the pinned commit.
fn verify_head_matches(checkout: &Path, expected: &str) -> Result<(), SourceIneligible> {
    let out = git(checkout, &["rev-parse", "HEAD"])?;
    let head = String::from_utf8_lossy(&out).trim().to_string();
    if !head.eq_ignore_ascii_case(expected) {
        return Err(SourceIneligible::Uninspectable {
            reason: format!("checkout HEAD is {head}, expected the pinned {expected}"),
        });
    }
    Ok(())
}

/// `git ls-tree -r -z --full-tree <sha>`, which lists gitlinks WITHOUT
/// recursing into them.
///
/// `--full-tree` so the listing is rooted at the tree top regardless of the
/// process working directory, and the sha is explicit rather than `HEAD`.
fn list_tree(checkout: &Path, commit: &str) -> Result<Vec<TreeEntry>, SourceIneligible> {
    let stdout = git(checkout, &["ls-tree", "-r", "-z", "--full-tree", commit])?;
    let out = GitOut { stdout };

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

/// Every path whose `filter` attribute resolves to `lfs`, per git's own rules.
///
/// `--cached` reads the index, so this reflects the pinned commit rather than
/// whatever the working tree happens to contain. `--stdin` because a repository
/// can have more paths than fit on a command line.
fn paths_with_lfs_filter(
    checkout: &Path,
    entries: &[TreeEntry],
) -> Result<Vec<String>, SourceIneligible> {
    use std::io::Write;

    let candidates: Vec<&TreeEntry> = entries.iter().filter(|e| e.mode != GITLINK_MODE).collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut child = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .arg("-c")
        .arg("credential.helper=")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["check-attr", "--cached", "--stdin", "-z", "filter"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SourceIneligible::Uninspectable {
            reason: format!("run git check-attr: {e}"),
        })?;

    {
        let mut stdin = child.stdin.take().ok_or(SourceIneligible::Uninspectable {
            reason: "git check-attr stdin unavailable".to_string(),
        })?;
        for entry in &candidates {
            let _ = stdin.write_all(entry.path.as_bytes());
            let _ = stdin.write_all(b"\0");
        }
        // Dropped here so git sees EOF and can exit.
    }

    let out = child
        .wait_with_output()
        .map_err(|e| SourceIneligible::Uninspectable {
            reason: format!("wait for git check-attr: {e}"),
        })?;
    if !out.status.success() {
        return Err(SourceIneligible::Uninspectable {
            reason: format!(
                "git check-attr failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }

    // `-z` output is a flat NUL-separated stream of (path, attr, value) triples.
    let fields: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).to_string())
        .collect();
    let mut hits = Vec::new();
    for triple in fields.chunks(3) {
        if triple.len() < 3 {
            continue;
        }
        // `unspecified` / `unset` / any other filter are all not-LFS. Only an
        // explicit `lfs` counts, which is what makes `-filter` behave.
        if triple[1] == "filter" && triple[2] == "lfs" {
            hits.push(triple[0].clone());
        }
    }
    Ok(hits)
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

    /// The checkout's current HEAD, so tests name the sha explicitly the way
    /// production does rather than relying on `HEAD` resolution.
    fn head(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
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
        assert!(verify_source_eligibility(dir.path(), &head(dir.path())).is_ok());
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

        let err =
            verify_source_eligibility(dir.path(), &head(dir.path())).expect_err("must refuse");
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

        let err =
            verify_source_eligibility(dir.path(), &head(dir.path())).expect_err("must refuse");
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
            verify_source_eligibility(dir.path(), &head(dir.path()))
                .expect_err("must refuse")
                .code(),
            "UNSUPPORTED_GIT_SUBMODULE"
        );
    }

    /// LFS declared by ATTRIBUTE, on a file whose bytes look completely
    /// ordinary.
    ///
    /// This is the case a pointer scan alone cannot catch: the blob is not a
    /// pointer, so only the attribute says the content is stored elsewhere.
    #[test]
    fn an_lfs_attribute_is_refused_even_when_the_blob_looks_ordinary() {
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
        fs::write(
            dir.path().join("model.bin"),
            "ordinary bytes, not a pointer\n",
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "lfs attrs"]);

        let err =
            verify_source_eligibility(dir.path(), &head(dir.path())).expect_err("must refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_LFS");
    }

    /// A pattern that matches NOTHING is not LFS usage.
    ///
    /// A substring search over `.gitattributes` would refuse this repository;
    /// git's own resolution does not, because no path carries the attribute.
    #[test]
    fn an_lfs_pattern_matching_no_file_is_not_lfs_usage() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(dir.path().join(".gitattributes"), "*.bin filter=lfs\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "unused lfs pattern"]);
        assert!(verify_source_eligibility(dir.path(), &head(dir.path())).is_ok());
    }

    /// `-filter` UNSETS the attribute — the path is not LFS.
    ///
    /// The case that separates real attribute resolution from a text search: a
    /// substring match sees `filter=lfs` and refuses a repository that
    /// explicitly turned it off for the only matching file.
    #[test]
    fn an_unset_filter_is_not_lfs_even_though_the_file_mentions_it() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(
            dir.path().join(".gitattributes"),
            "*.bin filter=lfs\nmodel.bin -filter\n",
        )
        .unwrap();
        fs::write(dir.path().join("model.bin"), "ordinary\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "unset filter"]);

        assert!(
            verify_source_eligibility(dir.path(), &head(dir.path())).is_ok(),
            "a path whose filter is unset is not LFS"
        );
    }

    /// A nested `.gitattributes` overrides its parent — and a nested one that
    /// ENABLES lfs is caught even though the root says nothing.
    #[test]
    fn a_nested_gitattributes_enabling_lfs_is_refused() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::create_dir_all(dir.path().join("assets")).unwrap();
        fs::write(
            dir.path().join("assets/.gitattributes"),
            "*.bin filter=lfs\n",
        )
        .unwrap();
        fs::write(dir.path().join("assets/model.bin"), "ordinary\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "nested attrs"]);

        assert_eq!(
            verify_source_eligibility(dir.path(), &head(dir.path()))
                .expect_err("must refuse")
                .code(),
            "UNSUPPORTED_GIT_LFS"
        );
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

        let err =
            verify_source_eligibility(dir.path(), &head(dir.path())).expect_err("must refuse");
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
            verify_source_eligibility(dir.path(), &head(dir.path()))
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
        assert!(verify_source_eligibility(dir.path(), &head(dir.path())).is_ok());
    }

    /// A non-git directory cannot be inspected, so it is refused rather than
    /// assumed eligible.
    #[test]
    fn a_non_git_directory_is_refused_rather_than_assumed_clean() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.py"), "x").unwrap();
        let err =
            verify_source_eligibility(dir.path(), &head(dir.path())).expect_err("must refuse");
        assert_eq!(err.code(), "SOURCE_UNINSPECTABLE");
    }

    // ── the pinned commit, not HEAD ─────────────────────────────────────────

    /// The tree is read from the NAMED sha, and a checkout parked on a
    /// different commit is refused rather than inspected.
    ///
    /// Without this, a branch that moved or a worktree left over from another
    /// job would have the gate inspect one tree while the build used another —
    /// and the refusal, or the pass, would be about the wrong source.
    #[test]
    fn a_checkout_parked_on_another_commit_is_refused() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        let first = head(dir.path());
        fs::write(dir.path().join("second.py"), "x\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "second"]);

        // HEAD is now the second commit; the attempt pinned the first.
        let err = verify_source_eligibility(dir.path(), &first).expect_err("must refuse");
        assert_eq!(err.code(), "SOURCE_UNINSPECTABLE");
        assert!(
            err.to_string().contains(&first),
            "the refusal names the pinned sha: {err}"
        );
    }

    /// The eligibility verdict follows the PINNED commit, not the working tree.
    #[test]
    fn the_verdict_is_taken_from_the_pinned_commit() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        fs::write(dir.path().join(".gitmodules"), "[submodule \"x\"]\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "add gitmodules"]);
        let bad = head(dir.path());

        // Naming the bad commit refuses; HEAD equals it, so this is a real read
        // of that tree rather than of the working directory.
        assert_eq!(
            verify_source_eligibility(dir.path(), &bad)
                .expect_err("must refuse")
                .code(),
            "UNSUPPORTED_GIT_SUBMODULE"
        );
    }

    // ── the lane entrance: a refusal produces NOTHING ───────────────────────

    /// An ineligible source produces a refusal and no side effects.
    ///
    /// Asserted from the lane ENTRANCE rather than by checking that the gate
    /// returns `Err`. A unit test on the gate cannot show that nothing
    /// downstream ran; this can, because `produce` is the only way anything
    /// downstream happens and it records whether it was called.
    #[test]
    fn an_ineligible_source_never_reaches_the_producer() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        // A gitlink: invisible on the filesystem, refused from the tree.
        git(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0000000000000000000000000000000000000001,vendor/dep",
            ],
        );
        git(dir.path(), &["commit", "-qm", "gitlink"]);
        let sha = head(dir.path());

        let outputs = std::cell::RefCell::new(Vec::<&'static str>::new());
        let out_dir = tempfile::tempdir().unwrap();

        let result = materialize_if_eligible(dir.path(), &sha, |_verified| {
            // Everything that would commit to this source.
            outputs.borrow_mut().push("tree_digest");
            outputs.borrow_mut().push("archive");
            outputs.borrow_mut().push("upload");
            outputs.borrow_mut().push("receipt");
            outputs.borrow_mut().push("source_revision");
            outputs.borrow_mut().push("attempt_attach");
            outputs.borrow_mut().push("build_dispatch");
            std::fs::write(out_dir.path().join("archive.tar.zst"), b"x").unwrap();
            Ok::<(), ()>(())
        });

        let err = result.expect_err("an ineligible source must refuse");
        assert_eq!(err.code(), "UNSUPPORTED_GIT_SUBMODULE");
        assert!(
            outputs.borrow().is_empty(),
            "the producer ran for an ineligible source: {:?}",
            outputs.borrow()
        );
        assert!(
            std::fs::read_dir(out_dir.path()).unwrap().next().is_none(),
            "an ineligible source left an artifact behind"
        );
    }

    /// The converse, so the guard is not vacuously passing by never running.
    #[test]
    fn an_eligible_source_does_reach_the_producer() {
        if !is_git_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        repo(dir.path());
        let sha = head(dir.path());
        let ran = std::cell::Cell::new(false);
        let out = materialize_if_eligible(dir.path(), &sha, |_v| {
            ran.set(true);
            Ok::<u8, ()>(7)
        })
        .expect("eligible")
        .expect("produced");
        assert!(ran.get());
        assert_eq!(out, 7);
    }
}
