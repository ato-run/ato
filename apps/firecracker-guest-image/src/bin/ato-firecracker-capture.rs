#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io::Read;
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail, ensure};
    use ato_adapter_api::{AdapterRegistry, WorkspaceCapturePolicy};
    use ato_computation::{ComputationRef, ContentRef};
    use ato_contracts::HTTP_ENDPOINT_VERIFIER_ID;
    use ato_materializer_api::{ContractDescriptor, Materializer, MaterializerContext};
    use ato_materializer_vm_snapshot::{
        ActiveFirecrackerCaptureSpec, ArtifactRole, CpuContract, DeviceContract,
        FirecrackerActiveVmCaptureSource, FirecrackerBackend, FirecrackerBackendConfig,
        FirecrackerIngressGate, FirecrackerRecordCaptureBarrier, FirecrackerRecordCaptureLease,
        FirecrackerRestoreLayout, FreshFirecrackerConfig, FreshFirecrackerRealization,
        HostBackendContract, MemoryContract, NetworkContract, SealedRecordFrontierVerifier,
        VM_SNAPSHOT_MATERIALIZER_ID, VmSnapshotDescriptor, VmSnapshotError, VmSnapshotMaterializer,
        VsockContract,
    };
    use ato_objects::{
        FsObjectStore, GraphMaterialization, GraphRestoreCapability, ObjectResolver, ObjectStore,
        decode_bundle, import_bundle, read_exact_object,
    };
    use ato_record_writer::{
        CaptureBarrier, PausedCapture, RecordPipeline, RecordSchemaRegistry, RecordWriterConfig,
        verify_frontier_object,
    };
    use ato_runtime_object_graph::{
        VisibilityPolicy, build_runtime_object_graph_index, standard_reference_registry,
    };
    use clap::Parser;
    use serde::Serialize;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    #[derive(Debug, Parser)]
    pub struct Args {
        #[arg(long)]
        capsule: PathBuf,
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
        #[arg(long, default_value = "127.0.0.1:18420")]
        contract_surface: SocketAddr,
        #[arg(long, default_value_t = 1)]
        vcpu_count: u32,
        #[arg(long, default_value_t = 512)]
        memory_mib: u64,
        #[arg(long, default_value_t = 60)]
        ready_timeout_seconds: u64,
        #[arg(long)]
        output_index: PathBuf,
    }

    #[derive(Default)]
    struct CaptureIngressState {
        events: Mutex<Vec<&'static str>>,
    }

    struct CaptureIngress {
        state: Arc<CaptureIngressState>,
    }

    impl FirecrackerIngressGate for CaptureIngress {
        fn freeze(&mut self) -> Result<(), VmSnapshotError> {
            self.state
                .events
                .lock()
                .expect("ingress lock")
                .push("freeze");
            Ok(())
        }

        fn quiesce(&mut self) -> Result<(), VmSnapshotError> {
            self.state
                .events
                .lock()
                .expect("ingress lock")
                .push("quiesce");
            Ok(())
        }

        fn unfreeze(&mut self) -> Result<(), VmSnapshotError> {
            self.state
                .events
                .lock()
                .expect("ingress lock")
                .push("unfreeze");
            Ok(())
        }
    }

    struct CountedRecordBarrier {
        barrier: CaptureBarrier,
        calls: Arc<AtomicUsize>,
    }

    struct RecordLease {
        frontier: ContentRef,
        _paused: PausedCapture,
    }

    impl FirecrackerRecordCaptureLease for RecordLease {
        fn frontier_ref(&self) -> &ContentRef {
            &self.frontier
        }
    }

    impl FirecrackerRecordCaptureBarrier for CountedRecordBarrier {
        fn pause_and_seal(
            &self,
        ) -> Result<Box<dyn FirecrackerRecordCaptureLease>, VmSnapshotError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let paused = self
                .barrier
                .pause_and_seal()
                .map_err(|error| VmSnapshotError::Backend(error.to_string()))?;
            Ok(Box::new(RecordLease {
                frontier: paused.frontier.frontier_digest.clone(),
                _paused: paused,
            }))
        }
    }

    struct FrontierVerifier;

    impl SealedRecordFrontierVerifier for FrontierVerifier {
        fn verify(
            &self,
            reference: &ContentRef,
            objects: &dyn ObjectResolver,
        ) -> Result<(), VmSnapshotError> {
            verify_frontier_object(reference, objects)
                .map(|_| ())
                .map_err(|error| VmSnapshotError::InvalidDescriptor(error.to_string()))
        }
    }

    #[derive(Serialize)]
    struct CaptureReceipt {
        target_computation_ref: String,
        record_frontier_ref: String,
        vm_descriptor_ref: String,
        graph_index_digest: String,
        declared_object_count: usize,
        logical_bytes: u64,
        capture_barrier_calls: usize,
        ingress_events: Vec<&'static str>,
        firecracker_pid: u32,
        firecracker_version: String,
        rootfs_backing_path: String,
        netns: String,
        tap: String,
        vsock_path: String,
        hidden_http_ready_before_capture: bool,
        hidden_http_ready_after_capture: bool,
        externally_reachable_before_publish: bool,
        security_scan: String,
        cleanup_process: bool,
        cleanup_netns: bool,
        cleanup_session: bool,
    }

    pub fn run() -> Result<()> {
        let args = Args::parse();
        fs::create_dir_all(&args.work_root)?;
        let objects = Arc::new(FsObjectStore::open(args.work_root.join("objects"))?);
        let imported = import_bundle(
            &decode_bundle(&fs::read(&args.capsule)?)?,
            objects.as_ref(),
            &standard_reference_registry()?,
        )?;
        let target = ComputationRef::parse(&args.target)?;
        ensure!(
            imported == target,
            "Capsule root does not equal capture target"
        );
        let version = firecracker_version(&args.firecracker)?;
        let layout = FirecrackerRestoreLayout::default();
        let spec = capture_spec(&args, &version, layout.clone())?;
        let ingress = Arc::new(CaptureIngressState::default());
        let active = FreshFirecrackerRealization::boot(
            target.clone(),
            format!("staging-capture-{}", std::process::id()),
            FreshFirecrackerConfig {
                binary: args.firecracker.clone(),
                ip_binary: args.ip.clone(),
                work_root: args.work_root.join("active"),
                kernel: args.kernel.clone(),
                rootfs: args.rootfs.clone(),
                netns_name: args.netns.clone(),
                tap_device: args.tap.clone(),
                tap_host_cidr: args.tap_host_cidr.clone(),
                boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/sbin/init ip=172.30.0.2::172.30.0.1:255.255.255.0::eth0:off".to_owned(),
                vcpu_count: args.vcpu_count,
                memory_mib: args.memory_mib,
                api_timeout: Duration::from_secs(15),
            },
            spec,
            Box::new(CaptureIngress {
                state: Arc::clone(&ingress),
            }),
        )?;
        let pid = active.process_id().context("active VM process is absent")?;
        let session_root = active.session_root().to_path_buf();
        let externally_reachable_before_publish =
            TcpStream::connect_timeout(&args.guest_surface, Duration::from_millis(500)).is_ok();
        wait_for_guest_http(&args)?;

        let RecordPipeline {
            stylus,
            barrier,
            published,
            ..
        } = RecordPipeline::start(
            RecordWriterConfig::at(args.work_root.join("records"), "staging-2048-capture"),
            Arc::clone(&objects) as Arc<dyn ObjectStore>,
            RecordSchemaRegistry::default(),
        )?;
        let barrier_calls = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(FirecrackerActiveVmCaptureSource::new(
            Box::new(active),
            Arc::new(CountedRecordBarrier {
                barrier,
                calls: Arc::clone(&barrier_calls),
            }),
            args.work_root.join("captures"),
        ));
        let backend = Arc::new(FirecrackerBackend::with_capture_source(
            FirecrackerBackendConfig {
                binary: args.firecracker.clone(),
                ip_binary: args.ip.clone(),
                work_root: args.work_root.join("restore"),
                slot_id: "staging-capture".to_owned(),
                api_timeout: Duration::from_secs(15),
                tap_host_cidr: Some(args.tap_host_cidr.clone()),
                surface_relay: None,
            },
            source,
        ));
        let materializer = VmSnapshotMaterializer::new(backend, Arc::new(FrontierVerifier));
        let adapters = AdapterRegistry::default();
        let workspace_policy = WorkspaceCapturePolicy::secure_default();
        let workspace = args.work_root.join("materializer-workspace");
        fs::create_dir_all(&workspace)?;
        let contract = ContractDescriptor::new(
            HTTP_ENDPOINT_VERIFIER_ID,
            serde_json::json!({
                "address": args.contract_surface,
                "path": "/",
                "expected_status": 200
            }),
        )?;
        let descriptor_ref = materializer.encode(
            &target,
            &MaterializerContext {
                objects: objects.as_ref(),
                adapters: &adapters,
                records: &[],
                records_v2: &[],
                replay_anchor: None,
                record_frontier_ref: None,
                workspace: &workspace,
                workspace_policy: &workspace_policy,
                realization: None,
                contracts: &[contract],
                runner_capabilities: None,
            },
        )?;
        ensure!(
            barrier_calls.load(Ordering::SeqCst) == 1,
            "Capture Barrier was not called exactly once"
        );
        wait_for_guest_http(&args)?;
        let descriptor = load_descriptor(&descriptor_ref, objects.as_ref())?;
        ensure!(
            descriptor.target_computation_ref == target.to_string(),
            "VM descriptor target changed during capture"
        );
        let frontier_ref = descriptor
            .record_frontier_ref
            .clone()
            .context("captured descriptor omitted RecordFrontier")?;
        verify_frontier_object(&ContentRef::parse(&frontier_ref)?, objects.as_ref())?;
        scan_vm_artifacts(&descriptor, objects.as_ref())?;

        let references = standard_reference_registry()?;
        let index = build_runtime_object_graph_index(
            &target,
            &[GraphMaterialization {
                id: VM_SNAPSHOT_MATERIALIZER_ID.to_owned(),
                descriptor_ref: descriptor_ref.to_string(),
                restore_capability: GraphRestoreCapability::Supported,
            }],
            objects.as_ref(),
            &references,
            VisibilityPolicy::Public,
        )?;
        let index_bytes = serde_jcs::to_vec(&index)?;
        if let Some(parent) = args.output_index.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&args.output_index, &index_bytes)?;
        let index_digest = format!("blake3:{}", blake3::hash(&index_bytes).to_hex());
        let logical_bytes = index.logical_bytes()?;

        drop(materializer);
        drop(stylus);
        drop(published);
        thread::sleep(Duration::from_millis(100));
        let cleanup_process = !process_exists(pid);
        let cleanup_netns = !netns_exists(&args.ip, &args.netns)?;
        let cleanup_session = !session_root.exists();
        ensure!(cleanup_process, "capture Firecracker process leaked");
        ensure!(cleanup_netns, "capture network namespace leaked");
        ensure!(cleanup_session, "capture session directory leaked");
        let ingress_events = ingress.events.lock().expect("ingress lock").clone();
        ensure!(
            ingress_events == ["freeze", "quiesce", "unfreeze"],
            "capture ingress ordering changed"
        );

        println!(
            "{}",
            serde_json::to_string_pretty(&CaptureReceipt {
                target_computation_ref: target.to_string(),
                record_frontier_ref: frontier_ref,
                vm_descriptor_ref: descriptor_ref.to_string(),
                graph_index_digest: index_digest,
                declared_object_count: index.objects.len(),
                logical_bytes,
                capture_barrier_calls: barrier_calls.load(Ordering::SeqCst),
                ingress_events,
                firecracker_pid: pid,
                firecracker_version: version,
                rootfs_backing_path: layout.rootfs_backing_path,
                netns: args.netns,
                tap: args.tap,
                vsock_path: layout.vsock_uds_path.unwrap_or_default(),
                hidden_http_ready_before_capture: true,
                hidden_http_ready_after_capture: true,
                externally_reachable_before_publish,
                security_scan: "pass".to_owned(),
                cleanup_process,
                cleanup_netns,
                cleanup_session,
            })?
        );
        Ok(())
    }

    fn capture_spec(
        args: &Args,
        version: &str,
        layout: FirecrackerRestoreLayout,
    ) -> Result<ActiveFirecrackerCaptureSpec> {
        Ok(ActiveFirecrackerCaptureSpec {
            captured_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
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
            firecracker_version: version.to_owned(),
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
                required_features: BTreeSet::from([
                    "vsock-uds".to_owned(),
                    "vsock-override".to_owned(),
                ]),
                uds_path: layout.vsock_uds_path.clone(),
            },
            memory_contract: MemoryContract {
                guest_memory_mib: args.memory_mib,
                minimum_host_memory_mib: args.memory_mib,
            },
            state_contract_refs: Vec::new(),
            placement_hint: Some("staging-linux-runner".to_owned()),
            restore_layout: layout,
        })
    }

    fn wait_for_guest_http(args: &Args) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(args.ready_timeout_seconds);
        let url = format!("http://{}/", args.guest_surface);
        let mut last_error = String::new();
        while Instant::now() < deadline {
            let output = Command::new(&args.ip)
                .args(["netns", "exec", &args.netns])
                .arg(&args.curl)
                .args(["--fail", "--silent", "--show-error", "--max-time", "2"])
                .arg(&url)
                .output()?;
            if output.status.success() && !output.stdout.is_empty() {
                return Ok(());
            }
            last_error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            thread::sleep(Duration::from_millis(250));
        }
        bail!("guest HTTP endpoint did not become ready: {last_error}")
    }

    fn load_descriptor(
        reference: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<VmSnapshotDescriptor> {
        let metadata = objects.metadata(reference)?;
        let bytes = read_exact_object(objects, reference, metadata.size, 16 * 1024 * 1024)?;
        let descriptor: VmSnapshotDescriptor = serde_json::from_slice(&bytes)?;
        ensure!(
            serde_jcs::to_vec(&descriptor)? == bytes,
            "VM descriptor is not canonical JCS"
        );
        Ok(descriptor)
    }

    fn scan_vm_artifacts(
        descriptor: &VmSnapshotDescriptor,
        objects: &dyn ObjectResolver,
    ) -> Result<()> {
        const PATTERNS: &[(&str, &[u8])] = &[
            ("private-key", b"-----BEGIN PRIVATE KEY-----"),
            ("rsa-private-key", b"-----BEGIN RSA PRIVATE KEY-----"),
            (
                "openssh-private-key",
                b"-----BEGIN OPENSSH PRIVATE KEY-----",
            ),
            ("authorization-bearer", b"Authorization: Bearer "),
            ("cloudflare-token", b"CLOUDFLARE_API_TOKEN="),
            ("runner-token", b"ATO_RUNNER_TOKEN="),
            ("ato-api-token", b"ATO_API_TOKEN="),
            ("aws-access-key", b"AKIA"),
        ];
        let maximum = PATTERNS
            .iter()
            .map(|(_, pattern)| pattern.len())
            .max()
            .unwrap_or(1);
        for artifact in &descriptor.artifacts {
            ensure!(
                matches!(
                    artifact.role,
                    ArtifactRole::Memory
                        | ArtifactRole::Rootfs
                        | ArtifactRole::Vmstate
                        | ArtifactRole::Metadata
                ),
                "unexpected VM artifact role"
            );
            let mut chunks = artifact.chunks.clone();
            chunks.sort_by_key(|chunk| chunk.ordinal);
            let mut tail = Vec::new();
            for chunk in chunks {
                let reference = ContentRef::parse(&chunk.content_ref)?;
                let mut reader = objects.open(&reference)?;
                let mut buffer = vec![0_u8; 64 * 1024];
                loop {
                    let read = reader.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    let mut window = tail;
                    window.extend_from_slice(&buffer[..read]);
                    for (label, pattern) in PATTERNS {
                        if window
                            .windows(pattern.len())
                            .any(|candidate| candidate == *pattern)
                        {
                            bail!("VM artifact security scan rejected marker `{label}`");
                        }
                    }
                    tail = window[window.len().saturating_sub(maximum - 1)..].to_vec();
                }
            }
        }
        Ok(())
    }

    fn firecracker_version(binary: &Path) -> Result<String> {
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

    fn netns_exists(ip: &Path, expected: &str) -> Result<bool> {
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
    anyhow::bail!("real Firecracker capture requires Linux/KVM")
}
