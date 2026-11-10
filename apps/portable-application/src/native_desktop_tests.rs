//! `ato.native@1` validation and selected-D admission.
//!
//! These tests build real bundles from the committed sample and from in-memory
//! sources, then reseal signed derivations to prove the validator fails closed.
//! Nothing here signs or launches an OS executable, so the same test body runs
//! wherever the workspace builds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ato_computation::ContentRef;
use ato_formation::authoring::{BoundDerivation, BoundState, BoundStep, StateAccess};
use ato_objects::{PortableApplicationBundle, PortableBundleObjectKind};

use crate::portable_run::{PortableBundleExecutor, PortableRunContext};
use crate::{
    BOUND_DERIVATION_SCHEMA, BundleShape, NATIVE_PROTOCOL, ObjectToEncode, PortableRealizationKind,
    add_structured, bundle_sha256, finish_bundle, validate_all_derivations, validate_bytes_all,
};

fn encode(bundle: &PortableApplicationBundle) -> Vec<u8> {
    crate::encode_portable_application_bundle(bundle).unwrap()
}

/// The committed authoring fixture PR8 reuses.
fn sample_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/native-desktop-multi-d")
}

fn sample_bundle() -> (Vec<u8>, PortableApplicationBundle) {
    crate::build_authored_bundle_v2(&sample_root()).expect("the native sample builds")
}

fn derivation_for(
    validated: &[crate::ValidatedPortableApplication],
    realization: PortableRealizationKind,
) -> &crate::ValidatedPortableApplication {
    validated
        .iter()
        .find(|route| route.realization == realization)
        .expect("route of the requested realization")
}

/// Rebuild a bundle around replacement Derivations. `remove` drops objects
/// (including their payloads) so a test can simulate a closure that does not
/// carry what a Derivation names.
fn objects_of(bundle: &PortableApplicationBundle) -> BTreeMap<String, ObjectToEncode> {
    let mut objects: BTreeMap<String, ObjectToEncode> = BTreeMap::new();
    for descriptor in &bundle.index.objects {
        let reference = ContentRef::parse(descriptor.reference.clone()).unwrap();
        let bytes = bundle.payload_bytes(&reference).unwrap();
        let object = match descriptor.kind {
            PortableBundleObjectKind::Structured => ObjectToEncode::Structured {
                schema: descriptor.schema.clone().unwrap(),
                bytes,
            },
            PortableBundleObjectKind::Blob => ObjectToEncode::Blob(bytes),
        };
        objects.insert(descriptor.reference.clone(), object);
    }
    objects
}

fn reseal(
    bundle: &PortableApplicationBundle,
    derivations: Vec<BoundDerivation>,
    remove: &[String],
) -> Vec<u8> {
    let mut objects = objects_of(bundle);
    for reference in remove {
        objects.remove(reference);
    }
    for reference in &bundle.index.derivations {
        objects.remove(reference);
    }
    let mut refs = Vec::with_capacity(derivations.len());
    for derivation in derivations {
        refs.push(add_structured(&mut objects, &derivation, BOUND_DERIVATION_SCHEMA).unwrap());
    }
    let probe = finish_bundle(
        objects,
        bundle.index.root_contract_ref.clone(),
        bundle.index.application_ref.clone(),
        refs,
        BundleShape::v3(),
    );
    // Keep only the closure the replacement Derivations reach, so dropping a
    // route does not leave its artifact behind as an unreachable object. When
    // the replacement itself is incomplete, hand the probe back untouched and
    // let the validator report the missing object.
    let Ok(reachable) = crate::portable_reference_registry().and_then(|registry| {
        ato_objects::reachable_portable_application_objects(&probe, &registry)
            .map_err(crate::PortableApplicationError::from)
    }) else {
        return encode(&probe);
    };
    let reachable = reachable
        .iter()
        .map(|reference| reference.as_str().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut objects = objects_of(&probe);
    objects.retain(|reference, _| reachable.contains(reference));
    let filtered = finish_bundle(
        objects,
        probe.index.root_contract_ref.clone(),
        probe.index.application_ref.clone(),
        probe.index.derivations.clone(),
        BundleShape::v3(),
    );
    encode(&filtered)
}

fn native_step(derivation: &BoundDerivation) -> BoundStep {
    derivation
        .steps
        .iter()
        .find(|step| step.protocol == NATIVE_PROTOCOL)
        .expect("native serving step")
        .clone()
}

#[test]
fn one_source_declares_three_routes_over_one_contract() {
    let (bytes, _) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    assert_eq!(validated.len(), 3);
    let contract_refs = validated
        .iter()
        .map(|route| route.contract_ref.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(contract_refs.len(), 1, "all routes share one ContractRef");
    let derivation_refs = validated
        .iter()
        .map(|route| route.derivation_ref.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(derivation_refs.len(), 3, "DerivationRefs are distinct");
    assert_eq!(
        validated
            .iter()
            .map(|route| route.realization)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            PortableRealizationKind::StaticWeb,
            PortableRealizationKind::NativeDesktop,
        ]
        .into_iter()
        .collect()
    );

    let macos = validated
        .iter()
        .filter_map(|route| route.native.as_ref())
        .find(|native| native.descriptor.os.as_str() == "macos")
        .expect("the macOS native route");
    assert_eq!(macos.descriptor.arch.as_str(), "aarch64");
    assert_eq!(macos.descriptor.entry, "app/run.sh");
    assert_eq!(macos.ports.len(), 1);
    assert_eq!(macos.ports[0].id, "app.http");
    assert_eq!(macos.ports[0].guest_port, 18080);
    assert!(
        macos.descriptor.signature.allows_local_override(),
        "the unsigned fixture artifact declares the explicit local override"
    );

    let windows = validated
        .iter()
        .filter_map(|route| route.native.as_ref())
        .find(|native| native.descriptor.os.as_str() == "windows")
        .expect("the Windows native route");
    assert_eq!(windows.descriptor.arch.as_str(), "x86_64");
    assert!(!windows.descriptor.signature.allows_local_override());
}

#[test]
fn adding_native_routes_changes_neither_the_contract_ref_nor_its_requirement_set() {
    let sample_root = sample_root();
    let (full_bytes, _) = crate::build_authored_bundle_v2(&sample_root).unwrap();
    let (_, full) = validate_bytes_all(&full_bytes).unwrap();

    // The same K declared with only the static route.
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    std::fs::create_dir_all(root.join("dist")).unwrap();
    std::fs::copy(sample_root.join("index.html"), root.join("index.html")).unwrap();
    std::fs::copy(sample_root.join("proof.txt"), root.join("proof.txt")).unwrap();
    let control = std::fs::read_to_string(sample_root.join("capsule.toml"))
        .unwrap()
        .replace(
            r#"[[derivation]]
id = "native-macos""#,
            r#"# removed for the control bundle
[[derivation]]
id = "native-macos""#,
        );
    // Drop the two native routes by keeping only the web derivation.
    let web_only = control
        .split("# removed for the control bundle")
        .next()
        .unwrap()
        .to_owned()
        + "\n[effects]\ndefault = \"pure\"\n";
    std::fs::write(root.join("capsule.toml"), web_only).unwrap();
    let (control_bytes, _) = crate::build_authored_bundle_v2(root)
        .expect("the control bundle with one static route builds");
    let (_, control) = validate_bytes_all(&control_bytes).unwrap();
    assert_eq!(control.len(), 1);

    assert_eq!(control[0].contract_ref, full[0].contract_ref);
    assert_eq!(control[0].contract, full[0].contract);
}

fn host_facts(
    os: &str,
    arch: &str,
    native: ato_host_facts::NativeFacts,
) -> ato_host_facts::HostFacts {
    ato_host_facts::HostFacts {
        os: os.to_owned(),
        arch: arch.to_owned(),
        docker: Default::default(),
        linux_sandbox: false,
        native,
    }
}

fn admit(
    bundle: &PortableApplicationBundle,
    route: &crate::ValidatedPortableApplication,
    runtime_root: &Path,
    host: &ato_host_facts::HostFacts,
) -> Option<ato_formation::request::AttemptFailure> {
    let context = PortableRunContext {
        cache_root: runtime_root,
        sandbox_shim: Path::new("/nonexistent/ato"),
        sandbox_subcommand: "__portable-sandbox-exec",
        host: Some(host),
        native_local_override: false,
    };
    let binding_environment = BTreeMap::new();
    let executor = PortableBundleExecutor {
        bundle,
        validated: route,
        runtime_root,
        static_state: None,
        filesystem_state: None,
        binding_environment: &binding_environment,
        context: &context,
    };
    ato_runtime_attempt::realize::CandidateRealizer::admit(
        &executor,
        &ato_formation::request::RuntimeProfile {
            runtime_id: "portable-fixture".to_owned(),
            capabilities: BTreeMap::new(),
        },
    )
}

#[test]
fn a_native_route_is_admitted_only_on_the_platform_it_was_built_for() {
    let (bytes, bundle) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    let macos = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "macos")
        })
        .unwrap();
    let windows = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "windows")
        })
        .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let macos_host = host_facts(
        "macos",
        "aarch64",
        ato_host_facts::NativeFacts {
            sandbox_tool: true,
            sandbox_detail: None,
            signature_tools: true,
        },
    );
    assert!(
        admit(&bundle, macos, scratch.path(), &macos_host).is_none(),
        "the macOS route is admitted on a macOS host that can enforce its policies"
    );
    let failure = admit(&bundle, windows, scratch.path(), &macos_host)
        .expect("a Windows route must not be admitted on macOS");
    assert_eq!(failure.code, "native_platform_mismatch");
    assert_eq!(failure.stage, "admission");

    // The same bundle and the same Windows D are format-valid on that host:
    // selecting another D never invalidates this one.
    let (_, windows_route) =
        crate::validate_bytes_for_derivation(&bytes, windows.derivation_ref.as_str())
            .expect("the Windows D validates as a bundle object on every host");
    assert_eq!(
        windows_route.realization,
        PortableRealizationKind::NativeDesktop
    );

    // A static web route over the same K remains runnable on this host.
    let web = derivation_for(&validated, PortableRealizationKind::StaticWeb);
    assert!(admit(&bundle, web, scratch.path(), &macos_host).is_none());

    // On a Windows host the two roles swap.
    let windows_host = host_facts(
        "windows",
        "x86_64",
        ato_host_facts::NativeFacts {
            sandbox_tool: false,
            sandbox_detail: Some("not enforced".to_owned()),
            signature_tools: true,
        },
    );
    let failure = admit(&bundle, windows, scratch.path(), &windows_host)
        .expect("the Windows Adapter is fail-closed until its mandatory policies are enforceable");
    assert_eq!(failure.code, "native_windows_containment_unavailable");
}

#[test]
fn a_native_route_whose_mandatory_policy_cannot_be_enforced_is_refused() {
    let (bytes, bundle) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    let macos = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "macos")
        })
        .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    for native in [
        ato_host_facts::NativeFacts {
            sandbox_tool: false,
            sandbox_detail: Some("not enforced".to_owned()),
            signature_tools: true,
        },
        ato_host_facts::NativeFacts {
            sandbox_tool: true,
            sandbox_detail: None,
            signature_tools: false,
        },
    ] {
        let host = host_facts("macos", "aarch64", native.clone());
        let failure = admit(&bundle, macos, scratch.path(), &host)
            .expect("an unenforceable mandatory policy must refuse admission");
        let expected = if native.sandbox_tool {
            "native_signature_verification_unavailable"
        } else {
            "native_sandbox_unavailable"
        };
        assert_eq!(failure.code, expected);
    }
}

#[test]
fn a_descriptor_the_validator_cannot_honour_fails_closed() {
    let (bytes, bundle) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    let macos = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "macos")
        })
        .unwrap();
    let native_ref = macos.derivation_ref.as_str().to_owned();
    let artifact_ref = macos.native.as_ref().unwrap().descriptor.artifact.clone();

    let original = macos.derivation.clone();
    let mut cases: Vec<(&str, BoundDerivation, Vec<String>, &str)> = Vec::new();

    let unknown_key = original.clone();
    let step = native_step(&unknown_key);
    let mut candidate = step.clone();
    candidate
        .runtimes
        .insert("ato.native.smuggle".to_owned(), "x".to_owned());
    let mut derivation = original.clone();
    derivation.steps = vec![candidate];
    cases.push(("unknown descriptor key", derivation, Vec::new(), "native"));

    let mut non_blob = original.clone();
    let mut candidate = native_step(&non_blob);
    candidate.runtimes.insert(
        ato_formation::native_desktop::NATIVE_ARTIFACT_RUNTIME.to_owned(),
        bundle.index.root_contract_ref.clone(),
    );
    non_blob.steps = vec![candidate];
    cases.push((
        "artifact is not a blob",
        non_blob,
        vec![artifact_ref.clone()],
        "blob",
    ));

    let mut absent = original.clone();
    let mut candidate = native_step(&absent);
    candidate.runtimes.insert(
        ato_formation::native_desktop::NATIVE_ARTIFACT_RUNTIME.to_owned(),
        format!("sha256:{}", "f".repeat(64)),
    );
    absent.steps = vec![candidate];
    cases.push((
        "artifact blob is absent",
        absent,
        vec![artifact_ref.clone()],
        "closure",
    ));

    for (what, entry) in [
        ("absolute entry", "/app/run.sh"),
        ("traversing entry", "app/../../run.sh"),
        ("empty segment entry", "app//run.sh"),
        ("drive entry", "C:/run.sh"),
    ] {
        let mut derivation = original.clone();
        let mut candidate = native_step(&derivation);
        candidate.runtimes.insert(
            ato_formation::native_desktop::NATIVE_ENTRY_RUNTIME.to_owned(),
            entry.to_owned(),
        );
        derivation.steps = vec![candidate];
        cases.push((what, derivation, Vec::new(), "native"));
    }

    for (what, key, value) in [
        (
            "unknown format",
            ato_formation::native_desktop::NATIVE_FORMAT_RUNTIME,
            "zip",
        ),
        (
            "unknown signature policy",
            ato_formation::native_desktop::NATIVE_SIGNATURE_RUNTIME,
            "verify-and-record",
        ),
        (
            "unknown operating system",
            ato_formation::native_desktop::NATIVE_OS_RUNTIME,
            "linux",
        ),
        (
            "unknown architecture",
            ato_formation::native_desktop::NATIVE_ARCH_RUNTIME,
            "amd64",
        ),
    ] {
        let mut derivation = original.clone();
        let mut candidate = native_step(&derivation);
        candidate.runtimes.insert(key.to_owned(), value.to_owned());
        derivation.steps = vec![candidate];
        cases.push((what, derivation, Vec::new(), "native"));
    }

    for (what, mutate) in [
        (
            "wrong op",
            (|derivation: &mut BoundDerivation| {
                derivation.steps[0].op = "build".to_owned();
            }) as fn(&mut BoundDerivation),
        ),
        ("undeclared port", |derivation: &mut BoundDerivation| {
            derivation.ports.clear();
        }),
        (
            "port from another step",
            |derivation: &mut BoundDerivation| {
                derivation.ports[0].from = "other".to_owned();
            },
        ),
        (
            "port without a loopback port",
            |derivation: &mut BoundDerivation| {
                derivation.ports[0].guest_port = None;
            },
        ),
        (
            "host condition on the route",
            |derivation: &mut BoundDerivation| {
                derivation.requirements.host = Some(ato_formation::requirements::HostRequirement {
                    os: ato_formation::requirements::HostOs::Macos,
                    arch: ato_formation::requirements::HostArch::Aarch64,
                    accelerators: Vec::new(),
                    min_memory_mib: None,
                    min_scratch_mib: None,
                });
            },
        ),
        (
            "fixed-mount state on a native route",
            |derivation: &mut BoundDerivation| {
                derivation.state = vec![BoundState {
                    id: "data".to_owned(),
                    protocol: "ato.state.filesystem@1".to_owned(),
                    mount: "/state/data".to_owned(),
                    access: StateAccess::ReadWrite,
                    mount_required: None,
                }];
            },
        ),
        (
            "two native serving steps",
            |derivation: &mut BoundDerivation| {
                let mut second = derivation.steps[0].clone();
                second.id = "serve-two".to_owned();
                derivation.steps.push(second);
            },
        ),
    ] {
        let mut derivation = original.clone();
        mutate(&mut derivation);
        cases.push((what, derivation, Vec::new(), "native"));
    }

    for (what, derivation, remove, boundary) in cases {
        let resealed = reseal(&bundle, vec![derivation], &remove);
        match validate_bytes_all(&resealed) {
            Err(error) => {
                let detail = error.to_string().to_lowercase();
                assert!(
                    detail.contains(boundary) || detail.contains("native"),
                    "{what}: unexpected refusal `{error}`"
                );
            }
            Ok(_) => panic!("{what} was accepted by the validator"),
        }
        // The unmodified bundle still validates: each case is a mutation of a
        // signed Derivation, not a change to K.
        let (_, still) = validate_bytes_all(&bytes).unwrap();
        assert_eq!(still.len(), 3);
        assert!(
            still
                .iter()
                .any(|route| route.derivation_ref.as_str() == native_ref)
        );
    }
}

#[test]
fn a_native_route_whose_artifact_has_no_digest_carrying_blob_is_refused() {
    let (bytes, bundle) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    let macos = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "macos")
        })
        .unwrap();
    let artifact = macos.native.as_ref().unwrap().descriptor.artifact.clone();
    let resealed = reseal(&bundle, vec![macos.derivation.clone()], &[artifact]);
    let error = validate_bytes_all(&resealed).unwrap_err();
    assert!(error.to_string().to_lowercase().contains("closure"));
}

#[test]
fn the_native_artifact_is_part_of_the_verified_closure() {
    let (bytes, bundle) = sample_bundle();
    for native in validate_all_derivations(&bundle)
        .unwrap()
        .iter()
        .filter_map(|route| route.native.as_ref())
    {
        let reference = ContentRef::parse(native.descriptor.artifact.clone()).unwrap();
        let descriptor = bundle.descriptor(&reference).expect("artifact descriptor");
        assert_eq!(descriptor.kind, PortableBundleObjectKind::Blob);
        assert!(descriptor.schema.is_none());
        let payload = bundle.payload_bytes(&reference).unwrap();
        assert_eq!(bundle_sha256(&payload), native.descriptor.artifact);
    }
    // Removing the artifact from a bundle whose derivation still names it is a
    // broken closure, not a bundle without a native route.
    let first = validate_all_derivations(&bundle)
        .unwrap()
        .into_iter()
        .find(|route| route.native.is_some())
        .unwrap();
    let resealed = reseal(
        &bundle,
        vec![first.derivation.clone()],
        &[first.native.unwrap().descriptor.artifact],
    );
    assert!(validate_bytes_all(&resealed).is_err());
    drop(bytes);
}

#[test]
fn a_native_route_that_declares_tcp_egress_is_refused_at_admission() {
    let (bytes, bundle) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    let macos = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "macos")
        })
        .unwrap();
    let mut derivation = macos.derivation.clone();
    derivation.requirements.network = vec![ato_formation::requirements::NetworkRequirement {
        phase: ato_formation::requirements::ExecutionPhase::Runtime,
        host: "example.com".to_owned(),
        port: 443,
    }];
    let resealed = reseal(&bundle, vec![derivation], &[]);
    let resealed_bundle = crate::decode_portable_application_bundle(&resealed).unwrap();
    let selected = resealed_bundle.index.derivations[0].clone();
    let route = crate::validate_bundle_for_derivation(&resealed_bundle, selected.as_str()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let host = host_facts(
        "macos",
        "aarch64",
        ato_host_facts::NativeFacts {
            sandbox_tool: true,
            sandbox_detail: None,
            signature_tools: true,
        },
    );
    let failure = admit(&resealed_bundle, &route, scratch.path(), &host);
    assert_eq!(
        failure
            .expect("an unenforceable egress policy must refuse")
            .code,
        "native_egress_policy_unavailable"
    );
}

/// PR6: the Windows adapter is fail-closed on its own platform, while the
/// bundle and every other Derivation stay valid and runnable.
#[test]
fn a_windows_native_route_is_fail_closed_on_windows_and_only_a_platform_mismatch_elsewhere() {
    let (bytes, bundle) = sample_bundle();
    let (_, validated) = validate_bytes_all(&bytes).unwrap();
    let windows = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "windows")
        })
        .unwrap();
    let macos = validated
        .iter()
        .find(|route| {
            route
                .native
                .as_ref()
                .is_some_and(|n| n.descriptor.os.as_str() == "macos")
        })
        .unwrap();
    let web = derivation_for(&validated, PortableRealizationKind::StaticWeb);
    let scratch = tempfile::tempdir().unwrap();

    // The bundle carrying the Windows D stays format-valid everywhere, and the
    // Windows D itself is a valid selected route.
    assert_eq!(validated.len(), 3);
    let (_, selected) =
        crate::validate_bytes_for_derivation(&bytes, windows.derivation_ref.as_str()).unwrap();
    assert_eq!(selected.realization, PortableRealizationKind::NativeDesktop);

    let macos_host = host_facts(
        "macos",
        "aarch64",
        ato_host_facts::NativeFacts {
            sandbox_tool: true,
            sandbox_detail: None,
            signature_tools: true,
        },
    );
    let failure = admit(&bundle, windows, scratch.path(), &macos_host)
        .expect("a Windows route is not admitted on macOS");
    assert_eq!(failure.code, "native_platform_mismatch");
    // The static route over the same K is runnable on that host.
    assert!(admit(&bundle, web, scratch.path(), &macos_host).is_none());

    let windows_host = host_facts(
        "windows",
        "x86_64",
        ato_host_facts::NativeFacts {
            sandbox_tool: false,
            sandbox_detail: Some("AppContainer policy not implemented".to_owned()),
            signature_tools: true,
        },
    );
    // On its own platform the Windows D is refused with the named fail-closed
    // reason: nothing is launched and no containment is claimed.
    let failure = admit(&bundle, windows, scratch.path(), &windows_host)
        .expect("Windows native admission must fail closed");
    assert_eq!(failure.code, "native_windows_containment_unavailable");
    assert_eq!(failure.stage, "admission");
    // A macOS D is a platform mismatch there, and the static route still runs.
    let failure = admit(&bundle, macos, scratch.path(), &windows_host)
        .expect("a macOS route is not admitted on Windows");
    assert_eq!(failure.code, "native_platform_mismatch");
    assert!(admit(&bundle, web, scratch.path(), &windows_host).is_none());
}

fn tiny_bundle_artifact() -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let bytes = b"#!/bin/sh\nexit 0\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o755);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(
            &mut header,
            "MyApp.app/Contents/MacOS/MyApp",
            std::io::Cursor::new(bytes),
        )
        .unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn write_bundle_source(root: &Path, signature_target: Option<&str>) {
    std::fs::create_dir_all(root.join("artifacts")).unwrap();
    std::fs::write(root.join("proof.txt"), b"proof\n").unwrap();
    std::fs::write(root.join("artifacts/App.tar.gz"), tiny_bundle_artifact()).unwrap();
    let target = signature_target
        .map(|value| format!("signature_target = \"{value}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        root.join("capsule.toml"),
        format!(
            r#"schema = "ato.capsule/2"

[application]
title = "Native bundle target"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "proof"
use = "ato.contract.http@1"
port = "app.http"
path = "/proof.txt"
status = 200

[[derivation]]
id = "native"
use = "ato.native@1"
runtimes = {{}}
guest_port = 18080
artifact = "artifacts/App.tar.gz"
os = "macos"
arch = "aarch64"
entry = "MyApp.app/Contents/MacOS/MyApp"
signature = "require-valid-or-local-override"
{target}
[effects]
default = "pure"
"#
        ),
    )
    .unwrap();
}

#[test]
fn a_bundle_signature_target_is_carried_through_authoring_and_changes_only_its_own_route() {
    let scratch = tempfile::tempdir().unwrap();
    let default_root = scratch.path().join("default");
    let explicit_root = scratch.path().join("explicit");
    let bundle_root = scratch.path().join("bundle");
    write_bundle_source(&default_root, None);
    write_bundle_source(&explicit_root, Some("executable"));
    write_bundle_source(&bundle_root, Some("bundle"));

    let build = |root: &Path| {
        let (bytes, _) = crate::build_authored_bundle_v2(root).expect("the source builds");
        let (_, validated) = validate_bytes_all(&bytes).expect("the bundle validates");
        let route = validated
            .iter()
            .find(|route| route.realization == PortableRealizationKind::NativeDesktop)
            .unwrap()
            .clone();
        (bytes, route)
    };
    let (_, default_route) = build(&default_root);
    let (_, explicit_route) = build(&explicit_root);
    let (_, bundle_route) = build(&bundle_root);

    // `executable` is the default, so declaring it changes nothing.
    assert_eq!(
        default_route
            .native
            .as_ref()
            .unwrap()
            .descriptor
            .signature_target,
        ato_formation::native_desktop::NativeSignatureTarget::Executable
    );
    assert_eq!(default_route.derivation_ref, explicit_route.derivation_ref);
    assert_eq!(default_route.contract_ref, bundle_route.contract_ref);
    assert_ne!(default_route.derivation_ref, bundle_route.derivation_ref);

    let native = bundle_route.native.as_ref().unwrap();
    assert_eq!(
        native.descriptor.signature_target,
        ato_formation::native_desktop::NativeSignatureTarget::Bundle
    );
    assert_eq!(
        bundle_route.derivation.steps[0]
            .runtimes
            .get(ato_formation::native_desktop::NATIVE_SIGNATURE_TARGET_RUNTIME)
            .map(String::as_str),
        Some("bundle")
    );
    // The artifact still enters the verified closure.
    assert!(
        bundle_route
            .derivation
            .inputs
            .iter()
            .all(|input| input.protocol == ato_formation::authoring::WORKSPACE_PROTOCOL)
    );
}
