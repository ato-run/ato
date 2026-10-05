//! A registered proposal is lowered into the existing isolated source OCI
//! builder and common OCI Runtime. The builder's weak generated K is not used.
use super::ExplorationTicket;
#[cfg(target_os = "linux")]
use anyhow::{Context, Result};
use ato_formation::{requirements::ExecutionPhase, source_oci_plan::SourceOciRecipe};
use ato_portable_application::source_oci;
use ato_portable_application::source_oci::BaseImageInput;
use ato_runtime_attempt::{
    control::{AttemptPhase, ExecutionControl},
    plan::PlannedCandidate,
    realize::{CandidateRealizer, RealizeFailure, Realized},
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOciSandbox {
    /// Frozen operator resource ceiling and approved image digests. These
    /// owner bindings are not sent to the provider or inherited by later Run.
    pub maximum_recipe: SourceOciRecipe,
    pub base_images: Vec<BaseImageInput>,
    /// Short operator-owned root, required by the existing isolated daemon.
    pub builder_root: PathBuf,
    pub runtime_socket: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<source_oci::SessionTools>,
}

impl SourceOciSandbox {
    /// Bound capabilities describe this owner process, not the presence of a CLI.
    pub(super) fn runtime_available(&self) -> bool {
        self.maximum_recipe.validate().is_ok()
            && self.tools.clone().unwrap_or_default().docker.is_file()
            && ato_adapter_oci::isolated_runtime_available(&self.runtime_socket)
    }
    pub(super) fn available(&self) -> bool {
        if self.maximum_recipe.validate().is_err()
            || self.builder_root.to_str().is_none_or(|p| p.len() > 31)
            || !self.builder_root.is_absolute()
            || self.maximum_recipe.base_images.iter().any(|base| {
                !self.base_images.iter().any(|binding| {
                    base.reference == binding.reference
                        && base.pinned_digest == binding.pinned_digest
                        && binding.archive.is_file()
                })
            })
            || !ato_adapter_oci::isolated_runtime_available(&self.runtime_socket)
        {
            return false;
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let tools = self.tools.clone().unwrap_or_default();
            let owner = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find_map(|l| l.strip_prefix("Uid:"))
                        .and_then(|v| v.split_whitespace().nth(1))
                        .map(str::to_owned)
                })
                == Some("0".into());
            let controllers = std::fs::read_to_string("/sys/fs/cgroup/cgroup.subtree_control")
                .unwrap_or_default();
            owner
                && ["cpu", "memory", "pids"]
                    .iter()
                    .all(|c| controllers.split_whitespace().any(|v| v == *c))
                && tools.buildx.as_ref().map_or_else(
                    || {
                        [
                            "/usr/libexec/docker/cli-plugins/docker-buildx",
                            "/usr/lib/docker/cli-plugins/docker-buildx",
                        ]
                        .iter()
                        .any(|p| Path::new(p).is_file())
                    },
                    |p| p.is_file(),
                )
                && [
                    &tools.sh,
                    &tools.docker,
                    &tools.dockerd,
                    &tools.unshare,
                    &tools.mkfs_ext4,
                    &tools.mount,
                    &tools.umount,
                    &tools.kill,
                    &tools.ip,
                    &tools.nsenter,
                    &tools.iptables,
                    &tools.sysctl,
                ]
                .iter()
                .all(|p| {
                    std::fs::metadata(p)
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) struct SourceOciRealizer<'a> {
    pub planned: &'a PlannedCandidate,
    pub archive: &'a std::fs::File,
    pub artifact_root: &'a Path,
    pub stored_limit: u64,
    pub archive_digest: &'a str,
    pub source_root: &'a Path,
    pub ticket: &'a ExplorationTicket,
    pub configured: Option<&'a SourceOciSandbox>,
    pub artifact: RefCell<Option<serde_json::Value>>,
}

fn refusal(code: &str, message: &str) -> Option<ato_formation::request::AttemptFailure> {
    Some(ato_formation::request::AttemptFailure {
        code: code.into(),
        stage: "admission".into(),
        message: message.into(),
    })
}

impl CandidateRealizer for SourceOciRealizer<'_> {
    fn admit(
        &self,
        _: &ato_formation::request::RuntimeProfile,
    ) -> Option<ato_formation::request::AttemptFailure> {
        let d = &self.planned.derivation;
        if let Err(e) = d.requirements.within(&self.ticket.ceiling) {
            return refusal(e.0, "outside frozen exploration ceiling");
        }
        if let Some(f) = ato_runtime_attempt::exploration_realizer::admit_isolated_authority(
            d,
            &self.ticket.ceiling,
        ) {
            return Some(f);
        }
        let Some(config) = self.configured else {
            return refusal(
                "source_oci_builder_unavailable",
                "operator has not configured the isolated source OCI builder",
            );
        };
        if let Some(image) = d.runtimes.get("oci.image") {
            let bound = config.base_images.iter().any(|b| {
                let pinned = if b.reference.contains('@') {
                    b.reference.clone()
                } else {
                    format!("{}@{}", b.reference, b.pinned_digest)
                };
                image == &pinned
                    && config.maximum_recipe.base_images.iter().any(|approved| {
                        approved.reference == b.reference
                            && approved.pinned_digest == b.pinned_digest
                    })
            });
            if !bound {
                return refusal(
                    "oci_image_unbound",
                    "selected immutable image has no approved archive binding",
                );
            }
            if d.runtimes.get("oci.platform") != Some(&config.maximum_recipe.platform)
                || !cfg!(target_os = "linux")
            {
                return refusal(
                    "unsupported_capability",
                    "bound OCI image platform is unavailable on this Runtime",
                );
            }
            if !d.requirements.network.is_empty() || !d.variable_bindings.is_empty() {
                return refusal(
                    "unsupported_capability",
                    "this image Runtime does not yet enforce runtime egress or variable bindings",
                );
            }
            return None;
        }
        let Some(recipe) = &d.source_oci else {
            return refusal(
                "source_oci_recipe_missing",
                "canonical D has no source OCI recipe",
            );
        };
        if recipe.validate().is_err() || config.maximum_recipe.validate().is_err() {
            return refusal(
                "source_oci_recipe_invalid",
                "registered source OCI recipe validation failed",
            );
        }
        let max = &config.maximum_recipe;
        let fits = |a: &ato_formation::source_oci_plan::OciLimits,
                    b: &ato_formation::source_oci_plan::OciLimits| {
            a.memory_bytes <= b.memory_bytes
                && a.cpu_limit_millis <= b.cpu_limit_millis
                && a.pids_limit <= b.pids_limit
        };
        if recipe.platform != max.platform
            || !fits(&recipe.build, &max.build)
            || !fits(&recipe.runtime, &max.runtime)
            || recipe.build_disk_bytes > max.build_disk_bytes
            || recipe.build_timeout_seconds > max.build_timeout_seconds
            || recipe.max_archive_bytes > max.max_archive_bytes
            || recipe
                .base_images
                .iter()
                .any(|b| !max.base_images.contains(b))
        {
            return refusal(
                "exploration_authority_exceeded",
                "source OCI recipe exceeds the operator's frozen builder ceiling",
            );
        }
        if recipe.base_images.iter().any(|b| {
            !config
                .base_images
                .iter()
                .any(|i| i.reference == b.reference && i.pinned_digest == b.pinned_digest)
        }) {
            return refusal(
                "source_oci_base_unavailable",
                "a pinned base archive binding is unavailable; no implicit registry fetch",
            );
        }
        if d.requirements
            .network
            .iter()
            .any(|n| n.phase != ExecutionPhase::Build || n.port != 443)
        {
            return refusal(
                "unsupported_source_oci_network_phase",
                "this builder enforces build-phase HTTPS; runtime egress is not supported",
            );
        }
        if d.state
            .iter()
            .any(|s| s.access != ato_formation::authoring::StateAccess::ReadWrite)
        {
            return refusal(
                "unsupported_source_oci_state_access",
                "the existing VOLUME profile requires writable isolated state",
            );
        }
        #[cfg(not(target_os = "linux"))]
        {
            refusal(
                "unsupported_toolchain",
                "isolated source OCI execution requires native Linux",
            )
        }
        #[cfg(target_os = "linux")]
        {
            // Management authority belongs to this preconfigured worker,
            // never to the Dockerfile or proposal. Do not invoke sudo here.
            if unsafe { libc::geteuid() } != 0 {
                return refusal(
                    "source_oci_builder_management_unavailable",
                    "the isolated builder requires a preconfigured owner process",
                );
            }
            None
        }
    }
    fn realize(
        &self,
        attempt_id: &str,
        root: &Path,
    ) -> std::result::Result<Realized, RealizeFailure> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (attempt_id, root);
            Err(anyhow::anyhow!("unsupported_toolchain").into())
        }
        #[cfg(target_os = "linux")]
        {
            self.realize_linux(attempt_id, root, None)
        }
    }
    fn realize_controlled(
        &self,
        attempt_id: &str,
        root: &Path,
        control: &ExecutionControl,
    ) -> std::result::Result<Realized, RealizeFailure> {
        control.remaining(AttemptPhase::Source)?;
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (attempt_id, root);
            Err(anyhow::anyhow!("unsupported_toolchain").into())
        }
        #[cfg(target_os = "linux")]
        {
            self.realize_linux(attempt_id, root, Some(control))
        }
    }
}

#[cfg(target_os = "linux")]
struct OciArtifact {
    provenance: serde_json::Value,
    archive: PathBuf,
    image_reference: String,
}

#[cfg(target_os = "linux")]
impl SourceOciRealizer<'_> {
    fn prepare_image(
        &self,
        image: &str,
        root: &Path,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<OciArtifact, RealizeFailure> {
        use std::io::{Read, Write};
        let config = self.configured.context("OCI sandbox missing")?;
        let binding = config
            .base_images
            .iter()
            .find(|b| {
                let pinned = if b.reference.contains('@') {
                    b.reference.clone()
                } else {
                    format!("{}@{}", b.reference, b.pinned_digest)
                };
                pinned == image
            })
            .context("immutable OCI image binding missing")?;
        let archive = root.join("bound-image.tar");
        let mut source =
            std::fs::File::open(&binding.archive).context("read bound image archive")?;
        let mut destination = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive)
            .context("create frozen image archive")?;
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            if let Some(c) = control {
                c.remaining(AttemptPhase::Source)?;
            }
            let n = source.read(&mut buffer).context("read bound image chunk")?;
            if n == 0 {
                break;
            }
            bytes = bytes.saturating_add(n as u64);
            if bytes > self.stored_limit || bytes > source_oci::MAX_ARCHIVE_BYTES {
                return Err(anyhow::anyhow!("search_stored_budget_exceeded").into());
            }
            destination
                .write_all(&buffer[..n])
                .context("write frozen image chunk")?;
        }
        destination
            .sync_all()
            .context("sync frozen image archive")?;
        let mut frozen = binding.clone();
        frozen.archive = archive.clone();
        let image_control = control
            .map(|c| {
                Ok::<_, anyhow::Error>(
                    ato_adapter_oci::command_control::CommandControl::new(c.deadline_ms())
                        .cap(c.remaining(AttemptPhase::Source)?),
                )
            })
            .transpose()?;
        let facts = source_oci::verify_bound_image_controlled(
            &frozen,
            &config.maximum_recipe.platform,
            image_control.as_ref(),
        )
        .map_err(|e| controlled_failure(e, control, AttemptPhase::Source))?;
        if let Some(c) = control {
            c.remaining(AttemptPhase::Source)?;
        }
        Ok(OciArtifact {
            archive,
            image_reference: image.into(),
            provenance: serde_json::json!({
                "schema":"ato.oci-image-acquisition/1","image":image,"platform":config.maximum_recipe.platform,
                "archive_sha256":binding.archive_sha256,"config_digest":facts.config_digest,"platform_manifest_digest":facts.manifest_digest,"layer_digests":facts.layer_digests,
                "acquisition":{"network":"none; prebound verified archive","authority":"explicit operator archive binding"},
                "build":"not required for existing image","outputs":{"archive_bytes":bytes,"archive_sha256":binding.archive_sha256}
            }),
        })
    }
    fn realize_linux(
        &self,
        attempt_id: &str,
        root: &Path,
        control: Option<&ExecutionControl>,
    ) -> std::result::Result<Realized, RealizeFailure> {
        use ato_adapter_oci::{
            DockerOciAdapter, OciEndpoint, OciMount, OciResourceLimits, OciSpec, StopBudget,
        };
        use ato_runtime_attempt::{
            launch::oci::{LaunchedOci, OciCandidate},
            realize::RunningCandidate,
        };
        use source_oci::session::{DockerCliBuilder, PrivateDockerSession, SessionEgress};
        use std::collections::BTreeMap;
        use std::io::{Read, Seek, Write};
        let mut source_phase = control.map(|c| c.phase(AttemptPhase::Source)).transpose()?;
        let d = &self.planned.derivation;
        let config = self.configured.context("source OCI sandbox missing")?;
        let recipe = d.source_oci.as_ref().unwrap_or(&config.maximum_recipe);
        let built = if let Some(image) = d.runtimes.get("oci.image") {
            self.prepare_image(image, root, control)?
        } else {
            let hostnames: std::collections::BTreeSet<_> = d
                .requirements
                .network
                .iter()
                .map(|n| n.host.clone())
                .collect();
            let egress = (!hostnames.is_empty()).then(|| source_oci::BuildEgress {
                hosts: hostnames.into_iter().collect(),
                ports: vec![443],
                max_transfer_bytes: self.ticket.network_transfer_bytes,
            });
            let mut source_archive = self
                .archive
                .try_clone()
                .context("clone verified source archive")?;
            source_archive
                .rewind()
                .context("rewind verified source archive")?;
            let archive_path = root.join("source-oci-input.tar");
            let mut frozen_file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&archive_path)
                .context("create frozen builder input")?;
            let mut buffer = [0u8; 65536];
            loop {
                if let Some(control) = control {
                    control.remaining(AttemptPhase::Source)?;
                }
                let n = source_archive
                    .read(&mut buffer)
                    .context("read frozen builder input")?;
                if n == 0 {
                    break;
                }
                frozen_file
                    .write_all(&buffer[..n])
                    .context("copy frozen builder input")?;
            }
            frozen_file.sync_all().context("sync builder input")?;
            let request = source_oci::SourceOciRequest {
                schema: source_oci::SOURCE_OCI_REQUEST_SCHEMA.into(),
                title: "Formation exploration".into(),
                source_archive: archive_path,
                source_archive_sha256: self.archive_digest.into(),
                dockerfile: recipe.dockerfile.clone(),
                platform: recipe.platform.clone(),
                base_images: recipe
                    .base_images
                    .iter()
                    .map(|b| {
                        config
                            .base_images
                            .iter()
                            .find(|i| {
                                i.reference == b.reference && i.pinned_digest == b.pinned_digest
                            })
                            .cloned()
                            .context("frozen base binding missing")
                    })
                    .collect::<Result<_>>()?,
                declared_transport_port: d.ports[0].guest_port.context("OCI guest port missing")?,
                authorized_state: d
                    .state
                    .iter()
                    .map(|s| source_oci::AuthorizedState {
                        id: s.id.clone(),
                        mount: s.mount.clone(),
                    })
                    .collect(),
                policy: source_oci::SourceOciPolicy {
                    network: if egress.is_some() {
                        "egress_allowlist"
                    } else {
                        "none"
                    }
                    .into(),
                    egress: egress.clone(),
                    build_timeout_seconds: recipe.build_timeout_seconds,
                    max_archive_bytes: recipe.max_archive_bytes,
                    build: source_oci::BuildLimits {
                        memory_bytes: recipe.build.memory_bytes,
                        cpu_limit_millis: recipe.build.cpu_limit_millis,
                        pids_limit: recipe.build.pids_limit,
                        disk_bytes: recipe.build_disk_bytes,
                    },
                    runtime: source_oci::RuntimeLimits {
                        memory_bytes: recipe.runtime.memory_bytes,
                        cpu_limit_millis: recipe.runtime.cpu_limit_millis,
                        pids_limit: recipe.runtime.pids_limit,
                    },
                },
            };
            let command_control = control
                .map(|c| {
                    Ok::<_, anyhow::Error>(
                        ato_adapter_oci::command_control::CommandControl::new(c.deadline_ms())
                            .cap(c.remaining(AttemptPhase::Source)?),
                    )
                })
                .transpose()?;
            source_oci::verify_dockerfile_selection(self.source_root, &request.dockerfile)
                .map_err(|e| controlled_failure(e, control, AttemptPhase::Source))?;
            let prepared = source_oci::prepare_controlled(&request, command_control.clone())
                .map_err(|e| controlled_failure(e, control, AttemptPhase::Source))?;
            drop(source_phase.take());
            let build_phase = control.map(|c| c.phase(AttemptPhase::Build)).transpose()?;
            // The same source closure is consumed by both the planner and builder.
            if prepared.source_closure_ref() != d.inputs[0].content_ref {
                return Err(anyhow::anyhow!("source_oci_source_mismatch").into());
            }
            let suffix = super::digest(attempt_id.as_bytes());
            std::fs::create_dir_all(&config.builder_root).context("create private builder root")?;
            let builder_path = config.builder_root.join(&suffix[7..23]);
            let session = PrivateDockerSession::start_controlled(
                &builder_path,
                &request.policy.build,
                config.tools.clone().unwrap_or_default(),
                egress.map(|e| SessionEgress {
                    hosts: e.hosts,
                    ports: e.ports,
                    max_transfer_bytes: e.max_transfer_bytes,
                }),
                command_control,
            )
            .map_err(|e| controlled_failure(e, control, AttemptPhase::Build))?;
            let built = source_oci::materialize(&prepared, &DockerCliBuilder::new(session), &{
                std::fs::create_dir_all(self.artifact_root.join("source-oci-evidence"))
                    .context("create evidence root")?;
                self.artifact_root
                    .join("source-oci-evidence")
                    .join(&suffix[7..])
            })
            .map_err(|e| controlled_failure(e, control, AttemptPhase::Build))?;
            drop(build_phase);
            OciArtifact {
                provenance: built.provenance,
                archive: built.archive,
                image_reference: built.image_reference,
            }
        };
        drop(source_phase);
        let launch_phase = control.map(|c| c.phase(AttemptPhase::Launch)).transpose()?;
        *self.artifact.borrow_mut() = Some(
            serde_json::json!({"kind":"exploration_source_oci_evidence","provenance":built.provenance, "stored_bytes":built.provenance["outputs"]["archive_bytes"].as_u64().unwrap_or(0)+serde_json::to_vec(&built.provenance).context("encode build provenance")?.len() as u64}),
        );
        if self
            .artifact
            .borrow()
            .as_ref()
            .and_then(|v| v["stored_bytes"].as_u64())
            .is_none_or(|n| n > self.stored_limit)
        {
            return Err(anyhow::anyhow!("search_stored_budget_exceeded").into());
        }
        let launch_control = control
            .map(|c| {
                Ok::<_, anyhow::Error>(
                    ato_adapter_oci::command_control::CommandControl::new(c.deadline_ms())
                        .cap(c.remaining(AttemptPhase::Launch)?),
                )
            })
            .transpose()?;
        let facts = if d.runtimes.contains_key("oci.image") {
            let binding = config
                .base_images
                .iter()
                .find(|b| {
                    if b.reference.contains('@') {
                        b.reference == built.image_reference
                    } else {
                        format!("{}@{}", b.reference, b.pinned_digest) == built.image_reference
                    }
                })
                .context("image binding missing")?;
            let mut frozen = binding.clone();
            frozen.archive = built.archive.clone();
            source_oci::verify_bound_image_controlled(
                &frozen,
                &recipe.platform,
                launch_control.as_ref(),
            )
        } else {
            source_oci::image_facts_controlled(&built.archive, launch_control.as_ref())
        }
        .map_err(|e| controlled_failure(e, control, AttemptPhase::Launch))?;
        let host_port = std::net::TcpListener::bind("127.0.0.1:0")
            .context("allocate OCI ingress")?
            .local_addr()
            .context("resolve OCI ingress")?
            .port();
        let scratch = root.join("oci-runtime");
        std::fs::create_dir_all(&scratch).context("create isolated OCI runtime")?;
        let mounts = d
            .state
            .iter()
            .map(|s| {
                let path = scratch.join("isolated-state").join(&s.id);
                std::fs::create_dir_all(&path)?;
                Ok(OciMount {
                    host_path: path,
                    guest_path: s.mount.clone(),
                    writable: true,
                })
            })
            .collect::<Result<_>>()?;
        let adapter = DockerOciAdapter::new_offline_at(
            OciSpec {
                id: attempt_id.into(),
                image: built.image_reference.clone(),
                platform: recipe.platform.clone(),
                entrypoint: None,
                argv: facts.cmd,
                working_dir: if facts.working_dir.is_empty() {
                    "/app".into()
                } else {
                    facts.working_dir
                },
                workspace_mount_path: source_oci::SOURCE_OCI_WORKSPACE_MOUNT.into(),
                environment: BTreeMap::new(),
                endpoints: vec![OciEndpoint {
                    host_port,
                    guest_port: d.ports[0].guest_port.context("OCI guest port missing")?,
                }],
                mounts,
                limits: OciResourceLimits {
                    memory_bytes: recipe.runtime.memory_bytes,
                    cpu_limit_millis: recipe.runtime.cpu_limit_millis,
                    pids_limit: recipe.runtime.pids_limit,
                },
                stop_timeout_seconds: 3,
                labels: BTreeMap::new(),
            },
            std::fs::read(&built.archive).context("read verified built image")?,
            facts.config_digest,
            config.tools.clone().unwrap_or_default().docker,
        )?
        .with_isolated_owner(&config.runtime_socket, &scratch.join("owner-config"))?;
        let adapter = match control {
            Some(c) => adapter.with_control(
                ato_adapter_oci::command_control::CommandControl::new(c.deadline_ms())
                    .cap(c.remaining(AttemptPhase::Launch)?),
            ),
            None => adapter,
        };
        let handle = adapter.spawn(self.source_root, &scratch).map_err(|e| {
            if let Some(u) = e.downcast_ref::<ato_adapter_oci::SpawnCleanupUnconfirmed>() {
                RealizeFailure::Abandoned {
                    error: anyhow::anyhow!("source OCI launch failed"),
                    cleanup: u.reason.clone(),
                    resources: vec![format!("container:{}", u.container)],
                }
            } else {
                RealizeFailure::Launch {
                    error: e,
                    evidence: None,
                }
            }
        })?;
        let container_id = handle.container_id().to_owned();
        let mut candidate = Box::new(OciCandidate::new(
            LaunchedOci::Container(handle),
            BTreeMap::from([(
                d.ports[0].id.clone(),
                format!("http://127.0.0.1:{host_port}"),
            )]),
            vec![scratch],
            StopBudget::DEFAULT,
        ));
        let budget = match control {
            Some(c) => match c.cap(AttemptPhase::Launch, std::time::Duration::from_secs(10)) {
                Ok(d) => d,
                Err(e) => return stopped_launch(candidate, e),
            },
            None => std::time::Duration::from_secs(10),
        };
        let deadline = std::time::Instant::now() + budget;
        loop {
            if let Some(c) = control
                && let Err(e) = c.remaining(AttemptPhase::Launch)
            {
                return stopped_launch(candidate, e);
            }
            let failed = match candidate.exited() {
                Ok(exit) => exit.map(|reason| anyhow::anyhow!(reason)),
                Err(error) => return stopped_launch(candidate, error),
            };
            if let Some(error) = failed {
                return stopped_launch(candidate, error);
            }
            if std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{host_port}")
                    .parse()
                    .context("parse OCI endpoint")?,
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(100)),
            )
            .is_ok()
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                return stopped_launch(candidate, anyhow::anyhow!("source_oci_readiness_timeout"));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        drop(launch_phase);
        let execution = ato_formation::verify::VerificationExecutionEvidence {
            realization: "oci".into(),
            runtime_executable: Some("docker".into()),
            runtime_version: None,
            pid: None,
            container_id: Some(container_id),
            image: Some(built.image_reference),
            platform: Some(recipe.platform.clone()),
            endpoint: None,
            run_id: Some(attempt_id.into()),
            lease_id: None,
            attempt_id: None,
            request_id: None,
            dependency_fetches: vec![],
            portability_profile: None,
            embedded_oci_image_loaded: Some(
                built.provenance["outputs"]["archive_sha256"]
                    .as_str()
                    .context("built archive digest missing")?
                    .into(),
            ),
            services: vec![],
        };
        Ok(Realized {
            candidate,
            execution,
            evidence: Some(ato_formation::request::RealizationEvidence {
                executor: "source-oci/runtime-oci".into(),
                containment:
                    "private bounded builder; internal container network, dropped capabilities"
                        .into(),
                workspace: "verified source read-only; empty isolated state".into(),
                build_network: "phase-scoped".into(),
                candidate_network: "internal network, no egress".into(),
                endpoints: BTreeMap::new(),
                destroyed: false,
            }),
            kept: None,
        })
    }
}

#[cfg(target_os = "linux")]
fn controlled_failure(
    e: source_oci::SourceOciError,
    control: Option<&ExecutionControl>,
    phase: AttemptPhase,
) -> RealizeFailure {
    if e.cleanup.is_some() || e.code == "source_oci_cleanup_unconfirmed" {
        return source_oci_failure(e);
    }
    if let Some(c) = control
        && let Err(deadline) = c.remaining(phase)
    {
        return deadline.into();
    }
    source_oci_failure(e)
}

#[cfg(target_os = "linux")]
fn source_oci_failure(e: source_oci::SourceOciError) -> RealizeFailure {
    if e.cleanup.is_some() || e.code == "source_oci_cleanup_unconfirmed" {
        RealizeFailure::Abandoned {
            cleanup: e.to_string(),
            error: anyhow::anyhow!(e),
            resources: vec!["source-oci-builder".into()],
        }
    } else {
        RealizeFailure::Execution(anyhow::anyhow!(e))
    }
}

#[cfg(target_os = "linux")]
fn stopped_launch(
    candidate: Box<dyn ato_runtime_attempt::realize::RunningCandidate>,
    error: anyhow::Error,
) -> std::result::Result<Realized, RealizeFailure> {
    match candidate.stop() {
        Ok(()) => Err(RealizeFailure::Launch {
            error,
            evidence: None,
        }),
        Err(cleanup) => Err(RealizeFailure::Abandoned {
            error,
            cleanup: cleanup.to_string(),
            resources: vec!["source-oci-runtime".into()],
        }),
    }
}
