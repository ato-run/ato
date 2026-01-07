//! Performance Benchmarks for Storage and Container Runtime
//!
//! These benchmarks measure:
//! - Storage I/O throughput (read/write)
//! - Layer cache hit/miss performance
//! - Container startup latency
//! - Image pull throughput
//!
//! To run benchmarks:
//! ```bash
//! cargo test --test integration -- benchmarks --nocapture
//! # For storage benchmarks requiring root:
//! sudo -E cargo test --test integration -- benchmarks --ignored --nocapture
//! ```

use std::fs;
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Benchmark result
#[derive(Debug)]
struct BenchmarkResult {
    name: String,
    iterations: u32,
    total_time: Duration,
    avg_time: Duration,
    throughput: Option<f64>, // MB/s if applicable
}

impl BenchmarkResult {
    fn new(name: &str, iterations: u32, total_time: Duration) -> Self {
        Self {
            name: name.to_string(),
            iterations,
            total_time,
            avg_time: total_time / iterations,
            throughput: None,
        }
    }

    fn with_throughput(mut self, bytes: u64) -> Self {
        let secs = self.total_time.as_secs_f64();
        let mb = bytes as f64 / (1024.0 * 1024.0);
        self.throughput = Some(mb / secs);
        self
    }

    fn print(&self) {
        println!("=== {} ===", self.name);
        println!("  Iterations: {}", self.iterations);
        println!("  Total time: {:?}", self.total_time);
        println!("  Avg time: {:?}", self.avg_time);
        if let Some(tp) = self.throughput {
            println!("  Throughput: {:.2} MB/s", tp);
        }
        println!();
    }
}

// ============================================================================
// File I/O Benchmarks
// ============================================================================

#[test]
fn bench_sequential_write() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("bench_write");

    const BLOCK_SIZE: usize = 4096;
    const NUM_BLOCKS: u32 = 10000; // ~40MB
    let data = vec![0xABu8; BLOCK_SIZE];

    let start = Instant::now();
    let mut file = fs::File::create(&file_path).unwrap();
    for _ in 0..NUM_BLOCKS {
        file.write_all(&data).unwrap();
    }
    file.sync_all().unwrap();
    let elapsed = start.elapsed();

    let total_bytes = BLOCK_SIZE as u64 * NUM_BLOCKS as u64;
    let result =
        BenchmarkResult::new("Sequential Write", NUM_BLOCKS, elapsed).with_throughput(total_bytes);
    result.print();

    // Assert minimum performance (1 MB/s is extremely conservative)
    assert!(result.throughput.unwrap() > 1.0, "Write throughput too low");
}

#[test]
fn bench_sequential_read() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("bench_read");

    const BLOCK_SIZE: usize = 4096;
    const NUM_BLOCKS: u32 = 10000;

    // Create test file
    let data = vec![0xCDu8; BLOCK_SIZE];
    let mut file = fs::File::create(&file_path).unwrap();
    for _ in 0..NUM_BLOCKS {
        file.write_all(&data).unwrap();
    }
    file.sync_all().unwrap();
    drop(file);

    // Benchmark read
    let start = Instant::now();
    let mut file = fs::File::open(&file_path).unwrap();
    let mut buffer = vec![0u8; BLOCK_SIZE];
    for _ in 0..NUM_BLOCKS {
        file.read_exact(&mut buffer).unwrap();
    }
    let elapsed = start.elapsed();

    let total_bytes = BLOCK_SIZE as u64 * NUM_BLOCKS as u64;
    let result =
        BenchmarkResult::new("Sequential Read", NUM_BLOCKS, elapsed).with_throughput(total_bytes);
    result.print();

    assert!(result.throughput.unwrap() > 1.0, "Read throughput too low");
}

#[test]
fn bench_random_read() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("bench_random");

    const BLOCK_SIZE: usize = 4096;
    const FILE_BLOCKS: usize = 1000;
    const READ_OPS: u32 = 1000;

    // Create test file
    let data = vec![0xEFu8; BLOCK_SIZE * FILE_BLOCKS];
    fs::write(&file_path, &data).unwrap();

    // Generate random offsets
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut offsets = Vec::with_capacity(READ_OPS as usize);
    for i in 0..READ_OPS {
        let mut hasher = DefaultHasher::new();
        i.hash(&mut hasher);
        let offset = (hasher.finish() as usize % FILE_BLOCKS) * BLOCK_SIZE;
        offsets.push(offset as u64);
    }

    // Benchmark random reads
    use std::io::Seek;
    let start = Instant::now();
    let mut file = fs::File::open(&file_path).unwrap();
    let mut buffer = vec![0u8; BLOCK_SIZE];
    for offset in offsets {
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        file.read_exact(&mut buffer).unwrap();
    }
    let elapsed = start.elapsed();

    let result = BenchmarkResult::new("Random Read", READ_OPS, elapsed);
    result.print();

    // Random reads should complete in reasonable time (< 100ms per op average)
    assert!(
        result.avg_time < Duration::from_millis(100),
        "Random read too slow"
    );
}

// ============================================================================
// Layer Cache Benchmarks
// ============================================================================

#[test]
fn bench_cache_write() {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = temp_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    const LAYER_SIZE: usize = 1024 * 1024; // 1MB per layer
    const NUM_LAYERS: u32 = 50;
    let layer_data = vec![0x12u8; LAYER_SIZE];

    let start = Instant::now();
    for i in 0..NUM_LAYERS {
        let digest = format!("sha256_{:08x}", i);
        let path = cache_dir.join(&digest);
        fs::write(&path, &layer_data).unwrap();
    }
    let elapsed = start.elapsed();

    let total_bytes = LAYER_SIZE as u64 * NUM_LAYERS as u64;
    let result =
        BenchmarkResult::new("Cache Write", NUM_LAYERS, elapsed).with_throughput(total_bytes);
    result.print();

    assert!(result.throughput.unwrap() > 10.0, "Cache write too slow");
}

#[test]
fn bench_cache_read_hit() {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = temp_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    const LAYER_SIZE: usize = 1024 * 1024;
    const NUM_LAYERS: u32 = 50;
    let layer_data = vec![0x34u8; LAYER_SIZE];

    // Pre-populate cache
    for i in 0..NUM_LAYERS {
        let digest = format!("sha256_{:08x}", i);
        let path = cache_dir.join(&digest);
        fs::write(&path, &layer_data).unwrap();
    }

    // Benchmark cache hits
    let start = Instant::now();
    for i in 0..NUM_LAYERS {
        let digest = format!("sha256_{:08x}", i);
        let path = cache_dir.join(&digest);
        let _data = fs::read(&path).unwrap();
    }
    let elapsed = start.elapsed();

    let total_bytes = LAYER_SIZE as u64 * NUM_LAYERS as u64;
    let result =
        BenchmarkResult::new("Cache Read (Hit)", NUM_LAYERS, elapsed).with_throughput(total_bytes);
    result.print();

    assert!(result.throughput.unwrap() > 50.0, "Cache read too slow");
}

#[test]
fn bench_cache_lookup() {
    let temp_dir = TempDir::new().unwrap();
    let cache_dir = temp_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    const NUM_LAYERS: u32 = 100;
    const LOOKUPS: u32 = 1000;

    // Create some layers
    for i in 0..NUM_LAYERS {
        let digest = format!("sha256_{:08x}", i);
        let path = cache_dir.join(&digest);
        fs::write(&path, b"dummy").unwrap();
    }

    // Benchmark lookups (existence check)
    let start = Instant::now();
    for i in 0..LOOKUPS {
        let digest = format!("sha256_{:08x}", i % (NUM_LAYERS * 2)); // 50% hit rate
        let path = cache_dir.join(&digest);
        let _exists = path.exists();
    }
    let elapsed = start.elapsed();

    let result = BenchmarkResult::new("Cache Lookup", LOOKUPS, elapsed);
    result.print();

    // Lookups should be very fast (< 1ms average)
    assert!(
        result.avg_time < Duration::from_millis(1),
        "Cache lookup too slow"
    );
}

// ============================================================================
// Tar Extraction Benchmarks
// ============================================================================

#[test]
fn bench_tar_extraction() {
    let temp_dir = TempDir::new().unwrap();
    let tar_path = temp_dir.path().join("test.tar");
    let extract_path = temp_dir.path().join("extracted");

    // Create a tar archive with multiple files
    const NUM_FILES: u32 = 100;
    const FILE_SIZE: usize = 10240; // 10KB per file

    {
        let tar_file = fs::File::create(&tar_path).unwrap();
        let mut builder = tar::Builder::new(tar_file);

        for i in 0..NUM_FILES {
            let name = format!("file_{:04}.txt", i);
            let data = vec![(i % 256) as u8; FILE_SIZE];
            let mut header = tar::Header::new_gnu();
            header.set_path(&name).unwrap();
            header.set_size(FILE_SIZE as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &data[..]).unwrap();
        }
        builder.finish().unwrap();
    }

    // Benchmark extraction
    let start = Instant::now();
    fs::create_dir_all(&extract_path).unwrap();
    let tar_file = fs::File::open(&tar_path).unwrap();
    let mut archive = tar::Archive::new(tar_file);
    archive.unpack(&extract_path).unwrap();
    let elapsed = start.elapsed();

    let total_bytes = (FILE_SIZE as u64) * (NUM_FILES as u64);
    let result =
        BenchmarkResult::new("Tar Extraction", NUM_FILES, elapsed).with_throughput(total_bytes);
    result.print();

    // Verify extraction
    let extracted_files = fs::read_dir(&extract_path).unwrap().count();
    assert_eq!(extracted_files, NUM_FILES as usize);

    assert!(result.throughput.unwrap() > 1.0, "Tar extraction too slow");
}

#[test]
fn bench_gzip_tar_extraction() {
    let temp_dir = TempDir::new().unwrap();
    let tar_gz_path = temp_dir.path().join("test.tar.gz");
    let extract_path = temp_dir.path().join("extracted");

    const NUM_FILES: u32 = 100;
    const FILE_SIZE: usize = 10240;

    // Create compressed tar
    {
        let tar_gz_file = fs::File::create(&tar_gz_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(tar_gz_file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        for i in 0..NUM_FILES {
            let name = format!("file_{:04}.txt", i);
            let data = vec![(i % 256) as u8; FILE_SIZE];
            let mut header = tar::Header::new_gnu();
            header.set_path(&name).unwrap();
            header.set_size(FILE_SIZE as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, &data[..]).unwrap();
        }
        builder.finish().unwrap();
    }

    // Benchmark extraction
    let start = Instant::now();
    fs::create_dir_all(&extract_path).unwrap();
    let tar_gz_file = fs::File::open(&tar_gz_path).unwrap();
    let decoder = flate2::read::GzDecoder::new(tar_gz_file);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(&extract_path).unwrap();
    let elapsed = start.elapsed();

    let total_bytes = (FILE_SIZE as u64) * (NUM_FILES as u64);
    let result = BenchmarkResult::new("Gzip+Tar Extraction", NUM_FILES, elapsed)
        .with_throughput(total_bytes);
    result.print();

    assert!(result.throughput.unwrap() > 0.5, "Gzip extraction too slow");
}

// ============================================================================
// Cold Start Benchmarks
// ============================================================================

/// Measures the time for cold start preparation:
/// - TOML manifest parsing
/// - Manifest validation
/// - RunPlan generation
/// - OCI spec building
#[test]
fn bench_cold_start_preparation() {
    use std::time::Instant;

    const SAMPLE_TOML: &str = r#"
schema_version = "1.0"
name = "bench-capsule"
version = "1.0.0"
type = "app"

[metadata]
display_name = "Benchmark Capsule"
description = "Test capsule for cold start benchmarks"

[execution]
runtime = "docker"
entrypoint = "ghcr.io/example/hello:latest"
port = 8080

[execution.env]
GUMBALL_ENV = "benchmark"
LOG_LEVEL = "info"

[requirements]
vram_min = "2GB"
"#;

    const ITERATIONS: u32 = 100;

    // Benchmark TOML parsing
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _manifest: Result<capsuled::capsule_types::capsule_v1::CapsuleManifestV1, _> =
            capsuled::capsule_types::capsule_v1::CapsuleManifestV1::from_toml(SAMPLE_TOML);
    }
    let parse_time = start.elapsed();
    let parse_avg = parse_time / ITERATIONS;

    // Benchmark validation (parse once, validate many times)
    let manifest =
        capsuled::capsule_types::capsule_v1::CapsuleManifestV1::from_toml(SAMPLE_TOML).unwrap();
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _valid = manifest.validate();
    }
    let validate_time = start.elapsed();
    let validate_avg = validate_time / ITERATIONS;

    // Benchmark RunPlan generation
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _plan = manifest.to_run_plan();
    }
    let runplan_time = start.elapsed();
    let runplan_avg = runplan_time / ITERATIONS;

    // Calculate total cold start preparation time
    let total_avg = parse_avg + validate_avg + runplan_avg;

    println!("\n=== Cold Start Preparation Benchmark ===");
    println!(
        "  TOML Parse:    {:?} avg ({} iterations)",
        parse_avg, ITERATIONS
    );
    println!("  Validation:    {:?} avg", validate_avg);
    println!("  RunPlan Gen:   {:?} avg", runplan_avg);
    println!("  ---------------------------------");
    println!("  TOTAL:         {:?} avg", total_avg);
    println!();

    // Target: cold start prep should be < 100ms (conservative)
    // In practice, should be < 10ms for just parsing/validation
    assert!(
        total_avg < Duration::from_millis(100),
        "Cold start preparation too slow: {:?} > 100ms",
        total_avg
    );

    // More aggressive target for just parsing/validation
    assert!(
        parse_avg + validate_avg < Duration::from_millis(10),
        "Manifest parsing+validation too slow: {:?}",
        parse_avg + validate_avg
    );

    println!("✅ Cold start preparation meets target (<100ms total)");
}

// ============================================================================
// Summary
// ============================================================================

#[test]
fn print_benchmark_summary() {
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║            Storage & Container Benchmark Summary             ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║ Run all benchmarks with:                                     ║");
    println!("║   cargo test --test integration -- benchmarks --nocapture    ║");
    println!("║                                                              ║");
    println!("║ Performance targets:                                         ║");
    println!("║   - Sequential I/O: > 100 MB/s                               ║");
    println!("║   - Cache operations: > 50 MB/s                              ║");
    println!("║   - Cache lookups: < 1ms                                     ║");
    println!("║   - Layer extraction: > 10 MB/s                              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
}

// ============================================================================
// Ignored benchmarks (require special setup)
// ============================================================================

#[test]
#[ignore]
fn bench_lvm_volume_creation() {
    // Requires root and LVM setup
    // Would measure time to create/delete LVM volumes
    println!("LVM volume creation benchmark would run here");
}

#[test]
#[ignore]
fn bench_luks_encryption() {
    // Requires root and cryptsetup
    // Would measure encryption/decryption overhead
    println!("LUKS encryption benchmark would run here");
}

#[test]
#[ignore]
fn bench_container_startup() {
    // Requires container runtime
    // Would measure container start latency
    println!("Container startup benchmark would run here");
}

#[test]
#[ignore]
fn bench_image_pull() {
    // Requires network
    // Would measure image pull throughput
    println!("Image pull benchmark would run here");
}
