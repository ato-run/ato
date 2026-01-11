use std::path::{Path, PathBuf};

fn find_in_path(executable: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(executable);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn resolve_protoc() -> Option<PathBuf> {
    if let Some(existing) = std::env::var_os("PROTOC").map(PathBuf::from) {
        if existing.is_file() {
            return Some(existing);
        }
    }

    if let Some(found) = find_in_path("protoc") {
        return Some(found);
    }

    // Fallback: protobuf-src bundled protoc (may not exist in all environments).
    let bundled = PathBuf::from(protobuf_src::protoc());
    if bundled.is_file() {
        return Some(bundled);
    }

    None
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // =========================================================================
    // Protobuf / gRPC Code Generation (using UARC proto definitions)
    // =========================================================================
    // UARC contains only the specification protos: common/v1, engine/v1
    // Coordinator API is Ato-specific and lives in ato-coordinator repo
    let uarc_path = std::env::var("UARC_PATH").unwrap_or_else(|_| "../uarc".to_string());

    if let Some(protoc_path) = resolve_protoc() {
        std::env::set_var("PROTOC", protoc_path);
    }

    tonic_build::configure()
        .build_server(true)
        .compile_well_known_types(false)
        .out_dir("src/proto")
        .compile_protos(
            &[
                format!("{}/proto/common/v1/common.proto", uarc_path),
                format!("{}/proto/engine/v1/api.proto", uarc_path),
            ],
            &[format!("{}/proto", uarc_path)], // .proto ファイルのインクルードパス
        )?;

    // =========================================================================
    // Cap'n Proto Code Generation (SSOT for CapsuleManifest)
    // =========================================================================
    // Requires `capnp` CLI tool installed: `brew install capnp` or `apt install capnproto`
    //
    // Generated code goes to src/ so it can be accessed as `crate::capsule_capnp`
    let capnp_out_dir = Path::new("src");

    // `capsule.capnp` includes Go annotations via `go.capnp` import for SSOT.
    // Rust builds should not depend on the Go toolchain or go.capnp being present,
    // so we compile from a sanitized copy that strips `$Go.*` lines.
    let uarc_schema_path = format!("{}/schema/capsule.capnp", uarc_path);
    let original_schema_path = Path::new(&uarc_schema_path);
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let sanitized_schema_dir = out_dir.join("capnp_sanitized");
    std::fs::create_dir_all(&sanitized_schema_dir)?;
    let sanitized_schema_path = sanitized_schema_dir.join("capsule.capnp");
    let original_schema = std::fs::read_to_string(original_schema_path)?;
    let sanitized_schema = original_schema
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed != "using Go = import \"/go.capnp\";" && !trimmed.starts_with("$Go.")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&sanitized_schema_path, sanitized_schema)?;

    capnpc::CompilerCommand::new()
        .file(&sanitized_schema_path)
        .src_prefix(&sanitized_schema_dir)
        .output_path(capnp_out_dir)
        .run()?;

    // Rerun if schema changes
    println!("cargo:rerun-if-changed={}/schema/capsule.capnp", uarc_path);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}/proto", uarc_path);
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PATH");

    Ok(())
}
