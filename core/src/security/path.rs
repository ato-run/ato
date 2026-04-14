use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Validate a path for security.
///
/// Checks:
/// 1. Path must be absolute
/// 2. Path must be within allowed_paths
/// 3. Path must not contain traversal components (..)
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
    use std::{fs, path::PathBuf};

    #[cfg(unix)]
    use std::os::unix::fs as unix_fs;

    use tempfile::tempdir;

    use super::{parse_allowed_host_paths_csv, validate_path};

    #[test]
    fn validate_path_allows_path_in_allowlist() {
        let allowed_paths = vec![
            "/opt/models".to_string(),
            "/mnt/cache".to_string(),
            "/tmp".to_string(),
        ];

        assert!(validate_path("/opt/models/llama-3.gguf", &allowed_paths).is_ok());
        assert!(validate_path("/mnt/cache/output", &allowed_paths).is_ok());
    }

    #[test]
    fn parse_allowed_host_paths_csv_trims_normalizes_and_dedupes() {
        let v = parse_allowed_host_paths_csv(" /var/lib/gumball/ ,/tmp,/var/lib/gumball");
        assert_eq!(v, vec!["/tmp".to_string(), "/var/lib/gumball".to_string()]);
    }

    #[test]
    fn parse_allowed_host_paths_csv_drops_relative_and_traversal() {
        let v = parse_allowed_host_paths_csv("relative/path,/opt/models/../etc,/opt/models");
        assert_eq!(v, vec!["/opt/models".to_string()]);
    }

    #[test]
    fn validate_path_denies_path_not_in_allowlist() {
        let allowed_paths = vec!["/opt/models".to_string()];

        let err = validate_path("/etc/shadow", &allowed_paths).unwrap_err();
        assert!(err.contains("not in the allowed paths"));
    }

    #[test]
    fn validate_path_denies_relative_paths() {
        let allowed_paths = vec!["/opt/models".to_string()];

        let err = validate_path("relative/path", &allowed_paths).unwrap_err();
        assert!(err.contains("must be absolute"));
    }

    #[test]
    fn validate_path_denies_traversal_components() {
        let allowed_paths = vec!["/opt/models".to_string()];

        let err = validate_path("/opt/models/../etc/passwd", &allowed_paths).unwrap_err();
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
