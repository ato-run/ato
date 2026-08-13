use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub(crate) const SYSTEM_WORKSPACE_FILES: &[&str] = &["package.json", "package-lock.json"];
pub(crate) const SOURCE_EXCLUDED_DIRS: &[&str] =
    &["node_modules", ".vite", ".astro", ".next", "target", "dist"];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SystemCapsuleLayout {
    SourceSeed,
    DistSeed,
    FileSet,
}

#[derive(Copy, Clone)]
pub(crate) enum SeedCopyMode {
    FileSet(&'static [&'static str]),
    SourceTree,
    SourceTreeWithDist,
    StaticDist,
    WorkspaceMembers(&'static [&'static str]),
}

#[derive(Copy, Clone)]
enum ServingRootKind {
    Root,
    Subdir(&'static [&'static str]),
    CapsuleTomlRun { fallback: &'static [&'static str] },
}

#[derive(Copy, Clone)]
pub(crate) struct SystemCapsuleRegistryEntry {
    pub slug: &'static str,
    pub layout: SystemCapsuleLayout,
    pub copy_mode: SeedCopyMode,
    serving_root_kind: ServingRootKind,
}

const REGISTRY: &[SystemCapsuleRegistryEntry] = &[
    SystemCapsuleRegistryEntry {
        slug: "ato-dev-console",
        layout: SystemCapsuleLayout::FileSet,
        copy_mode: SeedCopyMode::FileSet(&["index.html"]),
        serving_root_kind: ServingRootKind::Root,
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-dock",
        layout: SystemCapsuleLayout::SourceSeed,
        copy_mode: SeedCopyMode::SourceTreeWithDist,
        serving_root_kind: ServingRootKind::CapsuleTomlRun {
            fallback: &["dist"],
        },
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-identity",
        layout: SystemCapsuleLayout::FileSet,
        copy_mode: SeedCopyMode::FileSet(&["index.html"]),
        serving_root_kind: ServingRootKind::Root,
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-import",
        layout: SystemCapsuleLayout::FileSet,
        copy_mode: SeedCopyMode::FileSet(&["index.html"]),
        serving_root_kind: ServingRootKind::Root,
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-launch",
        layout: SystemCapsuleLayout::DistSeed,
        copy_mode: SeedCopyMode::StaticDist,
        serving_root_kind: ServingRootKind::Subdir(&["dist"]),
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-onboarding",
        layout: SystemCapsuleLayout::DistSeed,
        copy_mode: SeedCopyMode::StaticDist,
        serving_root_kind: ServingRootKind::Subdir(&["dist"]),
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-settings",
        layout: SystemCapsuleLayout::FileSet,
        copy_mode: SeedCopyMode::FileSet(&["index.html"]),
        serving_root_kind: ServingRootKind::Root,
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-start",
        layout: SystemCapsuleLayout::SourceSeed,
        copy_mode: SeedCopyMode::SourceTreeWithDist,
        serving_root_kind: ServingRootKind::CapsuleTomlRun {
            fallback: &["dist"],
        },
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-store",
        layout: SystemCapsuleLayout::DistSeed,
        copy_mode: SeedCopyMode::StaticDist,
        serving_root_kind: ServingRootKind::Subdir(&["dist"]),
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-ui",
        layout: SystemCapsuleLayout::SourceSeed,
        copy_mode: SeedCopyMode::WorkspaceMembers(&["ato-launch", "ato-ui"]),
        serving_root_kind: ServingRootKind::Subdir(&["ato-ui"]),
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-web-viewer",
        layout: SystemCapsuleLayout::FileSet,
        copy_mode: SeedCopyMode::FileSet(&["index.html"]),
        serving_root_kind: ServingRootKind::Root,
    },
    SystemCapsuleRegistryEntry {
        slug: "ato-windows",
        layout: SystemCapsuleLayout::FileSet,
        copy_mode: SeedCopyMode::FileSet(&["index.html", "start.html"]),
        serving_root_kind: ServingRootKind::Root,
    },
];

#[derive(Deserialize)]
struct CapsuleRunManifest {
    run: Option<String>,
}

pub(crate) fn all() -> &'static [SystemCapsuleRegistryEntry] {
    REGISTRY
}

pub(crate) fn lookup(slug: &str) -> Option<&'static SystemCapsuleRegistryEntry> {
    REGISTRY.iter().find(|entry| entry.slug == slug)
}

pub(crate) fn resolve_serving_root(
    entry: &SystemCapsuleRegistryEntry,
    root: &Path,
) -> Result<PathBuf> {
    match entry.serving_root_kind {
        ServingRootKind::Root => Ok(root.to_path_buf()),
        ServingRootKind::Subdir(segments) => Ok(join_segments(root, segments)),
        ServingRootKind::CapsuleTomlRun { fallback } => {
            Ok(root.join(resolve_capsule_run_dir(root, fallback)?))
        }
    }
}

fn join_segments(root: &Path, segments: &[&str]) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    path
}

fn resolve_capsule_run_dir(root: &Path, fallback: &[&str]) -> Result<PathBuf> {
    let manifest_path = root.join("capsule.toml");
    let run = if manifest_path.is_file() {
        fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|contents| toml::from_str::<CapsuleRunManifest>(&contents).ok())
            .and_then(|manifest| manifest.run)
            .and_then(|run| sanitize_relative_path(&run).ok())
    } else {
        None
    };

    let sanitized = match run {
        Some(path) if !path.as_os_str().is_empty() => path,
        _ => sanitize_fallback_segments(fallback)
            .context("failed to sanitize fallback run directory")?,
    };
    Ok(sanitized)
}

fn sanitize_fallback_segments(segments: &[&str]) -> Result<PathBuf> {
    let mut path = PathBuf::new();
    for segment in segments {
        let sanitized = sanitize_relative_path(segment)
            .with_context(|| format!("invalid fallback serving segment: {segment}"))?;
        path.push(sanitized);
    }
    Ok(path)
}

fn sanitize_relative_path(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim().trim_matches('/');
    if trimmed.is_empty() {
        return Ok(PathBuf::new());
    }

    let mut path = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(segment) => path.push(segment),
            _ => anyhow::bail!("relative path contains an invalid component: {input}"),
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{SystemCapsuleLayout, lookup, resolve_serving_root};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn onboarding_registry_entry_is_dist_seed() {
        let entry = lookup("ato-onboarding").expect("onboarding entry should exist");
        assert_eq!(entry.layout, SystemCapsuleLayout::DistSeed);
    }

    #[test]
    fn start_serving_root_uses_capsule_run_field_when_present() {
        let temp = TempDir::new().expect("temp dir should exist");
        fs::write(temp.path().join("capsule.toml"), "run = \"dist/web\"\n")
            .expect("manifest should write");

        let entry = lookup("ato-start").expect("start entry should exist");
        let serving_root =
            resolve_serving_root(entry, temp.path()).expect("serving root should resolve");

        assert_eq!(serving_root, temp.path().join("dist").join("web"));
    }
}
