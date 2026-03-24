use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};

pub(crate) mod execution;
pub(crate) mod test_temp;

pub(crate) fn unpack_capsule_into_directory(capsule_bytes: &[u8], root_dir: &Path) -> Result<()> {
    if root_dir.exists() {
        fs::remove_dir_all(root_dir)
            .with_context(|| format!("failed to remove existing sandbox {}", root_dir.display()))?;
    }
    fs::create_dir_all(root_dir)
        .with_context(|| format!("failed to create sandbox {}", root_dir.display()))?;

    let mut capsule_archive = tar::Archive::new(Cursor::new(capsule_bytes));
    capsule_archive
        .unpack(root_dir)
        .with_context(|| format!("failed to unpack capsule into {}", root_dir.display()))?;

    let payload_zst_path = root_dir.join("payload.tar.zst");
    if payload_zst_path.exists() {
        let mut decoder = zstd::stream::Decoder::new(
            fs::File::open(&payload_zst_path)
                .with_context(|| format!("failed to open {}", payload_zst_path.display()))?,
        )
        .context("failed to create payload decoder")?;
        let mut payload_tar = Vec::new();
        decoder
            .read_to_end(&mut payload_tar)
            .context("failed to decode payload.tar.zst")?;
        let mut payload_archive = tar::Archive::new(Cursor::new(payload_tar));
        payload_archive
            .unpack(root_dir)
            .with_context(|| format!("failed to expand payload into {}", root_dir.display()))?;
        fs::remove_file(&payload_zst_path).ok();
    }

    Ok(())
}
