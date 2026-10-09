//! Bounded range batches leave the resumable file as a contiguous prefix.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};

use super::{CHUNK_RETRIES, CountedRead};

const MAX_RANGE_DOWNLOADS: usize = 4;

struct RangeFile {
    path: PathBuf,
    requested: u64,
    transferred: u64,
}

impl Drop for RangeFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn discard_stale_ranges(partial: &Path) -> Result<()> {
    let prefix = format!(
        "{}.range-",
        partial
            .file_name()
            .context("missing partial name")?
            .to_string_lossy()
    );
    for entry in fs::read_dir(partial.parent().context("missing cache directory")?)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            ensure!(entry.file_type()?.is_file(), "unexpected range file type");
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn download_range(
    partial: &Path,
    (start, end): (u64, u64),
    fetch: &(impl Fn(u64, u64) -> Result<Box<dyn Read>> + Sync),
    keepalive: &Mutex<impl FnMut() -> Result<()> + Send>,
    cancelled: &AtomicBool,
) -> Result<RangeFile> {
    let mut range = RangeFile {
        path: partial.with_extension(format!("partial.range-{start}")),
        requested: 0,
        transferred: 0,
    };
    for attempt in 0..CHUNK_RETRIES {
        ensure!(!cancelled.load(Ordering::Acquire), "range batch cancelled");
        (keepalive
            .lock()
            .map_err(|_| anyhow!("range heartbeat lock poisoned"))?)()?;
        ensure!(!cancelled.load(Ordering::Acquire), "range batch cancelled");
        let result = (|| -> Result<()> {
            let size = end - start + 1;
            range.requested += size;
            let reader = CountedRead {
                reader: fetch(start, end)?,
                transferred: &mut range.transferred,
            };
            let mut reader = reader.take(size + 1);
            let mut file = File::create(&range.path)?;
            let copied = std::io::copy(&mut reader, &mut file)?;
            file.flush()?;
            ensure!(
                copied == size,
                "invalid range length: {copied} bytes, expected {size}"
            );
            Ok(())
        })();
        match result {
            Ok(()) => return Ok(range),
            Err(error) if attempt + 1 < CHUNK_RETRIES => {
                eprintln!(
                    "[data-plane] retry range {start}-{end}, attempt {}: {error:#}",
                    attempt + 1
                );
                std::thread::sleep(Duration::from_secs(2_u64.pow(attempt + 1)));
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the final retry returns");
}

/// Stream at most four bounded ranges per object, join, then append in order.
/// A failed batch never appends out-of-order bytes or leaves a sparse prefix.
pub(super) fn hydrate(
    partial: &Path,
    total: u64,
    chunk_bytes: u64,
    fetch: impl Fn(u64, u64) -> Result<Box<dyn Read>> + Sync,
    keepalive: impl FnMut() -> Result<()> + Send,
) -> Result<(u64, u64)> {
    ensure!(chunk_bytes > 0, "empty range bound");
    discard_stale_ranges(partial)?;
    let keepalive = Mutex::new(keepalive);
    let mut requested = 0;
    let mut transferred = 0;
    loop {
        let have = fs::metadata(partial).map(|m| m.len()).unwrap_or(0);
        if have > total {
            fs::remove_file(partial)?;
            continue;
        }
        if have == total {
            return Ok((requested, transferred));
        }
        let ranges: Vec<_> = (0..MAX_RANGE_DOWNLOADS)
            .map(|index| have.saturating_add(index as u64 * chunk_bytes))
            .take_while(|&start| start < total)
            .map(|start| (start, start.saturating_add(chunk_bytes).min(total) - 1))
            .collect();
        let cancelled = AtomicBool::new(false);
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = ranges
                .iter()
                .map(|&bounds| {
                    let fetch = &fetch;
                    let keepalive = &keepalive;
                    let cancelled = &cancelled;
                    scope.spawn(move || {
                        let result = download_range(partial, bounds, fetch, keepalive, cancelled);
                        if result.is_err() {
                            cancelled.store(true, Ordering::Release);
                        }
                        result
                    })
                })
                .collect();
            // Collect all joins before propagating any failure.
            handles
                .into_iter()
                .map(|handle| handle.join().map_err(|_| anyhow!("range worker panicked")))
                .collect::<Vec<_>>()
        });
        let completed = results
            .into_iter()
            .map(|result| result?)
            .collect::<Result<Vec<_>>>()?;
        let mut prefix = OpenOptions::new().create(true).append(true).open(partial)?;
        for range in completed {
            std::io::copy(&mut File::open(&range.path)?, &mut prefix)?;
            prefix.flush()?;
            requested += range.requested;
            transferred += range.transferred;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Arc, Barrier};

    #[test]
    fn ranges_overlap_with_a_bound_and_commit_in_order() {
        struct Reader {
            data: Cursor<Vec<u8>>,
            first: bool,
            barrier: Arc<Barrier>,
            active: Arc<AtomicUsize>,
        }
        impl Read for Reader {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.first {
                    self.first = false;
                    self.barrier.wait();
                }
                self.data.read(out)
            }
        }
        impl Drop for Reader {
            fn drop(&mut self) {
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("object.partial");
        let bytes: Vec<u8> = (0..16).collect();
        let barrier = Arc::new(Barrier::new(MAX_RANGE_DOWNLOADS));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = AtomicUsize::new(0);
        let report = hydrate(
            &partial,
            16,
            4,
            |start, end| {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                // Request construction also overlaps, not just body reads.
                barrier.wait();
                Ok(Box::new(Reader {
                    data: Cursor::new(bytes[start as usize..=end as usize].to_vec()),
                    first: true,
                    barrier: barrier.clone(),
                    active: active.clone(),
                }))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), MAX_RANGE_DOWNLOADS);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(report, (16, 16));
        assert_eq!(fs::read(partial).unwrap(), bytes);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn interrupted_batch_preserves_only_the_existing_prefix_and_cleans_ranges() {
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("object.partial");
        fs::write(&partial, [0, 1]).unwrap();
        fs::write(root.path().join("object.partial.range-0"), b"stale").unwrap();
        let calls = AtomicUsize::new(0);
        assert!(
            hydrate(
                &partial,
                18,
                4,
                |start, end| {
                    Ok(Box::new(Cursor::new(
                        (start..=end).map(|v| v as u8).collect::<Vec<_>>(),
                    )))
                },
                || {
                    ensure!(calls.fetch_add(1, Ordering::SeqCst) < 2, "Run stopped");
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(fs::read(&partial).unwrap(), [0, 1]);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        let report = hydrate(
            &partial,
            18,
            4,
            |start, end| {
                Ok(Box::new(Cursor::new(
                    (start..=end).map(|v| v as u8).collect::<Vec<_>>(),
                )))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(report, (16, 16));
        assert_eq!(fs::read(partial).unwrap(), (0..18).collect::<Vec<u8>>());
    }

    #[test]
    fn stopped_run_starts_no_range_request() {
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("object.partial");
        let calls = AtomicUsize::new(0);
        assert!(
            hydrate(
                &partial,
                16,
                4,
                |_, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(Cursor::new(vec![0; 4])))
                },
                || anyhow::bail!("Run stopped")
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn one_short_range_retries_with_actual_batch_byte_counts() {
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("object.partial");
        let attempts = AtomicUsize::new(0);
        let report = hydrate(
            &partial,
            16,
            4,
            |start, end| {
                let bytes = if start == 4 && attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    vec![4, 5]
                } else {
                    (start..=end).map(|v| v as u8).collect()
                };
                Ok(Box::new(Cursor::new(bytes)))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(report, (20, 18));
        assert_eq!(fs::read(partial).unwrap(), (0..16).collect::<Vec<u8>>());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
