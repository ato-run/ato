//! Collecting the measurements a v1 Execution Contract is minted from.
//!
//! [`ExecutionObservationV1`] is deliberately measured-only: it has no
//! constructor that takes a contract, so nothing here can copy an expected value
//! in. Every facet below is either read off the concrete build or assembled from
//! the v1 manifest the author wrote — and the ones that are neither are the ones
//! the Step-4 subset refuses, so they are genuinely empty rather than skipped.
//!
//! The measuring lives here, in the application layer, because the pure contract
//! layer performs no host I/O by design.
//!
//! # `launch.cwd` is resolved, not authored
//!
//! ADR-015 §4.1 lists it as `A→R`. For the recipe lane that is wrong in a way
//! worth stating: the v1 manifest has no working-directory field, and it should
//! not — the working directory is where the BUILD placed the source, which the
//! builder decides. Requiring the author to restate it would invite a manifest
//! that says `/srv` for a build that puts the source at `/app`, and the manifest
//! would be the thing that was wrong. So it is supplied by the build.

use crate::docker_import::ResolvedRuntimeArtifact;
use crate::rootfs_builder::ObservedInvocationPrefix;
use anyhow::{Context, Result};
use capsule::execution_contract::{
    ContentDigest, ExternalStateContract, GuestPath, GuestSurfaceContract, ResolvedTargetContract,
};
use capsule::execution_contract_finalize::{ExecutionObservationV1, MeasuredEnvValue};
use capsule::execution_payloads::{
    CapabilityPolicyPayloadV1, EnvironmentPolicyPayloadV1, FilesystemPolicyPayloadV1,
    FilesystemTopologyPayloadV1, NetworkPolicyPayloadV1, ProcessModelPayloadV1,
    RuntimeDynamicPayloadV1, SourceProjectionPayloadV1,
};
use capsule::types::manifest_v1::{CapsuleManifestV1, ConfigKindV1};

/// The Ato surface-contract profile a `[web]` capsule serves.
///
/// Resolved from the kind of surface declared, never authored (ADR-015 §6.1):
/// it names Ato's own contract, not an application-layer choice the author
/// makes.
///
pub const WEB_SURFACE_PROTOCOL: &str = "ato.web-surface.v1";
pub const TERMINAL_SURFACE_PROTOCOL: &str = "ato.terminal-surface.v1";

/// Everything measured about one concrete build.
///
/// Completeness is the type's, not a caller's: every field is required, so an
/// observation that exists at all carries every input `observe_v1` needs. There
/// is no partially-filled state to count facets in and no `Option` to forget —
/// a producer that has not measured something cannot construct this.
pub struct V1BuildObservation<'a> {
    pub manifest: &'a CapsuleManifestV1,
    /// A1v2 tree hash of the PROJECTED source (control files already withheld).
    pub source_digest: ContentDigest,
    /// Which control files the projection withheld, as repository-relative
    /// paths. Identity-bearing: a repo carrying `ato.lock.json` had a different
    /// file held out than one carrying `capsule.lock`.
    pub excluded_control_files: Vec<String>,
    /// The resolved runtime family, e.g. `python`, `node`, `static-web`. A
    /// property of the BUILD, not of the artifact: the same base image can back
    /// more than one family.
    pub runtime_kind: String,
    /// The base image, resolved to an immutable digest.
    pub runtime: &'a ResolvedRuntimeArtifact,
    /// argv the runtime prepends to the authored command — including the
    /// measured fact that it prepends nothing, which is not the same as nobody
    /// having looked.
    pub runtime_invocation_prefix: ObservedInvocationPrefix,
    /// What the guest filesystem CONTAINS, as one digest.
    ///
    /// Content rather than the ext4 serialization of it, and the difference is
    /// measured: `mke2fs` stamps every inode it creates with the wall clock and
    /// ignores `SOURCE_DATE_EPOCH`, so hashing the packed image made a rebuild
    /// of one program source a different execution. See
    /// `crate::guest_filesystem_digest` for what is committed and what is
    /// deliberately not.
    pub filesystem_view_digest: ContentDigest,
    pub target: ResolvedTargetContract,
    /// The complete argv the runner starts — the authored command after
    /// resolution, which may legitimately carry a runtime prefix (ADR-015 §3).
    pub resolved_argv: Vec<String>,
    /// Where the build placed the source in the guest.
    pub working_directory: String,
}

/// The writable boundary the sealed image actually has.
///
/// These are the tmpfs mounts the generated init creates, and nothing else: the
/// root image is mounted read-only. Listing them is not a duplicate of the
/// topology facet — topology commits WHAT IS MOUNTED, this commits WHERE THE
/// WORKLOAD MAY WRITE, and the two answer different questions even when they
/// happen to name the same paths.
const SEALED_WRITABLE_PATHS: &[&str] = &["/run", "/tmp", "/var/tmp"];

/// Assemble the observation. Every facet is set — `into_contract` requires all
/// of them, and a facet left unset would refuse the mint rather than default.
pub fn observe_v1(build: V1BuildObservation<'_>) -> Result<ExecutionObservationV1> {
    // The subset gate has already run. Tool pins select the measured primary
    // runtime and authored build commands mutate the exported rootfs committed
    // by filesystem.view_digest; neither creates a separate dependency or
    // named build-output artifact. External state is still refused.
    build
        .manifest
        .validate_for_interactive_capture()
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    let rootfs_digest = build.filesystem_view_digest;

    let mut environment: Vec<MeasuredEnvValue> = build
        .manifest
        .env
        .iter()
        .map(|(name, value)| MeasuredEnvValue {
            name: name.clone(),
            value_payload: capsule::execution_contract::EnvironmentValuePayloadV1::utf8(
                value.clone(),
            ),
        })
        .collect();
    environment.sort_by(|a, b| a.name.cmp(&b.name));

    let mut secret_bindings: Vec<String> = build
        .manifest
        .config
        .iter()
        .filter(|(_, field)| field.kind == ConfigKindV1::Secret)
        .map(|(name, _)| name.clone())
        .collect();
    secret_bindings.sort();

    // Required vs optional is what the author DECLARED, not what happens to be
    // set anywhere — the whole point of making both explicit in v1.
    let names_where = |required: bool| -> Vec<String> {
        build
            .manifest
            .config
            .iter()
            .filter(|(_, field)| field.required == required)
            .map(|(name, _)| name.clone())
            .collect()
    };
    let required = names_where(true);
    let optional = names_where(false);

    let guest_surface = match (build.manifest.web.as_ref(), build.manifest.surface.as_ref()) {
        (Some(_), Some(surface))
            if surface.kind == ato_ipc::session_surface::SessionSurfaceKind::Terminal =>
        {
            Ok(GuestSurfaceContract {
                bind_address: "vsock".to_string(),
                protocol: TERMINAL_SURFACE_PROTOCOL.to_string(),
                port: None,
                features: Vec::new(),
            })
        }
        (Some(web), None) => Ok(GuestSurfaceContract {
            bind_address: web.bind.clone(),
            protocol: WEB_SURFACE_PROTOCOL.to_string(),
            port: Some(
                std::num::NonZeroU16::new(web.port)
                    .context("web.port is validated non-zero by the manifest")?,
            ),
            features: Vec::new(),
        }),
        _ => Err(anyhow::anyhow!(
            "the Step-4 subset requires a supported guest surface"
        )),
    }?;

    let writable_paths = SEALED_WRITABLE_PATHS
        .iter()
        .map(|path| GuestPath::parse(path).map_err(|error| anyhow::anyhow!("{path}: {error}")))
        .collect::<Result<Vec<_>>>()?;

    let observation = ExecutionObservationV1::new()
        .measured_source_digest(build.source_digest)
        .measured_source_projection(serde_json::to_value(SourceProjectionPayloadV1::a1v2(
            build.excluded_control_files,
        ))?)
        .measured_target(build.target)
        .measured_runtime(build.runtime_kind.clone(), build.runtime.digest)
        .measured_runtime_dynamic(serde_json::to_value(RuntimeDynamicPayloadV1::new(
            build.runtime_kind,
            build.runtime_invocation_prefix.into_words(),
        ))?)
        .measured_dependencies(Vec::new())
        .measured_build_outputs(Vec::new())
        .measured_launch(
            build.resolved_argv,
            GuestPath::parse(&build.working_directory)
                .map_err(|error| anyhow::anyhow!("{}: {error}", build.working_directory))?,
        )
        .measured_process_model(serde_json::to_value(
            ProcessModelPayloadV1::single_process(),
        )?)
        .measured_environment(environment)
        .measured_environment_policy(serde_json::to_value(
            EnvironmentPolicyPayloadV1::sealed_guest(required, optional),
        )?)
        .measured_secret_bindings(secret_bindings)
        .measured_filesystem_view(rootfs_digest)
        .measured_filesystem_topology(serde_json::to_value(
            FilesystemTopologyPayloadV1::sealed_no_volumes(),
        )?)
        // One layer: the builder packs a single ext4 image, so the composed view
        // and the only read-only layer are the same bytes. Saying so is not a
        // duplication — it is what this build genuinely is.
        .measured_readonly_layers(vec![rootfs_digest])
        .measured_writable_paths(writable_paths)
        .measured_policy(
            serde_json::to_value(NetworkPolicyPayloadV1::new(
                Vec::new(),
                vec![
                    build
                        .manifest
                        .web
                        .as_ref()
                        .map(|web| web.port)
                        .unwrap_or_default(),
                ],
            ))?,
            // Nothing declared. The v1 manifest has no capability surface yet,
            // and the security schema's own rule is that absence must not be
            // read as a level — so this says "not declared" rather than "none".
            serde_json::to_value(CapabilityPolicyPayloadV1::undeclared())?,
            serde_json::to_value(FilesystemPolicyPayloadV1::new(Vec::new(), Vec::new()))?,
        )
        .measured_guest_surface(guest_surface)
        .measured_external_state(Vec::<ExternalStateContract>::new());

    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::execution_contract::{DigestAlgorithm, ExecutionContractV1};

    fn manifest(extra: &str) -> CapsuleManifestV1 {
        CapsuleManifestV1::from_toml(&format!(
            r#"
schema_version = "1"
name = "demo"
version = "0.1.0"

[run]
command = ["python", "-m", "app"]

[web]
port = 8080
bind = "0.0.0.0"

[seal_at]
command = ["true"]
{extra}
"#
        ))
        .expect("v1 manifest")
    }

    fn runtime() -> ResolvedRuntimeArtifact {
        ResolvedRuntimeArtifact {
            original_ref: "python:3.11-slim".into(),
            resolved_ref: format!("docker.io/library/python@sha256:{}", "c".repeat(64)),
            digest: ContentDigest::try_from(format!("sha256:{}", "c".repeat(64))).unwrap(),
        }
    }

    fn target() -> ResolvedTargetContract {
        ResolvedTargetContract {
            os: "linux".into(),
            architecture: "x86_64".into(),
            abi: "gnu".into(),
            libc: Some("glibc-2.36".into()),
            observable_features: Default::default(),
        }
    }

    /// Stand-in for `crate::guest_filesystem_digest`, which takes the same
    /// kind of value over the exported rootfs TREE.
    fn view_digest(bytes: &[u8]) -> ContentDigest {
        ContentDigest::new(DigestAlgorithm::Blake3, *blake3::hash(bytes).as_bytes())
    }

    fn observe(manifest: &CapsuleManifestV1, rootfs: &[u8]) -> ExecutionObservationV1 {
        let runtime = runtime();
        observe_v1(V1BuildObservation {
            manifest,
            source_digest: ContentDigest::new(DigestAlgorithm::Blake3, [7u8; 32]),
            excluded_control_files: vec!["capsule.lock".into(), "capsule.toml".into()],
            runtime_kind: "python".into(),
            runtime: &runtime,
            runtime_invocation_prefix: ObservedInvocationPrefix::observed_none(),
            filesystem_view_digest: view_digest(rootfs),
            target: target(),
            resolved_argv: vec!["python".into(), "-m".into(), "app".into()],
            working_directory: "/app".into(),
        })
        .expect("observation")
    }

    fn mint(manifest: &CapsuleManifestV1, rootfs: &[u8]) -> ExecutionContractV1 {
        observe(manifest, rootfs)
            .into_contract()
            .expect("every facet is measured")
    }

    /// The whole point: a real build produces a COMPLETE observation, so the
    /// mint succeeds instead of refusing on an unmeasured facet.
    ///
    /// Until this landed, every facet but three had no producer and
    /// `into_contract` could only ever refuse.
    #[test]
    fn a_real_build_measures_every_facet_and_mints() {
        let contract = mint(&manifest(""), b"rootfs-bytes");
        contract.validate().expect("the minted contract is valid");
        contract
            .compute_execution_id()
            .expect("and it has an identity");
    }

    /// The subset's empty collections are genuinely empty — the manifest
    /// declares none, and one that did would have been refused before this ran.
    #[test]
    fn the_subset_facets_are_empty_because_the_capsule_declares_none() {
        let contract = mint(&manifest(""), b"rootfs");
        assert!(contract.dependencies.is_empty());
        assert!(contract.build_outputs.is_empty());
        assert!(contract.external_state.is_empty());
        assert!(contract.guest_surface.features.is_empty());
    }

    /// Changing the ROOTFS changes the identity, because the composed view and
    /// the read-only layer are both measured from it.
    #[test]
    fn different_image_bytes_are_a_different_execution() {
        let manifest = manifest("");
        assert_ne!(
            mint(&manifest, b"one").compute_execution_id().unwrap(),
            mint(&manifest, b"two").compute_execution_id().unwrap()
        );
    }

    /// The runtime artifact is identity-bearing: the same source on a different
    /// base image is a different execution, which is exactly what resolving the
    /// tag to a digest buys.
    #[test]
    fn a_different_resolved_runtime_is_a_different_execution() {
        let manifest = manifest("");
        let baseline = mint(&manifest, b"rootfs").compute_execution_id().unwrap();

        let other = ResolvedRuntimeArtifact {
            original_ref: "python:3.11-slim".into(),
            // The SAME tag, resolved to different bytes — the case a tag cannot
            // distinguish and a digest can.
            resolved_ref: format!("docker.io/library/python@sha256:{}", "d".repeat(64)),
            digest: ContentDigest::try_from(format!("sha256:{}", "d".repeat(64))).unwrap(),
        };
        let moved = observe_v1(V1BuildObservation {
            manifest: &manifest,
            source_digest: ContentDigest::new(DigestAlgorithm::Blake3, [7u8; 32]),
            excluded_control_files: vec!["capsule.lock".into(), "capsule.toml".into()],
            runtime_kind: "python".into(),
            runtime: &other,
            runtime_invocation_prefix: ObservedInvocationPrefix::observed_none(),
            filesystem_view_digest: view_digest(b"rootfs"),
            target: target(),
            resolved_argv: vec!["python".into(), "-m".into(), "app".into()],
            working_directory: "/app".into(),
        })
        .unwrap()
        .into_contract()
        .unwrap()
        .compute_execution_id()
        .unwrap();

        assert_ne!(baseline, moved, "the resolved artifact is identity-bearing");
    }

    /// An authored non-secret value is committed; a secret is bound by NAME and
    /// its value never enters the contract.
    #[test]
    fn env_values_are_committed_and_secrets_are_bound_by_name_only() {
        let contract = mint(
            &manifest(
                r#"
[env]
NODE_ENV = "production"

[config.API_KEY]
kind = "secret"
required = true
"#,
            ),
            b"rootfs",
        );
        assert_eq!(contract.launch.environment.len(), 1);
        assert_eq!(contract.launch.environment[0].name, "NODE_ENV");
        assert_eq!(contract.launch.secret_bindings, ["API_KEY"]);

        // The secret's VALUE appears nowhere in the canonical bytes — there was
        // never a value to leak, which is the property, not a filter that ran.
        let canonical = String::from_utf8(contract.canonical_bytes().unwrap()).unwrap();
        assert!(canonical.contains("API_KEY"), "the NAME is bound");
        assert!(!canonical.contains("production") || canonical.contains("value_digest"));
    }

    /// Changing an authored env VALUE moves the identity, even though only its
    /// digest is stored.
    #[test]
    fn changing_an_env_value_changes_the_execution_id() {
        let one = mint(&manifest("\n[env]\nNODE_ENV = \"production\"\n"), b"r");
        let two = mint(&manifest("\n[env]\nNODE_ENV = \"staging\"\n"), b"r");
        assert_ne!(
            one.compute_execution_id().unwrap(),
            two.compute_execution_id().unwrap()
        );
    }

    /// The surface protocol is RESOLVED from the kind of surface, not authored
    /// — it names Ato's own contract rather than an application-layer choice.
    #[test]
    fn the_web_surface_resolves_its_protocol() {
        let contract = mint(&manifest(""), b"rootfs");
        assert_eq!(contract.guest_surface.protocol, WEB_SURFACE_PROTOCOL);
        assert_eq!(contract.guest_surface.bind_address, "0.0.0.0");
        assert_eq!(contract.guest_surface.port.unwrap().get(), 8080);
    }

    /// The writable boundary is what the sealed image actually has: the three
    /// tmpfs mounts, with the root image read-only.
    #[test]
    fn the_writable_boundary_is_the_tmpfs_set() {
        let contract = mint(&manifest(""), b"rootfs");
        let paths: Vec<String> = contract
            .filesystem
            .writable_paths
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(paths, ["/run", "/tmp", "/var/tmp"]);
    }

    /// A capsule outside the subset never reaches a measurement — the refusal
    /// comes from the manifest gate, before anything is observed.
    #[test]
    fn a_capsule_outside_the_subset_is_refused_before_measuring() {
        let error = observe_v1(V1BuildObservation {
            manifest: &manifest(
                "\n[state.data]\nmount = \"/data\"\naccess = \"read-write\"\nschema = \"v1\"\nsnapshot = \"exclude\"\n",
            ),
            source_digest: ContentDigest::new(DigestAlgorithm::Blake3, [7u8; 32]),
            excluded_control_files: vec!["capsule.toml".into()],
            runtime_kind: "python".into(),
            runtime: &runtime(),
            runtime_invocation_prefix: ObservedInvocationPrefix::observed_none(),
            filesystem_view_digest: view_digest(b"rootfs"),
            target: target(),
            resolved_argv: vec!["python".into()],
            working_directory: "/app".into(),
        })
        .expect_err("outside the subset");
        assert!(format!("{error:#}").contains("[state.<name>]"), "{error:#}");
    }
}
