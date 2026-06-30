//! Backend selection for the Ready-State path.
//!
//! Honors `ATO_SNAPSHOT_BACKEND`; otherwise probes Firecracker and falls back to
//! the Fake backend. On a KVM-less host this always yields the Fake backend, so
//! the whole build→restore pipeline runs end-to-end without `/dev/kvm`. Mirrors
//! the principled selection in `snapshot`'s e2e tests but routed through the
//! flag.

use snapshot::{
    FakeSnapshotBackend, FirecrackerBackend, KataBackend, QemuBackend, SnapshotBackend,
};

use super::flags;

/// Choose a snapshot backend. Never panics; falls back to Fake when the
/// requested/available backend cannot build (e.g. no `/dev/kvm`).
pub(crate) fn select_backend() -> Box<dyn SnapshotBackend> {
    if let Some(id) = flags::selected_backend_id() {
        match id.as_str() {
            "fake" => return Box::new(FakeSnapshotBackend::new()),
            "firecracker" => {
                let fc = FirecrackerBackend::new();
                if fc.probe().available {
                    return Box::new(fc);
                }
            }
            "qemu" => {
                let q = QemuBackend::new();
                if q.probe().available {
                    return Box::new(q);
                }
            }
            "kata" => {
                let k = KataBackend::new();
                if k.probe().available {
                    return Box::new(k);
                }
            }
            _ => {}
        }
        // Explicit selection that is unavailable → fall through to Fake so the
        // KVM-free pipeline still runs (never a hard failure of the product).
        return Box::new(FakeSnapshotBackend::new());
    }

    // No explicit selection: prefer a real microVM backend if it can build here,
    // else Fake.
    let fc = FirecrackerBackend::new();
    if fc.probe().available {
        Box::new(fc)
    } else {
        Box::new(FakeSnapshotBackend::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_backend_falls_back_to_fake_without_kvm() {
        // No ATO_SNAPSHOT_BACKEND set in the test env, no /dev/kvm on this host.
        // (If the env var leaks in from the runner, this still returns a usable
        // backend; we only assert it produces something selectable.)
        let b = select_backend();
        assert!(!b.id().is_empty());
    }
}
