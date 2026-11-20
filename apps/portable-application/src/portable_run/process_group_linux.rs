//! Publication launches each process service in its own Linux namespaces.
//! The owner holds all host listeners and routes only declared peer edges.

use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;

use ato_formation::authoring::{BoundDerivation, BoundStep};
use ato_runtime_attempt::network_bridge::{HostBridge, INGRESS_ROOT, IngressBridge, PEER_ROOT};

use super::*;

pub(super) struct ScopedNetwork {
    ingress: BTreeMap<SocketAddr, IngressBridge>,
    peers: Vec<HostBridge>,
}

impl ScopedNetwork {
    fn allocate(
        root: &Path,
        derivation: &BoundDerivation,
    ) -> Result<(Self, BTreeMap<String, SocketAddr>)> {
        let mut ingress = BTreeMap::new();
        let mut addresses = BTreeMap::new();
        for step in &derivation.steps {
            fs::create_dir_all(root.join(&step.id).join("ingress"))?;
        }
        for port in &derivation.ports {
            let bridge = IngressBridge::allocate(&root.join(&port.from).join("ingress"))?;
            let address = SocketAddr::from((Ipv4Addr::LOCALHOST, bridge.host_port()));
            ingress.insert(address, bridge);
            addresses.insert(port.id.clone(), address);
        }
        Ok((
            Self {
                ingress,
                peers: Vec::new(),
            },
            addresses,
        ))
    }
}

impl PortableBundleExecutor<'_> {
    pub(super) fn start_bounded_process_group(
        &self,
        workspace: PathBuf,
        control: &ExecutionControl,
    ) -> Result<Realized, RealizeFailure> {
        let root = self.runtime_root.join("process");
        let owned = vec![workspace.clone(), root.clone()];
        self.launch_bounded_process_group(&workspace, &root, control, &owned)
            .map_err(|error| process_group_launch_failure(error, &owned))
    }

    fn launch_bounded_process_group(
        &self,
        workspace: &Path,
        root: &Path,
        control: &ExecutionControl,
        owned: &[PathBuf],
    ) -> Result<Realized> {
        let workspace = workspace.canonicalize()?;
        fs::create_dir(root)?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        let root = root.canonicalize()?;
        let derivation = &self.validated.derivation;
        let (mut namespace, addresses) = ScopedNetwork::allocate(&root, derivation)?;
        let mut services = Vec::with_capacity(derivation.steps.len());
        let mut runtime_identity = BTreeMap::new();
        for step in &derivation.steps {
            control.remaining(AttemptPhase::Launch)?;
            let (executable, version) = resolve_service_python(step, Some(control))?;
            let service_root = root.join(&step.id);
            let mut environment = self.process_service_environment(step);
            let bind: Vec<_> = derivation
                .ports
                .iter()
                .filter(|port| port.from == step.id)
                .map(|port| {
                    let address = addresses[&port.id];
                    environment
                        .insert(endpoint_port_env_name(&port.id), address.port().to_string());
                    address
                })
                .collect();
            let readiness = *bind.first().context("service serves no Port")?;
            let mut peers = BTreeMap::new();
            for port in &step.peer_ports {
                let address = addresses[port];
                let socket = service_root.join(format!("peer-{}.sock", address.port()));
                namespace.peers.push(HostBridge::start(&socket, address)?);
                environment.insert(endpoint_port_env_name(port), address.port().to_string());
                peers.insert(address.port(), socket);
            }
            for name in ["ATO_RUNTIME_DIR", "HOME", "TMPDIR", "TMP", "TEMP"] {
                environment.insert(name.into(), "/tmp".into());
            }
            environment.insert("XDG_CACHE_HOME".into(), "/tmp/.cache".into());
            ato_runtime_attempt::launch::executable_gate::validate_derivation_executable(
                &step.argv[0],
                &workspace.join(&step.cwd),
                &environment,
                derivation,
            )
            .context(ato_runtime_attempt::launch::executable_gate::context_for_launch())?;
            let command = scoped_service_command(
                &workspace,
                &service_root,
                self.context.sandbox_shim,
                step,
                &executable,
                &environment,
                &bind,
                &peers,
            )?;
            runtime_identity.insert(step.id.clone(), (executable, version));
            services.push(ServiceSpec {
                name: step.id.clone(),
                command,
                cwd: PathBuf::from("."),
                // Runtime commands must never receive source-controlled variables.
                environment: BTreeMap::new(),
                readiness,
                helpers: Vec::new(),
            });
        }
        let group =
            ProcessServiceGroup::start_bounded(&workspace, services, control, &namespace.ingress)?;
        let endpoints: BTreeMap<_, _> = addresses
            .into_iter()
            .map(|(port, address)| (port, format!("http://{address}")))
            .collect();
        let evidence = process_group_evidence("bwrap_landlock", endpoints.clone());
        let mut realized = self.realized_process_group(
            ProcessServiceGroupCandidate {
                group: Some(group),
                broker: None,
                namespace: Some(namespace),
                endpoints,
                owned: owned.to_vec(),
            },
            &runtime_identity,
        );
        realized.evidence = Some(evidence);
        Ok(realized)
    }
}

#[allow(clippy::too_many_arguments)]
fn scoped_service_command(
    workspace: &Path,
    service_root: &Path,
    shim: &Path,
    step: &BoundStep,
    executable: &str,
    environment: &BTreeMap<String, String>,
    bind: &[SocketAddr],
    peers: &BTreeMap<u16, PathBuf>,
) -> Result<Vec<String>> {
    let interpreter_root = Path::new(executable)
        .parent()
        .and_then(Path::parent)
        .context("portable runtime has no readable installation root")?;
    let policy = SandboxPolicy::new()
        .allow_read_write(["/tmp"])
        .allow_read_only([
            Path::new("/app"),
            interpreter_root,
            Path::new("/usr"),
            Path::new("/bin"),
            Path::new("/sbin"),
            Path::new("/lib"),
            Path::new("/lib64"),
            Path::new("/etc"),
            Path::new("/dev"),
            Path::new("/proc"),
        ])
        .with_network(false)
        .allow_tcp_bind(bind.iter().map(SocketAddr::port))
        .allow_tcp_connect(peers.keys().copied());
    let policy_path = service_root.join("sandbox-policy.json");
    fs::write(&policy_path, serde_json::to_vec(&policy)?)?;
    let environment_path = service_root.join("environment.json");
    fs::write(&environment_path, serde_json::to_vec(environment)?)?;
    let mut command = process_bwrap_base(workspace, interpreter_root, false)?;
    command.extend(["--dir", "/.ato", "--dir", PEER_ROOT].map(str::to_owned));
    for (flag, path, guest) in [
        ("--ro-bind", shim, "/.ato/ato"),
        ("--ro-bind", policy_path.as_path(), "/.ato/policy.json"),
        (
            "--ro-bind",
            environment_path.as_path(),
            "/.ato/environment.json",
        ),
        (
            "--bind",
            service_root.join("ingress").as_path(),
            INGRESS_ROOT,
        ),
    ] {
        command.extend([
            flag.to_owned(),
            path.display().to_string(),
            guest.to_owned(),
        ]);
    }
    for (port, socket) in peers {
        command.extend([
            "--ro-bind".into(),
            socket.display().to_string(),
            format!("{PEER_ROOT}/{port}.sock"),
        ]);
    }
    command.extend([
        "--chdir".into(),
        format!("/app/{}", step.cwd),
        "/.ato/ato".into(),
    ]);
    command.extend(
        [
            "sandbox-exec",
            "--policy",
            "/.ato/policy.json",
            "--workload-env-file",
            "/.ato/environment.json",
            "--require-landlock",
            "--scoped-ingress",
            "--relay-shim",
            "/.ato/ato",
        ]
        .map(str::to_owned),
    );
    for address in bind {
        command.extend(["--ingress-port".into(), address.port().to_string()]);
    }
    for port in peers.keys() {
        command.extend(["--peer-port".into(), port.to_string()]);
    }
    command.extend(["--".into(), executable.to_owned()]);
    command.extend(step.argv[1..].iter().cloned());
    Ok(command)
}
