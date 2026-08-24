use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ato_computation::ComputationRef;
use ato_firecracker_guest_image::{GuestNetwork, GuestPhysicalBuildProfile, build_guest_image};
use ato_objects::{FsObjectStore, decode_bundle, import_bundle};
use ato_runtime_object_graph::standard_reference_registry;
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    capsule: PathBuf,
    #[arg(long)]
    target: String,
    #[arg(long)]
    base_image: String,
    #[arg(long)]
    guest_agent: PathBuf,
    #[arg(long)]
    kernel: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    work_root: PathBuf,
    #[arg(long, default_value_t = 512)]
    image_size_mib: u64,
    #[arg(long, default_value = "docker")]
    container_tool: String,
    #[arg(long, default_value = "/usr/sbin/mke2fs")]
    mke2fs: PathBuf,
    #[arg(long, default_value = "172.30.0.2")]
    guest_ip: String,
    #[arg(long, default_value = "172.30.0.1")]
    host_ip: String,
    #[arg(long, default_value = "255.255.255.0")]
    netmask: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    fs::create_dir_all(&args.work_root)?;
    let objects = FsObjectStore::open(args.work_root.join("objects"))?;
    let bundle = decode_bundle(&fs::read(&args.capsule)?)?;
    let imported = import_bundle(&bundle, &objects, &standard_reference_registry()?)?;
    let target = ComputationRef::parse(&args.target)?;
    ensure!(
        imported == target,
        "Capsule root does not equal requested target ComputationRef"
    );
    let profile = GuestPhysicalBuildProfile {
        base_image: args.base_image,
        guest_agent: args.guest_agent,
        kernel: args.kernel,
        image_size_mib: args.image_size_mib,
        network: GuestNetwork {
            guest_ip: args.guest_ip,
            host_ip: args.host_ip,
            netmask: args.netmask,
        },
        container_tool: args.container_tool,
        mke2fs: args.mke2fs,
    };
    let receipt = build_guest_image(&target, &objects, &profile, &args.work_root, &args.output)
        .context("build Firecracker guest image")?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}
