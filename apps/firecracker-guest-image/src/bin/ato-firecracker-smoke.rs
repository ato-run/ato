#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeSet;
    use std::net::{SocketAddr, TcpStream};
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail, ensure};
    use ato_computation::ComputationRef;
    use ato_materializer_vm_snapshot::{
        ActiveFirecrackerCaptureSpec, CpuContract, DeviceContract, FirecrackerIngressGate,
        FirecrackerRestoreLayout, FreshFirecrackerConfig, FreshFirecrackerRealization,
        HostBackendContract, MemoryContract, NetworkContract, VsockContract,
    };
    use clap::Parser;
    use serde::Serialize;

    #[derive(Debug, Parser)]
    struct Args {
        #[arg(long)]
        target: String,
        #[arg(long)]
        rootfs: PathBuf,
        #[arg(long)]
        kernel: PathBuf,
        #[arg(long, default_value = "/usr/local/bin/firecracker")]
        firecracker: PathBuf,
        #[arg(long, default_value = "/usr/sbin/ip")]
        ip: PathBuf,
        #[arg(long, default_value = "curl")]
        curl: PathBuf,
        #[arg(long)]
        work_root: PathBuf,
        #[arg(long)]
        netns: String,
        #[arg(long, default_value = "tap0")]
        tap: String,
        #[arg(long, default_value = "172.30.0.1/24")]
        tap_host_cidr: String,
        #[arg(long, default_value = "172.30.0.2:38865")]
        guest_surface: SocketAddr,
        #[arg(long, default_value_t = 1)]
        vcpu_count: u32,
        #[arg(long, default_value_t = 512)]
        memory_mib: u64,
        #[arg(long, default_value_t = 60)]
        ready_timeout_seconds: u64,
    }

    #[derive(Default)]
    struct SmokeIngress;

    impl FirecrackerIngressGate for SmokeIngress {
        fn freeze(&mut self) -> Result<(), ato_materializer_vm_snapshot::VmSnapshotError> {
            Ok(())
        }

        fn quiesce(&mut self) -> Result<(), ato_materializer_vm_snapshot::VmSnapshotError> {
            Ok(())
        }

        fn unfreeze(&mut self) -> Result<(), ato_materializer_vm_snapshot::VmSnapshotError> {
            Ok(())
        }
    }

    #[derive(Serialize)]
    struct SmokeReceipt {
        target_computation_ref: String,
        firecracker_pid: u32,
        firecracker_version: String,
        netns: String,
        tap: String,
        session_root: String,
        guest_surface: String,
        hidden_surface_http_status: u16,
        response_bytes: usize,
        response_blake3: String,
        externally_reachable_before_publish: bool,
        cleanup_process: bool,
        cleanup_netns: bool,
        cleanup_session: bool,
    }

    pub fn run() -> Result<()> {
        let args = Args::parse();
        let target = ComputationRef::parse(&args.target)?;
        let version = firecracker_version(&args.firecracker)?;
        let layout = FirecrackerRestoreLayout::default();
        let spec = ActiveFirecrackerCaptureSpec {
            captured_at: "staging-smoke-not-captured".to_owned(),
            snapshot_format: "fc-full-file-v1".to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            guest_os: "linux".to_owned(),
            host_backend_contract: HostBackendContract {
                backend_id: "firecracker".to_owned(),
                host_os: "linux".to_owned(),
                required_features: BTreeSet::from(["kvm".to_owned()]),
            },
            cpu_contract: CpuContract {
                vcpu_count: args.vcpu_count,
                required_features: BTreeSet::new(),
            },
            firecracker_version: version.clone(),
            device_contract: DeviceContract {
                required_features: BTreeSet::from([
                    "kvm".to_owned(),
                    "virtio-blk".to_owned(),
                    "content-addressed-rootfs-path-v1".to_owned(),
                ]),
            },
            network_contract: NetworkContract {
                required_features: BTreeSet::from([
                    "tap".to_owned(),
                    "network-namespace".to_owned(),
                ]),
                tap_device: Some(args.tap.clone()),
            },
            vsock_contract: VsockContract {
                required_features: BTreeSet::from(["vsock-uds".to_owned()]),
                uds_path: layout.vsock_uds_path.clone(),
            },
            memory_contract: MemoryContract {
                guest_memory_mib: args.memory_mib,
                minimum_host_memory_mib: args.memory_mib,
            },
            state_contract_refs: Vec::new(),
            placement_hint: Some("staging-smoke".to_owned()),
            restore_layout: layout,
        };
        let config = FreshFirecrackerConfig {
        binary: args.firecracker.clone(),
        ip_binary: args.ip.clone(),
        work_root: args.work_root.clone(),
        kernel: args.kernel.clone(),
        rootfs: args.rootfs.clone(),
        netns_name: args.netns.clone(),
        tap_device: args.tap.clone(),
        tap_host_cidr: args.tap_host_cidr.clone(),
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/init ip=172.30.0.2::172.30.0.1:255.255.255.0::eth0:off".to_owned(),
        vcpu_count: args.vcpu_count,
        memory_mib: args.memory_mib,
        api_timeout: Duration::from_secs(15),
    };
        let realization = FreshFirecrackerRealization::boot(
            target.clone(),
            format!("staging-smoke-{}", std::process::id()),
            config,
            spec,
            Box::<SmokeIngress>::default(),
        )?;
        let pid = realization
            .process_id()
            .context("fresh Firecracker has no process")?;
        let session_root = realization.session_root().to_path_buf();
        let external_before_publish =
            TcpStream::connect_timeout(&args.guest_surface, Duration::from_millis(500)).is_ok();
        let body = wait_for_guest_http(&args, realization.network_namespace())?;
        let body_digest = blake3::hash(&body).to_hex().to_string();

        drop(realization);
        let cleanup_process = !process_exists(pid);
        let cleanup_netns = !netns_exists(&args.ip, &args.netns)?;
        let cleanup_session = !session_root.exists();
        ensure!(cleanup_process, "Firecracker process leaked after smoke");
        ensure!(cleanup_netns, "network namespace leaked after smoke");
        ensure!(
            cleanup_session,
            "Firecracker session directory leaked after smoke"
        );

        println!(
            "{}",
            serde_json::to_string_pretty(&SmokeReceipt {
                target_computation_ref: target.to_string(),
                firecracker_pid: pid,
                firecracker_version: version,
                netns: args.netns,
                tap: args.tap,
                session_root: session_root.display().to_string(),
                guest_surface: args.guest_surface.to_string(),
                hidden_surface_http_status: 200,
                response_bytes: body.len(),
                response_blake3: body_digest,
                externally_reachable_before_publish: external_before_publish,
                cleanup_process,
                cleanup_netns,
                cleanup_session,
            })?
        );
        Ok(())
    }

    fn wait_for_guest_http(args: &Args, netns: &str) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_secs(args.ready_timeout_seconds);
        let url = format!("http://{}/", args.guest_surface);
        let mut last_error = String::new();
        while Instant::now() < deadline {
            let output = Command::new(&args.ip)
                .args(["netns", "exec", netns])
                .arg(&args.curl)
                .args(["--fail", "--silent", "--show-error", "--max-time", "2"])
                .arg(&url)
                .output()
                .context("run hidden guest HTTP probe")?;
            if output.status.success() {
                ensure!(
                    !output.stdout.is_empty(),
                    "guest returned an empty HTTP body"
                );
                return Ok(output.stdout);
            }
            last_error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            thread::sleep(Duration::from_millis(250));
        }
        bail!("guest HTTP endpoint did not become ready: {last_error}")
    }

    fn firecracker_version(binary: &PathBuf) -> Result<String> {
        let output = Command::new(binary).arg("--version").output()?;
        ensure!(output.status.success(), "Firecracker version probe failed");
        let stdout = String::from_utf8(output.stdout)?;
        stdout
            .split_whitespace()
            .find_map(|value| value.strip_prefix('v'))
            .map(ToOwned::to_owned)
            .context("Firecracker version output is unsupported")
    }

    fn process_exists(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }

    fn netns_exists(ip: &PathBuf, expected: &str) -> Result<bool> {
        let output = Command::new(ip).args(["netns", "list"]).output()?;
        ensure!(
            output.status.success(),
            "network namespace cleanup probe failed"
        );
        Ok(String::from_utf8(output.stdout)?
            .lines()
            .any(|line| line.split_whitespace().next() == Some(expected)))
    }
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("real Firecracker smoke requires Linux/KVM")
}
