use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

pub fn resolve_skill_path(skill_name: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to get current directory")?;
    let home = dirs::home_dir().context("failed to resolve home directory")?;
    resolve_skill_path_with_roots(skill_name, &cwd, &home)
}

fn resolve_skill_path_with_roots(skill_name: &str, cwd: &Path, home: &Path) -> Result<PathBuf> {
    let workspace = cwd
        .join(".agents")
        .join("skills")
        .join(skill_name)
        .join("SKILL.md");
    if workspace.is_file() {
        return Ok(workspace);
    }

    let mcp_workspace = cwd
        .join(".mcp")
        .join("skills")
        .join(skill_name)
        .join("SKILL.md");
    if mcp_workspace.is_file() {
        return Ok(mcp_workspace);
    }

    let global = home
        .join(".config")
        .join("agents")
        .join("skills")
        .join(skill_name)
        .join("SKILL.md");
    if global.is_file() {
        return Ok(global);
    }

    if let Some(store) = resolve_from_capsule_store(skill_name, home)? {
        return Ok(store);
    }

    Err(AtoExecutionError::skill_not_found(
        format!(
            "Skill '{}' not found. Searched in:\n  - {}\n  - {}\n  - {}\n  - {}/.ato/store/*/{}/<version>/source/SKILL.md",
            skill_name,
            workspace.display(),
            mcp_workspace.display(),
            global.display(),
            home.display(),
            skill_name
        ),
        Some(skill_name),
    )
    .into())
}

fn resolve_from_capsule_store(skill_name: &str, home: &Path) -> Result<Option<PathBuf>> {
    let store_root = home.join(".ato").join("store");
    if !store_root.is_dir() {
        return Ok(None);
    }

    let mut candidates: Vec<(PathBuf, VersionKey)> = Vec::new();
    for publisher_entry in fs::read_dir(&store_root).with_context(|| {
        format!(
            "failed to read capsule store root directory: {}",
            store_root.display()
        )
    })? {
        let publisher_entry = publisher_entry?;
        if !publisher_entry.file_type()?.is_dir() {
            continue;
        }

        let slug_dir = publisher_entry.path().join(skill_name);
        if !slug_dir.is_dir() {
            continue;
        }

        for version_entry in fs::read_dir(&slug_dir)
            .with_context(|| format!("failed to read slug dir: {}", slug_dir.display()))?
        {
            let version_entry = version_entry?;
            if !version_entry.file_type()?.is_dir() {
                continue;
            }
            let version_name = version_entry.file_name().to_string_lossy().to_string();
            let skill_path = version_entry.path().join("source").join("SKILL.md");
            if skill_path.is_file() {
                candidates.push((skill_path, parse_version_key(&version_name)));
            }
        }
    }

    candidates.sort_by(|(_, a), (_, b)| compare_version_keys(b, a));
    Ok(candidates.into_iter().next().map(|(path, _)| path))
}

#[derive(Debug, Clone)]
struct VersionKey {
    parts: Vec<u64>,
    raw: String,
}

fn parse_version_key(raw: &str) -> VersionKey {
    let mut parts = Vec::new();
    for p in raw.split('.') {
        let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(v) = digits.parse::<u64>() {
            parts.push(v);
        } else {
            break;
        }
    }
    VersionKey {
        parts,
        raw: raw.to_string(),
    }
}

fn compare_version_keys(a: &VersionKey, b: &VersionKey) -> Ordering {
    let max = a.parts.len().max(b.parts.len());
    for i in 0..max {
        let av = a.parts.get(i).copied().unwrap_or(0);
        let bv = b.parts.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.raw.cmp(&b.raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolves_workspace_first() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        let path = dir
            .path()
            .join(".agents")
            .join("skills")
            .join("demo")
            .join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# demo").unwrap();

        let resolved = resolve_skill_path_with_roots("demo", dir.path(), home.path()).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn resolves_global_when_workspace_missing() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        let path = home
            .path()
            .join(".config")
            .join("agents")
            .join("skills")
            .join("demo")
            .join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# demo").unwrap();

        let resolved = resolve_skill_path_with_roots("demo", dir.path(), home.path()).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn resolves_mcp_workspace_when_agents_missing() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        let path = dir
            .path()
            .join(".mcp")
            .join("skills")
            .join("demo")
            .join("SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# demo mcp").unwrap();

        let resolved = resolve_skill_path_with_roots("demo", dir.path(), home.path()).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn resolves_latest_store_version() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();
        let v1 = home
            .path()
            .join(".ato")
            .join("store")
            .join("alice")
            .join("demo")
            .join("1.2.0")
            .join("source")
            .join("SKILL.md");
        let v2 = home
            .path()
            .join(".ato")
            .join("store")
            .join("alice")
            .join("demo")
            .join("1.10.0")
            .join("source")
            .join("SKILL.md");
        fs::create_dir_all(v1.parent().unwrap()).unwrap();
        fs::create_dir_all(v2.parent().unwrap()).unwrap();
        fs::write(&v1, "# old").unwrap();
        fs::write(&v2, "# new").unwrap();

        let resolved = resolve_skill_path_with_roots("demo", dir.path(), home.path()).unwrap();
        assert_eq!(resolved, v2);
    }

    #[test]
    fn returns_structured_skill_not_found_error() {
        let dir = tempdir().unwrap();
        let home = tempdir().unwrap();

        let err = resolve_skill_path_with_roots("missing-skill", dir.path(), home.path())
            .expect_err("must fail when skill is absent");
        let ato_err = err
            .downcast_ref::<AtoExecutionError>()
            .expect("must be AtoExecutionError");
        assert_eq!(ato_err.code, "ATO_ERR_SKILL_NOT_FOUND");
        assert_eq!(ato_err.resource.as_deref(), Some("skill"));
        assert_eq!(ato_err.target.as_deref(), Some("missing-skill"));
    }
}
