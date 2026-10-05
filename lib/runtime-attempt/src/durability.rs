//! Durable same-filesystem publication shared by Runtime records and state.
//! Callers sync the payload before publishing it. Unix syncs the containing
//! directories; Windows uses the existing journal's write-through move.

#[cfg(windows)]
use anyhow::ensure;
use anyhow::{Context, Result};
#[cfg(not(windows))]
use std::fs::File;
use std::path::Path;

#[cfg(not(windows))]
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?
        .sync_all()
        .context("sync publication directory")
}

#[cfg(not(windows))]
pub fn rename(temporary: &Path, path: &Path) -> Result<()> {
    std::fs::rename(temporary, path)
        .with_context(|| format!("cannot publish into {}", path.display()))?;
    let dir = path.parent().context("record has no directory")?;
    sync_directory(dir)?;
    let source_dir = temporary.parent().context("move source has no directory")?;
    if source_dir != dir {
        sync_directory(source_dir)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn rename(temporary: &Path, path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    // COPY_ALLOWED and DELAY_UNTIL_REBOOT are deliberately absent: the synced
    // temporary and destination must be replaced now on the same filesystem.
    ensure!(
        temporary.parent().is_some() && path.parent().is_some(),
        "durable move has no parent"
    );
    let wide_path = |value: &Path| -> Result<Vec<u16>> {
        let mut wide: Vec<u16> = value.as_os_str().encode_wide().collect();
        ensure!(!wide.contains(&0), "move path contains a null character");
        wide.push(0);
        Ok(wide)
    };
    let temporary = wide_path(temporary)?;
    let destination = wide_path(path)?;
    // SAFETY: both owned UTF-16 buffers are null-terminated, contain no inner
    // null, and remain alive throughout this synchronous Win32 call.
    let moved = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("cannot durably publish into {}", path.display()));
    }
    Ok(())
}
