use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::{DEFAULT_PREVIEW_DIR, ENV_PREVIEW_ROOT, PREVIEW_MANIFEST_FILE_NAME};
use crate::application::preview::PreviewSession;

pub fn persist_session_with_warning(session: &PreviewSession) -> Option<String> {
    session
        .persist()
        .err()
        .map(|error| format!("Failed to persist preview session metadata: {error}"))
}

pub fn preview_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(ENV_PREVIEW_ROOT) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return Ok(path);
        }
    }

    capsule_core::common::paths::ato_path("previews").context("Failed to determine ato home")
}

pub fn load_preview_session_for_manifest(manifest_path: &Path) -> Result<Option<PreviewSession>> {
    if manifest_path.file_name().and_then(|value| value.to_str())
        != Some(PREVIEW_MANIFEST_FILE_NAME)
    {
        return Ok(None);
    }

    let root = preview_root()?;
    let session_root = match manifest_path.parent() {
        Some(path) => path,
        None => return Ok(None),
    };
    if !session_root.starts_with(&root) {
        return Ok(None);
    }

    let metadata_path = session_root.join(super::PREVIEW_METADATA_FILE_NAME);
    if !metadata_path.exists() {
        return Ok(None);
    }

    PreviewSession::load(&metadata_path).map(Some)
}
