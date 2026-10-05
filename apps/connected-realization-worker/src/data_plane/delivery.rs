//! Bounded hydration shared by every Model Set. Reports retain declaration
//! order; scheduling larger unique objects first does not change their identity.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, anyhow, ensure};

use super::{ModelCache, ModelSetEntry, ObjectDelivery};

const MAX_OBJECT_DOWNLOADS: usize = 4;

pub(super) fn ensure_objects(
    cache: &ModelCache,
    entries: &[&ModelSetEntry],
    fetch: impl Fn(&ModelSetEntry, u64, u64) -> Result<Box<dyn Read>> + Sync,
    mut keepalive: impl FnMut() -> Result<()> + Send,
) -> Result<Vec<ObjectDelivery>> {
    // One writer per digest even when two paths or inputs reference it.
    let mut unique = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        match unique.get(entry.digest.as_str()) {
            Some((_, bytes)) => ensure!(*bytes == entry.bytes, "conflicting object sizes"),
            None => {
                unique.insert(entry.digest.as_str(), (index, entry.bytes));
            }
        }
    }
    let mut jobs: Vec<_> = unique.values().map(|&(index, _)| index).collect();
    jobs.sort_by_key(|&index| (std::cmp::Reverse(entries[index].bytes), index));
    let next = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let first_error = Mutex::new(None);
    // Execution authorization is mutable and must be refreshed serially.
    let heartbeat = Mutex::new(&mut keepalive);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..MAX_OBJECT_DOWNLOADS.min(jobs.len()))
            .map(|_| {
                scope.spawn(|| -> Result<Vec<(usize, ObjectDelivery)>> {
                    let mut completed = Vec::new();
                    while !cancelled.load(Ordering::Acquire) {
                        let Some(&index) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                            break;
                        };
                        let entry = entries[index];
                        let delivered = cache.ensure(
                            entry,
                            |start, end| fetch(entry, start, end),
                            || {
                                ensure!(!cancelled.load(Ordering::Acquire), "delivery cancelled");
                                let mut refresh = heartbeat
                                    .lock()
                                    .map_err(|_| anyhow!("delivery heartbeat lock poisoned"))?;
                                ensure!(!cancelled.load(Ordering::Acquire), "delivery cancelled");
                                (**refresh)()
                            },
                        );
                        match delivered {
                            Ok(report) => {
                                eprintln!(
                                    "[data-plane] {} {} {} bytes in {} ms",
                                    report.cache,
                                    report.digest,
                                    report.bytes_transferred,
                                    report.millis
                                );
                                completed.push((index, report));
                            }
                            Err(error) => {
                                first_error
                                    .lock()
                                    .map_err(|_| anyhow!("delivery failure lock poisoned"))?
                                    .get_or_insert(error);
                                cancelled.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                    Ok(completed)
                })
            })
            .collect();
        // Join every worker before returning an error or publishing inputs.
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow!("delivery worker panicked"))
            })
            .collect::<Vec<_>>()
    });
    let mut reports = BTreeMap::new();
    for result in results {
        reports.extend(result??);
    }
    if let Some(error) = first_error
        .into_inner()
        .map_err(|_| anyhow!("delivery failure lock poisoned"))?
    {
        return Err(error);
    }
    drop(heartbeat);
    let mut ordered = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let report = if let Some(report) = reports.remove(&index) {
            report
        } else {
            // A duplicate path reuses the verified object and records an actual
            // marker check, with zero requested/transferred bytes.
            cache.ensure(entry, |start, end| fetch(entry, start, end), &mut keepalive)?
        };
        ordered.push(report);
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    #[test]
    fn concurrent_unique_objects_are_bounded_and_reported_in_declaration_order() {
        struct Reader {
            bytes: Cursor<Vec<u8>>,
            active: Arc<AtomicUsize>,
        }
        impl Read for Reader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.bytes.read(buf)
            }
        }
        impl Drop for Reader {
            fn drop(&mut self) {
                self.active.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let payloads: Vec<_> = (1..=12).map(|size| vec![size as u8; size * 100]).collect();
        let entries: Vec<_> = payloads
            .iter()
            .enumerate()
            .map(|(i, data)| ModelSetEntry {
                path: format!("{i}.bin"),
                digest: super::super::sha256_ref(data),
                bytes: data.len() as u64,
            })
            .collect();
        let refs: Vec<_> = entries.iter().collect();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(MAX_OBJECT_DOWNLOADS);
        let reports = ensure_objects(
            &cache,
            &refs,
            |entry, start, end| {
                assert_eq!((start, end), (0, entry.bytes - 1));
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                // Each group must overlap; no wall-clock performance assertion.
                barrier.wait();
                let index = entries
                    .iter()
                    .position(|e| e.digest == entry.digest)
                    .unwrap();
                Ok(Box::new(Reader {
                    bytes: Cursor::new(payloads[index].clone()),
                    active: Arc::clone(&active),
                }))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), MAX_OBJECT_DOWNLOADS);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        for (entry, report) in entries.iter().zip(&reports) {
            assert_eq!(report.digest, entry.digest);
            assert_eq!(report.cache, "miss");
            assert_eq!(report.bytes_requested, entry.bytes);
            assert_eq!(report.bytes_transferred, entry.bytes);
            assert_eq!(
                std::fs::metadata(cache.object_path(&entry.digest).unwrap())
                    .unwrap()
                    .len(),
                entry.bytes
            );
        }
        let warm = ensure_objects(
            &cache,
            &refs,
            |_, _, _| panic!("a warm object must not fetch"),
            || Ok(()),
        )
        .unwrap();
        assert!(
            warm.iter()
                .all(|r| r.cache == "hit" && r.bytes_requested == 0 && r.bytes_transferred == 0)
        );
    }

    #[test]
    fn duplicate_digest_has_one_writer_and_only_one_transfer() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let data = b"shared model";
        let first = ModelSetEntry {
            path: "a.bin".into(),
            digest: super::super::sha256_ref(data),
            bytes: data.len() as u64,
        };
        let second = ModelSetEntry {
            path: "b.bin".into(),
            ..first.clone()
        };
        let calls = AtomicUsize::new(0);
        let reports = ensure_objects(
            &cache,
            &[&first, &second],
            |_, _, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Cursor::new(data.to_vec())))
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!((reports[0].cache, reports[1].cache), ("miss", "hit"));
        assert_eq!(
            reports.iter().map(|r| r.bytes_transferred).sum::<u64>(),
            data.len() as u64
        );
        let conflicting = ModelSetEntry {
            bytes: first.bytes + 1,
            ..second
        };
        assert!(
            ensure_objects(
                &cache,
                &[&first, &conflicting],
                |_, _, _| panic!("reject before I/O"),
                || Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn revoked_authorization_stops_all_workers_before_fetching() {
        let root = tempfile::tempdir().unwrap();
        let cache = ModelCache::open(root.path()).unwrap();
        let entries: Vec<_> = (0..20)
            .map(|n| ModelSetEntry {
                path: format!("{n}.bin"),
                digest: super::super::sha256_ref(&[n]),
                bytes: 1,
            })
            .collect();
        let refs: Vec<_> = entries.iter().collect();
        let mut refreshes = 0;
        let error = ensure_objects(
            &cache,
            &refs,
            |_, _, _| panic!("revoked grants must never fetch"),
            || {
                refreshes += 1;
                anyhow::bail!("authorization revoked")
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("authorization revoked"));
        assert!((1..=MAX_OBJECT_DOWNLOADS).contains(&refreshes));
        assert!(
            entries
                .iter()
                .all(|e| !cache.object_path(&e.digest).unwrap().exists())
        );
    }
}
