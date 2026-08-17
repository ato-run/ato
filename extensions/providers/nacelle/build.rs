use std::{
    env,
    ffi::OsString,
    fs,
    io::{BufRead as _, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
};

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
        "--manifest-path",
        "ebpf/Cargo.toml",
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

fn write_placeholder_ebpf() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let dst = out_dir.join("nacelle-ebpf");
    if !dst.exists() {
        fs::write(&dst, [])?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    aya_build::emit_bpf_target_arch_cfg();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        let target = env::var("TARGET").unwrap_or_default();
        let host = env::var("HOST").unwrap_or_default();
        if target == host {
            if let Err(err) = build_ebpf_program() {
                println!("cargo:warning=Failed to build eBPF program: {err}");
                write_placeholder_ebpf()?;
            }
        } else {
            println!(
                "cargo:warning=Skipping eBPF build for cross-compilation (host={host}, target={target})"
            );
            write_placeholder_ebpf()?;
        }
    } else {
        write_placeholder_ebpf()?;
    }

    Ok(())
}
