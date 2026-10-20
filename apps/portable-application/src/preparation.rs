//! Explicit local Formation constraints and agent-produced candidate admission.
//! These are orchestration inputs; they never change a frozen Contract.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use ato_objects::PortableApplicationBundle;
use serde::{Deserialize, Serialize};

use crate::{
    PYTHON_RUNTIME, PortableApplicationError, PortableAuthoredBundleSpec, PortableDynamicRouteSpec,
    PortableExecutionSpec, PortableFilesystemStateSpec, PortableHttpRequirementSpec,
    PortableRealizationKind, ValidatedPortableApplication, build_dynamic_routes_bundle, profile,
    validate_all_derivations,
};

pub const NATIVE_OS: &str = "native.os";
pub const NATIVE_ARCH: &str = "native.arch";
pub const NATIVE_EXECUTABLE: &str = "native.executable";
pub const NATIVE_ENCODING: &str = "native.encoding";
const MAX_NATIVE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PreparationConstraints {
    pub no_vm: bool,
    pub offline_after_save: bool,
}

#[derive(Debug, Serialize)]
pub struct PreparationReport {
    pub schema: &'static str,
    pub status: &'static str,
    pub contract_ref: String,
    pub selected_derivation_ref: Option<String>,
    pub eligible_derivations: Vec<String>,
    pub constraints: PreparationConstraints,
    pub target_os: String,
    pub target_arch: String,
}

/// An explicit process proposal. The producer supplies immutable files and
/// argv, never a shell command for the receiver to execute while forming it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProcessProposal {
    pub schema: String,
    pub executable: String,
    #[serde(default = "identity_encoding")]
    pub encoding: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub target_os: String,
    pub target_arch: String,
}

/// Reuse only declared D; proposals must retain the exact frozen K and the
/// logical Surface/state boundary. VM/remote fallbacks are not authorized.
pub fn assess_preparation(
    original: &PortableApplicationBundle,
    candidate: Option<&PortableApplicationBundle>,
    constraints: PreparationConstraints,
    selected: Option<&str>,
) -> Result<PreparationReport, PortableApplicationError> {
    if !constraints.no_vm || !constraints.offline_after_save {
        return Err(profile(
            "this preparation profile requires no_vm and offline_after_save",
        ));
    }
    let original_routes = validate_all_derivations(original)?;
    let original_route = original_routes
        .first()
        .ok_or_else(|| profile("no original route"))?;
    let routes = validate_all_derivations(candidate.unwrap_or(original))?;
    if routes.iter().any(|route| {
        route.contract_ref != original_route.contract_ref
            || route.application.surfaces != original_route.application.surfaces
            || !original_routes
                .iter()
                .any(|base| base.derivation.state == route.derivation.state)
    }) {
        return Err(profile(
            "proposal changed the frozen Contract, Surface or state boundary",
        ));
    }
    let eligible = routes
        .iter()
        .filter(|route| admissible_local_route(candidate.unwrap_or(original), route).is_ok())
        .map(|route| route.derivation_ref.to_string())
        .collect::<Vec<_>>();
    let chosen = match selected {
        Some(value) if eligible.iter().any(|reference| reference == value) => {
            Some(value.to_owned())
        }
        Some(_) => {
            return Err(profile(
                "selected D does not satisfy the preparation constraints",
            ));
        }
        None if eligible.len() == 1 => eligible.first().cloned(),
        None => None,
    };
    Ok(PreparationReport {
        schema: "ato.local-preparation-assessment/1",
        status: if chosen.is_some() {
            "ready"
        } else if eligible.is_empty() {
            "needs_proposal"
        } else {
            "needs_selection"
        },
        contract_ref: original_route.contract_ref.to_string(),
        selected_derivation_ref: chosen,
        eligible_derivations: eligible,
        constraints,
        target_os: std::env::consts::OS.to_owned(),
        target_arch: std::env::consts::ARCH.to_owned(),
    })
}

/// Check runtime policy separately from format/closure validation. Actual
/// execution, dependency materialization and Contract verification still follow.
pub fn admissible_local_route(
    bundle: &PortableApplicationBundle,
    route: &ValidatedPortableApplication,
) -> Result<(), PortableApplicationError> {
    if !route.application.bindings.is_empty()
        || route
            .derivation
            .steps
            .iter()
            .any(|step| !step.bindings.is_empty() || step.network != Default::default())
    {
        return Err(profile(
            "offline preparation does not admit external Bindings or network grants",
        ));
    }
    match route.realization {
        PortableRealizationKind::StaticWeb => Ok(()),
        PortableRealizationKind::LocalProcess
            if route.derivation.runtimes.contains_key(PYTHON_RUNTIME) =>
        {
            if !matches!(
                route.derivation.steps[0].argv[0].as_str(),
                "python" | "python3"
            ) {
                return Err(profile("Python route must name its pinned logical runtime"));
            }
            Ok(())
        }
        PortableRealizationKind::LocalProcess => {
            let executable = native_executable_path(route)?;
            let entry = route
                .tree
                .entries
                .iter()
                .find(|entry| entry.path == executable)
                .ok_or_else(|| profile("native executable is absent from the immutable tree"))?;
            let reference = crate::parse_ref(&entry.content_ref, "native executable")?;
            let bytes = bundle.payload_bytes(&reference)?;
            let image = decode_native_image(
                &bytes,
                route
                    .derivation
                    .runtimes
                    .get(NATIVE_ENCODING)
                    .map(String::as_str)
                    .unwrap_or("identity"),
            )?;
            validate_native_image(&image, std::env::consts::OS, std::env::consts::ARCH)
        }
        _ => Err(profile(
            "VM-free preparation refuses OCI and service-group execution",
        )),
    }
}

/// Resolve only a digest-verified executable in the declared immutable input.
pub fn native_executable_path(
    route: &ValidatedPortableApplication,
) -> Result<&str, PortableApplicationError> {
    let runtimes = &route.derivation.runtimes;
    if runtimes.get(NATIVE_OS).map(String::as_str) != Some(std::env::consts::OS)
        || runtimes.get(NATIVE_ARCH).map(String::as_str) != Some(std::env::consts::ARCH)
    {
        return Err(profile(
            "native executable does not target this OS and architecture",
        ));
    }
    let executable = runtimes
        .get(NATIVE_EXECUTABLE)
        .ok_or_else(|| profile("native executable is undeclared"))?;
    if !crate::valid_workspace_relative(executable)
        || executable == "."
        || route.derivation.steps[0].argv.first() != Some(executable)
        || route.derivation.steps[0].cwd != "."
    {
        return Err(profile(
            "native executable must be an explicit workspace-relative argv[0] with cwd '.'",
        ));
    }
    Ok(executable)
}

fn identity_encoding() -> String {
    "identity".to_owned()
}

/// A bounded transport encoding; expanded bytes are checked before execution.
/// It changes D, never K. No archive paths or extraction commands are accepted.
pub fn decode_native_image(
    bytes: &[u8],
    encoding: &str,
) -> Result<Vec<u8>, PortableApplicationError> {
    let image = match encoding {
        "identity" => bytes.to_vec(),
        "gzip" => {
            let mut image = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .take(MAX_NATIVE_BYTES + 1)
                .read_to_end(&mut image)
                .map_err(|error| profile(format!("invalid native gzip: {error}")))?;
            image
        }
        _ => return Err(profile("unsupported native executable encoding")),
    };
    if image.len() as u64 > MAX_NATIVE_BYTES {
        return Err(profile("expanded native executable exceeds limit"));
    }
    Ok(image)
}

fn validate_native_image(
    bytes: &[u8],
    os: &str,
    arch: &str,
) -> Result<(), PortableApplicationError> {
    // The first supported native target is the user's Apple Silicon Mac.
    // Refuse scripts, wrappers, foreign binaries and unknown targets.
    if os != "macos"
        || arch != "aarch64"
        || bytes.len() < 32
        || bytes.get(12..16) != Some(&2u32.to_le_bytes())
        || bytes[..4] != [0xcf, 0xfa, 0xed, 0xfe]
        || u32::from_le_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| profile("invalid native header"))?,
        ) != 0x0100_000c
    {
        return Err(profile(
            "native preparation requires an arm64 Mach-O executable on macOS",
        ));
    }
    Ok(())
}

/// Compile an agent's explicit native route through the existing Rust builder.
/// The original K is copied as input and compared after canonical compilation.
pub fn form_native_candidate(
    original: &PortableApplicationBundle,
    workspace: &Path,
    proposal: NativeProcessProposal,
) -> Result<(Vec<u8>, PortableApplicationBundle), PortableApplicationError> {
    if proposal.schema != "ato.native-process-proposal/1" {
        return Err(profile("unsupported native process proposal schema"));
    }
    let routes = validate_all_derivations(original)?;
    let base = routes.first().ok_or_else(|| profile("no original route"))?;
    let requirements = base
        .contract
        .requirements
        .iter()
        .map(|requirement| {
            if requirement.verifier != ato_formation::authoring::HTTP_CONTRACT_VERIFIER
                || requirement.method.as_deref() != Some("GET")
                || requirement.port.as_deref() != Some("app.http")
            {
                return Err(profile(
                    "native proposal cannot rewrite a non-HTTP original Contract",
                ));
            }
            Ok(PortableHttpRequirementSpec {
                id: requirement.id.clone(),
                path: requirement.path.clone().unwrap_or_else(|| "/".to_owned()),
                status: requirement.status.unwrap_or(200),
                body_digest: requirement.body_digest.clone(),
            })
        })
        .collect::<Result<Vec<_>, PortableApplicationError>>()?;
    let (bytes, candidate) = build_dynamic_routes_bundle(
        workspace,
        &PortableAuthoredBundleSpec {
            title: base.application.title.clone(),
            surface_path: base.application.surfaces[0].path.clone(),
            routes: vec![PortableDynamicRouteSpec {
                realization: PortableRealizationKind::LocalProcess,
                execution: PortableExecutionSpec {
                    runtimes: BTreeMap::from([
                        (NATIVE_OS.to_owned(), proposal.target_os),
                        (NATIVE_ARCH.to_owned(), proposal.target_arch),
                        (NATIVE_EXECUTABLE.to_owned(), proposal.executable),
                        (NATIVE_ENCODING.to_owned(), proposal.encoding),
                    ]),
                    argv: proposal.argv,
                    cwd: proposal.cwd,
                    env: proposal.env,
                },
                guest_port: base.derivation.ports[0]
                    .guest_port
                    .ok_or_else(|| profile("no declared HTTP port"))?,
                services: Vec::new(),
            }],
            filesystem_state: base.derivation.state.first().map(|state| {
                PortableFilesystemStateSpec {
                    id: state.id.clone(),
                    mount: state.mount.clone(),
                }
            }),
            bindings: base.application.bindings.clone(),
            requirements,
            effects: base.derivation.effects,
        },
    )?;
    let report = assess_preparation(
        original,
        Some(&candidate),
        PreparationConstraints {
            no_vm: true,
            offline_after_save: true,
        },
        None,
    )?;
    if report.status != "ready" {
        return Err(profile(
            "native proposal does not satisfy the frozen constraints",
        ));
    }
    Ok((bytes, candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_executables_and_script_wrappers_are_not_native_candidates() {
        assert!(validate_native_image(b"#!/bin/sh\ndocker run image", "macos", "aarch64").is_err());
        let mut bytes = vec![0; 32];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(&0x0100_000cu32.to_le_bytes());
        bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
        assert!(validate_native_image(&bytes, "macos", "aarch64").is_ok());
        assert!(validate_native_image(&bytes, "macos", "x86_64").is_err());
    }

    fn constraints() -> PreparationConstraints {
        PreparationConstraints {
            no_vm: true,
            offline_after_save: true,
        }
    }

    #[test]
    fn existing_routes_require_an_explicit_choice_when_multiple_are_eligible() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples/portable-multi-process-authored");
        let (_, original) = crate::build_authored_bundle_v2(&source).unwrap();
        let report = assess_preparation(&original, None, constraints(), None).unwrap();
        assert_eq!(report.status, "needs_selection");
        assert_eq!(report.eligible_derivations.len(), 2);
        let selected = &report.eligible_derivations[0];
        let ready = assess_preparation(&original, None, constraints(), Some(selected)).unwrap();
        assert_eq!(
            ready.selected_derivation_ref.as_deref(),
            Some(selected.as_str())
        );
        assert_eq!(ready.contract_ref, report.contract_ref);
        assert!(assess_preparation(&original, None, constraints(), Some("undeclared")).is_err());
    }

    #[test]
    fn compressed_native_inputs_reject_corruption_and_unknown_encoding() {
        assert!(decode_native_image(b"invalid gzip", "gzip").is_err());
        assert!(decode_native_image(b"binary", "tar").is_err());
    }

    #[test]
    fn preparation_cannot_accept_a_candidate_with_a_different_contract() {
        let temporary = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
        std::fs::create_dir_all(&temporary).unwrap();
        let source = tempfile::tempdir_in(temporary).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../samples/interop-static-k/capsule.toml"),
            source.path().join("capsule.toml"),
        )
        .unwrap();
        std::fs::write(source.path().join("index.html"), "original").unwrap();
        std::fs::write(source.path().join("proof.txt"), "proof").unwrap();
        let (_, original) = crate::build_static_bundle(source.path(), "test").unwrap();
        std::fs::write(source.path().join("index.html"), "changed").unwrap();
        let (_, changed) = crate::build_static_bundle(source.path(), "test").unwrap();
        assert!(
            assess_preparation(
                &original,
                Some(&changed),
                PreparationConstraints {
                    no_vm: true,
                    offline_after_save: true
                },
                None
            )
            .is_err()
        );
    }
}
