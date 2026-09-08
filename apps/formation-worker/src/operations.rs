//! Bounded, pre-build source sidecar. Parsing and execution stay in the common
//! operation registrar/isolated executor; this worker only transfers source.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

const MAX_BYTES: u64 = 576 * 1024;
const MAX_FILES: usize = 65;
const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Serialize)]
pub struct SourceFile {
    path: String,
    content: String,
}
#[derive(Debug, Serialize)]
pub struct OperationSource {
    files: Vec<SourceFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<&'static str>,
}

/// A declaration makes any incomplete scan fatal. Without one, an over-budget
/// scan yields no inferred operations and an explicit unsupported diagnostic.
pub fn collect(root: &Path) -> Result<OperationSource> {
    let explicit = root.join("ato.operations.yaml").try_exists()?;
    match scan(root) {
        Ok(files) => Ok(OperationSource {
            files,
            diagnostic: None,
        }),
        Err(error) if explicit => Err(error.context("explicit operation source is incomplete")),
        Err(_) => Ok(OperationSource {
            files: vec![],
            diagnostic: Some("source_scan_limit"),
        }),
    }
}

fn scan(root: &Path) -> Result<Vec<SourceFile>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut bytes = 0;
    let mut entries = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= MAX_ENTRIES, "operation source scan limit");
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF8 source path")?;
            // These paths cannot contribute to operation source. The verified
            // closure already enforces credential exclusion during capture.
            if name.starts_with('.')
                || matches!(
                    name,
                    "node_modules" | "credentials" | "id_rsa" | "id_ed25519"
                )
            {
                continue;
            }
            let kind = entry.file_type()?;
            ensure!(!kind.is_symlink(), "operation source symlink");
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF8 source path")?
                .replace('\\', "/");
            let declaration = relative == "ato.operations.yaml";
            if !declaration
                && !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("js" | "mjs" | "html")
                )
            {
                continue;
            }
            let size = entry.metadata()?.len();
            if declaration {
                ensure!(size <= 64 * 1024, "operation declaration too large");
            }
            bytes += size;
            ensure!(
                bytes <= MAX_BYTES && files.len() < MAX_FILES,
                "operation source limit"
            );
            let content = std::fs::read_to_string(path).context("operation source must be UTF8")?;
            files.push(SourceFile {
                path: relative,
                content,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_declaration_and_modules_without_loading_large_assets() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("ato.operations.yaml"),
            "schema: ato.operations/v1",
        )?;
        std::fs::write(
            root.path().join("op.js"),
            "export const run=()=>({operations:[]})",
        )?;
        let asset = std::fs::File::create(root.path().join("image.png"))?;
        asset.set_len(6 * 1024 * 1024)?;
        let source = collect(root.path())?;
        assert_eq!(source.files.len(), 2);
        assert!(source.diagnostic.is_none());
        Ok(())
    }
    #[test]
    fn incomplete_explicit_source_fails_without_discovery_fallback() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("large.js"),
            vec![b' '; MAX_BYTES as usize + 1],
        )?;
        assert_eq!(collect(root.path())?.diagnostic, Some("source_scan_limit"));
        std::fs::write(root.path().join("ato.operations.yaml"), "operations: []")?;
        assert!(collect(root.path()).is_err());
        Ok(())
    }
}
