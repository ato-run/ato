//! Backend selection for the Ready-State path (driven by `ATO_SNAPSHOT_BACKEND`).
//!
//! Fail-closed semantics (the product never silently substitutes a backend the
//! user explicitly asked for):
//!
//! - **unset** → the KVM-free [`FakeSnapshotBackend`] (safe default; the legacy cold path is selected elsewhere — this engine only runs when Ready-State is enabled).
//! - **`fake`** → [`FakeSnapshotBackend`].
//! - **`firecracker`** → the real [`FirecrackerBackend`] **iff** it probes available (`/dev/kvm` + the binary); otherwise a **clear error**, never a silent fallback. This backend is **single-session (max-concurrency 1)** — enforced by its per-tap lockfile.
//! - **`qemu`/`kata`** → those backends iff available (skeletons today ⇒ always a clear "unavailable" error).
//! - anything else → a clear "unknown backend" error.

use anyhow::bail;
use snapshot::{
    FakeSnapshotBackend, FirecrackerBackend, KataBackend, QemuBackend, SnapshotBackend,
};

use super::flags;

fn require_available(
    backend: Box<dyn SnapshotBackend>,
    id: &str,
) -> anyhow::Result<Box<dyn SnapshotBackend>> {
    let probe = backend.probe();
    if probe.available {
        Ok(backend)
    } else {
        bail!(
            "ATO_SNAPSHOT_BACKEND={id} was requested but the backend is unavailable: {}",
            probe.reason.unwrap_or_else(|| "unknown reason".to_string())
        )
    }
}

/// Select the Ready-State snapshot backend per `ATO_SNAPSHOT_BACKEND`,
/// fail-closed (see module docs). Returns an error rather than silently
/// substituting a different backend than the one explicitly requested.
pub(crate) fn select_backend() -> anyhow::Result<Box<dyn SnapshotBackend>> {
    let Some(id) = flags::selected_backend_id() else {
        // Unset: safe KVM-free default.
        return Ok(Box::new(FakeSnapshotBackend::new()));
    };
    match id.as_str() {
        "fake" => Ok(Box::new(FakeSnapshotBackend::new())),
        "firecracker" => require_available(Box::new(FirecrackerBackend::new()), "firecracker"),
        "qemu" => require_available(Box::new(QemuBackend::new()), "qemu"),
        "kata" => require_available(Box::new(KataBackend::new()), "kata"),
        other => bail!(
            "unknown ATO_SNAPSHOT_BACKEND='{other}' (expected: fake | firecracker | qemu | kata)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests mutate a process-global env var, so they must not run
    // concurrently with each other; keep them in one test fn.
    #[test]
    fn select_backend_is_fail_closed() {
        // SAFETY: single-threaded test body; we restore the var at the end.
        let prev = std::env::var("ATO_SNAPSHOT_BACKEND").ok();
        let set = |v: Option<&str>| unsafe {
            match v {
                Some(v) => std::env::set_var("ATO_SNAPSHOT_BACKEND", v),
                None => std::env::remove_var("ATO_SNAPSHOT_BACKEND"),
            }
        };

        // unset → Fake (safe default).
        set(None);
        assert_eq!(select_backend().unwrap().id(), "fake");

        // explicit fake → Fake.
        set(Some("fake"));
        assert_eq!(select_backend().unwrap().id(), "fake");

        // explicit firecracker without /dev/kvm → clear error, NOT a fallback.
        if !snapshot::FirecrackerBackend::kvm_present() {
            set(Some("firecracker"));
            // `.err().unwrap()` avoids requiring `Debug` on `Box<dyn SnapshotBackend>`.
            let err = select_backend().err().expect("must fail closed without kvm");
            assert!(err.to_string().contains("firecracker") && err.to_string().contains("unavailable"));
        }

        // qemu/kata skeletons are unavailable → error (never silently used).
        set(Some("qemu"));
        assert!(select_backend().is_err());

        // unknown → error.
        set(Some("nonsense-backend"));
        let err = select_backend().err().expect("unknown backend must error");
        assert!(err.to_string().contains("unknown"));

        set(prev.as_deref());
    }
}
