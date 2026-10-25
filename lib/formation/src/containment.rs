//! Path containment for trees that land on a disk.
//!
//! One rule, shared by every tree Formation writes out — a source tree
//! (`source`, resolver v2) and a built artifact (`formation-worker`'s
//! `pack`) — so the two cannot come to disagree about what "contained" means.
//! Only the rule is shared: each tree keeps its own identity (a source tree
//! digest is not an artifact digest).

use std::path::Path;

/// Why a symlink target is not contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymlinkEscape {
    pub reason: &'static str,
}

/// The target of a symlink at `link`, a tree-relative path, if the link
/// stays inside the tree however it is followed. Returned verbatim: the
/// string is what the tree records, not its resolution.
///
/// Contained means: non-empty UTF-8, no NUL, relative (no leading `/`, no
/// `\`), and shaped as zero or more `..` followed only by plain names — no
/// `..` after a name — with no more `..` than `link` has parent directories.
///
/// The shape, not a lexical resolution, is what keeps *chains* of links
/// inside: `s -> .` and `a -> s/s/../..` both resolve inside lexically, but
/// `a` climbs out through `s` on a disk. A leading `..` walks up real
/// directories only (provided nothing in the tree lies beneath a symlink —
/// the caller's rule for archives; a filesystem walk that does not follow
/// links cannot find anything beneath one), and plain names never walk up.
pub fn validate_contained_symlink_target(
    link: &Path,
    target: &[u8],
) -> Result<String, SymlinkEscape> {
    let escape = |reason: &'static str| Err(SymlinkEscape { reason });
    if target.is_empty() {
        return escape("empty target");
    }
    if target.contains(&0) {
        return escape("NUL in target");
    }
    let Ok(text) = std::str::from_utf8(target) else {
        return escape("target is not UTF-8");
    };
    if text.starts_with('/') || text.contains('\\') {
        return escape("absolute target");
    }
    let parents = link.components().count().saturating_sub(1);
    let mut climbs = 0usize;
    let mut descended = false;
    for segment in text.split('/') {
        match segment {
            "" | "." => {}
            ".." if descended => return escape("climbs after descending"),
            ".." => {
                climbs += 1;
                if climbs > parents {
                    return escape("climbs out of the tree");
                }
            }
            _ => descended = true,
        }
    }
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contained_and_escaping_targets() {
        for (link, target) in [
            ("alias", "real"),
            ("assets/current", "versions/v1"),
            ("foo/link", "../shared"),
            ("sub/link", ".."),
            ("link", "./a/b"),
        ] {
            assert!(
                validate_contained_symlink_target(Path::new(link), target.as_bytes()).is_ok(),
                "{link} -> {target}"
            );
        }
        for (link, target) in [
            ("link", "/etc/passwd"),
            ("link", "/opt/ato/toolchains/python/3.12.7/bin/python3"),
            ("link", ".."),
            ("sub/link", "../../outside"),
            ("link", "a/../b"),
            ("link", "s/s/../.."),
            ("link", ""),
            ("link", "a\\\\b"),
        ] {
            assert!(
                validate_contained_symlink_target(Path::new(link), target.as_bytes()).is_err(),
                "{link} -> {target}"
            );
        }
        assert!(validate_contained_symlink_target(Path::new("link"), b"a\0b").is_err());
    }
}
