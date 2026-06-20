use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Validate a path for security.
///
/// Checks:
/// 1. Path must be absolute
/// 2. Path must not contain traversal components (`..`)
/// 3. The canonicalized form of the path must be within one of `allowed_paths`
///
/// # Security
///
/// **TOCTOU constraint**: this function canonicalizes the path at call time using
/// [`std::fs::canonicalize`]. There is an inherent race between the time of check
/// and the time of use: an attacker who can replace a path component with a symlink
/// between this call and the subsequent file operation may bypass the check.
/// Callers must either hold an exclusive lock on the relevant directory tree or
/// accept this residual risk in low-privilege contexts.
///
/// Symbolic links that exist *at check time* are detected correctly because
/// canonicalization follows them and the resulting absolute path is then compared
/// against the allow-list.
pub fn validate_path(path_str: &str, allowed_paths: &[String]) -> Result<(), String> {
    let path = Path::new(path_str);

    if !path.is_absolute() {
        return Err(format!("Path must be absolute: {}", path_str));
    }

    for component in path.components() {
        if let Component::ParentDir = component {
            return Err(format!("Path traversal detected: {}", path_str));
        }
    }

    let allowed_canon: Vec<PathBuf> = allowed_paths
        .iter()
        .filter_map(|p| {
            let ap = Path::new(p);
            if !ap.is_absolute() {
                return None;
            }
            if ap.exists() {
                fs::canonicalize(ap).ok()
            } else {
                let mut cur = Some(ap);
                while let Some(c) = cur {
                    if c.exists() {
                        return fs::canonicalize(c).ok();
                    }
                    cur = c.parent();
                }
                None
            }
        })
        .collect();

    let (existing_prefix, canonical_prefix) = if path.exists() {
        (
            path.to_path_buf(),
            fs::canonicalize(path)
                .map_err(|e| format!("Failed to canonicalize '{}': {}", path_str, e))?,
        )
    } else {
        let mut cur = path;
        while !cur.exists() {
            cur = cur
                .parent()
                .ok_or_else(|| format!("Failed to find existing ancestor for '{}'", path_str))?;
        }
        (
            cur.to_path_buf(),
            fs::canonicalize(cur)
                .map_err(|e| format!("Failed to canonicalize ancestor of '{}': {}", path_str, e))?,
        )
    };

    let remainder = path
        .strip_prefix(&existing_prefix)
        .map_err(|_| format!("Failed to compute path remainder for '{}'", path_str))?;
    let canonical_candidate = canonical_prefix.join(remainder);

    let allowed = allowed_canon
        .iter()
        .any(|allowed_root| canonical_candidate.starts_with(allowed_root));

    if !allowed {
        return Err(format!(
            "Path '{}' is not in the allowed paths: {:?}",
            path_str, allowed_paths
        ));
    }

    Ok(())
}

/// Parse a CSV allowlist for host filesystem paths.
///
/// Preserves absolute paths, trims whitespace, normalizes trailing slashes,
/// drops relative paths, and de-dupes.
#[allow(dead_code)]
pub fn parse_allowed_host_paths_csv(value: &str) -> Vec<String> {
    let mut out: Vec<String> = value
        .split(',')
        .filter_map(|raw| {
            let s = raw.trim();
            if s.is_empty() {
                return None;
            }

            let normalized = if s.len() > 1 {
                s.trim_end_matches('/')
            } else {
                s
            };

            let path = Path::new(normalized);
            if !path.is_absolute() {
                return None;
            }

            if path.components().any(|c| matches!(c, Component::ParentDir)) {
                return None;
            }

            Some(normalized.to_string())
        })
        .collect();

    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    // Only the unix-gated symlink-escape test below needs these.
    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;
    #[cfg(unix)]
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{parse_allowed_host_paths_csv, validate_path};

    /// Host-absolute spelling for a unix-style test path: `/opt/models`
    /// stays as-is on unix and becomes `C:/opt/models` on Windows (drive-
    /// prefixed forward slashes are absolute there and keep the `/`-based
    /// normalization in `parse_allowed_host_paths_csv` exercised).
    fn host_abs(unix: &str) -> String {
        if cfg!(windows) {
            format!("C:{unix}")
        } else {
            unix.to_string()
        }
    }

    #[test]
    fn validate_path_allows_path_in_allowlist() {
        // Anchor the allowlist at real directories so the existing-ancestor
        // fallback in canonicalization plays no role in the outcome.
        let temp = tempdir().expect("tempdir");
        let models = temp.path().join("models");
        let cache = temp.path().join("cache");
        fs::create_dir_all(&models).expect("models dir");
        fs::create_dir_all(&cache).expect("cache dir");
        let allowed_paths = vec![
            models.to_string_lossy().to_string(),
            cache.to_string_lossy().to_string(),
        ];

        assert!(
            validate_path(
                &models.join("llama-3.gguf").to_string_lossy(),
                &allowed_paths
            )
            .is_ok()
        );
        assert!(validate_path(&cache.join("output").to_string_lossy(), &allowed_paths).is_ok());
    }

    #[test]
    fn parse_allowed_host_paths_csv_trims_normalizes_and_dedupes() {
        let gumball = host_abs("/var/lib/gumball");
        let tmp = host_abs("/tmp");
        let v = parse_allowed_host_paths_csv(&format!(" {gumball}/ ,{tmp},{gumball}"));
        assert_eq!(v, vec![tmp, gumball]);
    }

    #[test]
    fn parse_allowed_host_paths_csv_drops_relative_and_traversal() {
        let models = host_abs("/opt/models");
        let traversal = host_abs("/opt/models/../etc");
        let v = parse_allowed_host_paths_csv(&format!("relative/path,{traversal},{models}"));
        assert_eq!(v, vec![models]);
    }

    #[test]
    fn validate_path_denies_path_not_in_allowlist() {
        // Both roots exist so the deny verdict comes from the allowlist
        // comparison, not from a canonicalization fallback.
        let temp = tempdir().expect("tempdir");
        let models = temp.path().join("models");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&models).expect("models dir");
        fs::create_dir_all(&outside).expect("outside dir");
        let allowed_paths = vec![models.to_string_lossy().to_string()];

        let err =
            validate_path(&outside.join("shadow").to_string_lossy(), &allowed_paths).unwrap_err();
        assert!(err.contains("not in the allowed paths"));
    }

    #[test]
    fn validate_path_denies_relative_paths() {
        let allowed_paths = vec![host_abs("/opt/models")];

        let err = validate_path("relative/path", &allowed_paths).unwrap_err();
        assert!(err.contains("must be absolute"));
    }

    #[test]
    fn validate_path_denies_traversal_components() {
        let allowed_paths = vec![host_abs("/opt/models")];

        let err =
            validate_path(&host_abs("/opt/models/../etc/passwd"), &allowed_paths).unwrap_err();
        assert!(err.contains("Path traversal detected"));
    }

    #[test]
    #[cfg(unix)]
    fn validate_path_denies_symlink_escape_when_path_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let allowed_root = temp.path().join("allowed");
        let outside_root = temp.path().join("outside");

        fs::create_dir_all(&allowed_root).expect("create allowed");
        fs::create_dir_all(&outside_root).expect("create outside");

        let secret = outside_root.join("secret.txt");
        fs::write(&secret, "top-secret").expect("write secret");

        let link = allowed_root.join("link");
        unix_fs::symlink(&outside_root, &link).expect("create symlink");

        let attack_path: PathBuf = link.join("secret.txt");
        let allowed_paths = vec![allowed_root.to_string_lossy().to_string()];

        let err = validate_path(
            attack_path
                .to_str()
                .expect("attack path should be valid UTF-8"),
            &allowed_paths,
        )
        .unwrap_err();

        assert!(err.contains("not in the allowed paths"));
    }
}
