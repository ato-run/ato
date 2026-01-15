use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

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
    let bundled = protobuf_src::protoc();
    if bundled.is_file() {
        return Some(bundled);
    }

    None
}

fn target_arch_fixup(target_arch: &str) -> &str {
    if target_arch.starts_with("riscv64") {
        "riscv64"
    } else {
        target_arch
    }
}

fn build_ebpf_program() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let endian = env::var("CARGO_CFG_TARGET_ENDIAN")?;
    let target = match endian.as_str() {
        "big" => "bpfeb",
        "little" => "bpfel",
        _ => return Err(format!("unsupported endian={endian}").into()),
    };
    let target = format!("{target}-unknown-none");

    let bpf_target_arch =
        env::var("AYA_BPF_TARGET_ARCH").or_else(|_| env::var("CARGO_CFG_TARGET_ARCH"))?;
    let bpf_target_arch = target_arch_fixup(&bpf_target_arch).to_string();

    println!("cargo:rerun-if-changed=ebpf");

    let target_dir = out_dir.join("ebpf-target");
    let mut cmd = Command::new("rustup");
    cmd.args([
        "run",
        "nightly",
        "cargo",
        "build",
        "--package",
        "nacelle-ebpf",
        "-Z",
        "build-std=core",
        "--bins",
        "--release",
        "--target",
        &target,
        "--no-default-features",
    ]);
    cmd.arg("--target-dir").arg(&target_dir);

    const SEPARATOR: &str = "\x1f";
    let mut rustflags = OsString::new();
    for s in [
        "--cfg=bpf_target_arch=\"",
        &bpf_target_arch,
        "\"",
        SEPARATOR,
        "-Cdebuginfo=2",
        SEPARATOR,
        "-Clink-arg=--btf",
    ] {
        rustflags.push(s);
    }
    cmd.env("CARGO_ENCODED_RUSTFLAGS", rustflags);
    for key in ["RUSTC", "RUSTC_WORKSPACE_WRAPPER"] {
        cmd.env_remove(key);
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child.stdout.take().expect("stdout");
    let stderr = child.stderr.take().expect("stderr");

    let stdout_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            println!("cargo:warning={line}");
        }
    });
    let stderr_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            println!("cargo:warning={line}");
        }
    });

    let status = child.wait()?;
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    if !status.success() {
        return Err(format!("ebpf build failed: {status}").into());
    }

    let built = target_dir
        .join(&target)
        .join("release")
        .join("nacelle-ebpf");
    let dst = out_dir.join("nacelle-ebpf");
    if dst.is_dir() {
        fs::remove_dir_all(&dst)?;
    }
    fs::copy(&built, &dst)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    aya_build::emit_bpf_target_arch_cfg();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        build_ebpf_program().expect("Failed to build eBPF program");
    }

    // =========================================================================
    // Protobuf / gRPC Code Generation (using local proto definitions)
    // =========================================================================
    // Proto definitions are now self-contained in the proto/ directory (nacelle package)
    let proto_dir = "proto";

    if let Some(protoc_path) = resolve_protoc() {
        env::set_var("PROTOC", protoc_path);
    }

    tonic_build::configure()
        .build_server(true)
        .compile_well_known_types(false)
        .out_dir("src/proto")
        .compile_protos(
            &[
                format!("{}/common/v1/common.proto", proto_dir),
                format!("{}/engine/v1/api.proto", proto_dir),
            ],
            &[proto_dir], // .proto ファイルのインクルードパス
        )?;

    // =========================================================================
    // Cap'n Proto Code Generation (SSOT for CapsuleManifest)
    // =========================================================================
    // Requires `capnp` CLI tool installed: `brew install capnp` or `apt install capnproto`
    //
    // Generated code goes to src/ so it can be accessed as `crate::capsule_capnp`
    let capnp_out_dir = Path::new("src");

    // For now, using a local capsule.capnp if needed, or skipping if not available
    // TODO: Consider adding capsule.capnp to this repository if needed for standalone builds
    let capnp_schema_path = "schema/capsule.capnp";
    
    if Path::new(capnp_schema_path).exists() {
        capnpc::CompilerCommand::new()
            .file(capnp_schema_path)
            .src_prefix("schema")
            .output_path(capnp_out_dir)
            .run()?;
        
        // Rerun if schema changes
        println!("cargo:rerun-if-changed={}", capnp_schema_path);
    }

    Ok(())
}
