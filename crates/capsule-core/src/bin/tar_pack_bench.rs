use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Copy)]
struct Args {
    files: usize,
    size_bytes: usize,
}

#[derive(Debug, Serialize)]
struct BenchResult {
    files: usize,
    size_bytes: usize,
    generated_bytes: u64,
    generate_elapsed_ms: u128,
    pack_elapsed_ms: u128,
    total_elapsed_ms: u128,
    artifact_bytes: u64,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    run(args)
}

fn run(args: Args) -> Result<()> {
    if args.files == 0 {
        return Err("--files must be greater than zero".into());
    }

    let total_started = Instant::now();
    let tmp = tempfile::tempdir().map_err(|e| format!("create tempdir: {e}"))?;
    let project_root = tmp.path();
    let manifest_path = project_root.join("capsule.toml");
    let lockfile_path = project_root.join("capsule.lock");

    let generate_started = Instant::now();
    fs::write(
        &manifest_path,
        r#"schema_version = "0.3"
name = "tar-pack-bench"
version = "0.1.0"
type = "app"

runtime = "source/native"
run = "source/group-000/file-000000.txt""#,
    )
    .map_err(|e| format!("write capsule.toml: {e}"))?;
    fs::write(
        &lockfile_path,
        r#"version = "1"

[meta]
created_at = "2026-01-01T00:00:00Z"
manifest_hash = "sha256:dummy"
"#,
    )
    .map_err(|e| format!("write capsule.lock: {e}"))?;

    let generated_bytes = generate_source_tree(project_root, args.files, args.size_bytes)
        .map_err(|e| format!("generate source tree: {e}"))?;
    let generate_elapsed = generate_started.elapsed();

    let config_json = Arc::new(capsule_core::runtime_config::generate_config(
        &manifest_path,
        Some("strict".to_string()),
        false,
    )?);
    let config_path =
        capsule_core::runtime_config::write_config(&manifest_path, config_json.as_ref())
            .map_err(|e| format!("write config.json: {e}"))?;

    let decision = capsule_core::router::route_manifest(
        &manifest_path,
        capsule_core::router::ExecutionProfile::Release,
        None,
    )
    .map_err(|e| format!("route manifest: {e}"))?;

    let output = project_root.join("tar-pack-bench.capsule");
    let pack_started = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("build tokio runtime: {e}"))?;
    let artifact_path = runtime
        .block_on(capsule_core::packers::capsule::pack(
            &decision.plan,
            capsule_core::packers::capsule::CapsulePackOptions {
                compat_input: decision.plan.compat_project_input()?,
                workspace_root: project_root.to_path_buf(),
                output: Some(output),
                config_json,
                config_path,
                lockfile_path,
                signing_key: None,
            },
            Arc::new(capsule_core::reporter::NoOpReporter),
        ))
        .map_err(|e| format!("pack capsule: {e}"))?;
    let pack_elapsed = pack_started.elapsed();
    let artifact_bytes = fs::metadata(&artifact_path)
        .map_err(|e| format!("stat artifact {}: {e}", artifact_path.display()))?
        .len();

    let result = BenchResult {
        files: args.files,
        size_bytes: args.size_bytes,
        generated_bytes,
        generate_elapsed_ms: generate_elapsed.as_millis(),
        pack_elapsed_ms: pack_elapsed.as_millis(),
        total_elapsed_ms: total_started.elapsed().as_millis(),
        artifact_bytes,
    };

    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn generate_source_tree(root: &Path, files: usize, size_bytes: usize) -> Result<u64> {
    let source_root = root.join("source");
    fs::create_dir_all(&source_root)
        .map_err(|e| format!("create {}: {e}", source_root.display()))?;

    for shard in 0..256usize {
        let dir = source_root.join(format!("group-{shard:03}"));
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    }

    let payload = vec![b'x'; size_bytes];
    for idx in 0..files {
        let dir = source_root.join(format!("group-{:03}", idx % 256));
        let file_path = dir.join(format!("file-{idx:06}.txt"));
        fs::write(&file_path, &payload)
            .map_err(|e| format!("write {}: {e}", file_path.display()))?;
    }

    Ok((files as u64) * (size_bytes as u64))
}

fn parse_args() -> Result<Args> {
    let mut files: usize = 10_000;
    let mut size_bytes: usize = 1024;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--files" => {
                let value = args.next().ok_or("--files requires a value")?;
                files = value
                    .parse()
                    .map_err(|e| format!("invalid --files value: {e}"))?;
            }
            "--size-bytes" => {
                let value = args.next().ok_or("--size-bytes requires a value")?;
                size_bytes = value
                    .parse()
                    .map_err(|e| format!("invalid --size-bytes value: {e}"))?;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }

    Ok(Args { files, size_bytes })
}

fn print_help() {
    println!(
        "tar_pack_bench\n\nUsage:\n  cargo run -p capsule-core --bin tar_pack_bench -- [--files N] [--size-bytes N]\n\nOptions:\n  --files N        Number of files to generate (default: 10000)\n  --size-bytes N   Bytes per file (default: 1024)"
    );
}
