use std::fs;
use std::sync::{Arc, Mutex};

use capsule_core::lockfile::ensure_lockfile;
use capsule_core::reporter::{CapsuleReporter, NoOpReporter};
use tempfile::TempDir;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;
static DHAT_TEST_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy)]
struct AllocationDelta {
    total_blocks: u64,
    total_bytes: u64,
}

fn measure_allocations<F, T>(f: F) -> (T, AllocationDelta)
where
    F: FnOnce() -> T,
{
    let before = dhat::HeapStats::get();
    let result = f();
    let after = dhat::HeapStats::get();
    (
        result,
        AllocationDelta {
            total_blocks: after.total_blocks - before.total_blocks,
            total_bytes: after.total_bytes - before.total_bytes,
        },
    )
}

fn assert_allocation_gate(
    label: &str,
    delta: AllocationDelta,
    max_total_blocks: u64,
    max_total_bytes: u64,
) {
    assert!(
        delta.total_blocks <= max_total_blocks,
        "allocation regression ({label} blocks): {} > {}",
        delta.total_blocks,
        max_total_blocks
    );
    assert!(
        delta.total_bytes <= max_total_bytes,
        "allocation regression ({label} bytes): {} > {}",
        delta.total_bytes,
        max_total_bytes
    );
}

fn run_lockfile_measurement() -> (
    std::path::PathBuf,
    std::path::PathBuf,
    AllocationDelta,
    AllocationDelta,
) {
    let _guard = DHAT_TEST_MUTEX.lock().expect("dhat mutex");
    let _profiler = dhat::Profiler::new_heap();

    let temp = TempDir::new().expect("tempdir");
    let manifest_path = temp.path().join("capsule.toml");
    let manifest_text = r#"
schema_version = "0.3"
name = "alloc-gate-demo"
version = "0.1.0"
type = "app"

runtime = "source/native"
run = "source/main.sh""#;
    fs::write(&manifest_path, manifest_text).expect("write manifest");
    fs::create_dir_all(temp.path().join("source")).expect("create source dir");
    fs::write(temp.path().join("source/main.sh"), "echo alloc-gate").expect("write entrypoint");

    let manifest_raw: toml::Value = toml::from_str(manifest_text).expect("parse manifest");
    let reporter: Arc<dyn CapsuleReporter + 'static> = Arc::new(NoOpReporter);
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let (first_path, first_delta) = measure_allocations(|| {
        rt.block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter.clone(),
            false,
        ))
        .expect("first ensure_lockfile")
    });

    let (second_path, second_delta) = measure_allocations(|| {
        rt.block_on(ensure_lockfile(
            &manifest_path,
            &manifest_raw,
            manifest_text,
            reporter,
            false,
        ))
        .expect("second ensure_lockfile")
    });

    drop(_profiler);
    let _ = fs::remove_file("dhat-heap.json");
    (first_path, second_path, first_delta, second_delta)
}

#[test]
#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    ignore = "DHAT allocation totals vary on hosted CI runners; tracked in #82"
)]
fn lockfile_allocation_regression_gate() {
    const MAX_FIRST_TOTAL_BLOCKS: u64 = 4_000;
    const MAX_FIRST_TOTAL_BYTES: u64 = 900_000;
    const MAX_REUSE_TOTAL_BLOCKS: u64 = 1_500;
    const MAX_REUSE_TOTAL_BYTES: u64 = 220_000;

    let (first_path, second_path, first_delta, second_delta) = run_lockfile_measurement();
    assert_eq!(first_path, second_path, "lockfile path must be stable");
    assert_allocation_gate(
        "first run",
        first_delta,
        MAX_FIRST_TOTAL_BLOCKS,
        MAX_FIRST_TOTAL_BYTES,
    );
    assert_allocation_gate(
        "reuse run",
        second_delta,
        MAX_REUSE_TOTAL_BLOCKS,
        MAX_REUSE_TOTAL_BYTES,
    );
}

#[test]
#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    ignore = "DHAT allocation totals vary on hosted CI runners; tracked in #82"
)]
#[should_panic(expected = "allocation regression")]
fn lockfile_allocation_gate_panics_when_threshold_is_too_low() {
    let (_, _, first_delta, _) = run_lockfile_measurement();
    assert_allocation_gate("forced panic", first_delta, 1, 1);
}
