//! Hotset profile — the ordered set of chunks a restore touches between
//! LoadSnapshot and the first healthy request (plan §7).
//!
//! Recorded during build (by observing which memory pages / rootfs blocks the
//! boot-to-readiness path touches), it lets a restore prefetch the hot chunks
//! sequentially from local SSD before/while resuming, minimizing first-request
//! latency. Stage 1 uses it to warm the page cache via [`LazyBlobReader`];
//! Stage 2's UFFD page server will use the same ordering.

use serde::{Deserialize, Serialize};

use crate::hash::ContentHash;

/// An ordered list of chunk hashes in first-touch order. Deduplicated: a chunk
/// touched twice appears once, at its first occurrence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotsetProfile {
    /// Chunks in the order they were first accessed.
    pub ordered_chunks: Vec<ContentHash>,
}

impl HotsetProfile {
    /// Number of chunks in the profile.
    pub fn len(&self) -> usize {
        self.ordered_chunks.len()
    }

    /// Whether the profile is empty.
    pub fn is_empty(&self) -> bool {
        self.ordered_chunks.is_empty()
    }
}

/// Builds a [`HotsetProfile`] by recording chunk accesses in order, ignoring
/// repeats so the profile is the *first-touch* sequence.
#[derive(Debug, Default)]
pub struct HotsetRecorder {
    ordered: Vec<ContentHash>,
    seen: std::collections::HashSet<ContentHash>,
}

impl HotsetRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a chunk access. The first time a chunk is seen it is appended;
    /// later repeats are ignored.
    pub fn record(&mut self, hash: &ContentHash) {
        if self.seen.insert(hash.clone()) {
            self.ordered.push(hash.clone());
        }
    }

    /// Finish recording and produce the profile.
    pub fn finish(self) -> HotsetProfile {
        HotsetProfile {
            ordered_chunks: self.ordered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;

    #[test]
    fn recorder_keeps_first_touch_order_and_dedups() {
        let a = hash_bytes(b"a");
        let b = hash_bytes(b"b");
        let c = hash_bytes(b"c");
        let mut r = HotsetRecorder::new();
        r.record(&b);
        r.record(&a);
        r.record(&b); // repeat ignored
        r.record(&c);
        r.record(&a); // repeat ignored
        let profile = r.finish();
        assert_eq!(profile.ordered_chunks, vec![b, a, c]);
        assert_eq!(profile.len(), 3);
    }

    #[test]
    fn empty_profile() {
        let p = HotsetRecorder::new().finish();
        assert!(p.is_empty());
    }
}
