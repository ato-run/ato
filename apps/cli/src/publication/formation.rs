//! Compile agent-proposed, source-owned Python entrypoints without executing them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, ensure};
use ato_formation::authoring::EffectClass;
use ato_objects::{PortableApplicationBundle, PortableDependencyProfile, PortableOciArchive};
use ato_portable_application::{
    PYTHON_RUNTIME, PortableAuthoredBundleSpec, PortableDynamicRouteSpec, PortableExecutionSpec,
    PortableHttpRequirementSpec, PortableRealizationKind, ValidatedPortableApplication,
    build_dynamic_routes_bundle, oci_archive::verify_oci_archive, oci_images,
    portability_export::repack_portable_dependencies_with_archives,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;

use super::{HttpCheck, MAX_ARTIFACT, limited_text, read_limited};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OciArchiveInput {
    image: String,
    path: PathBuf,
}

pub(super) fn resolve_oci_archives(
    workspace: &Path,
    source: &Path,
    inputs: Vec<OciArchiveInput>,
) -> Result<BTreeMap<String, PathBuf>> {
    ensure!(inputs.len() <= 8, "Supply at most eight OCI archives");
    let mut sources = BTreeMap::new();
    for input in inputs {
        let path = workspace
            .join(input.path)
            .canonicalize()
            .map_err(|_| anyhow::anyhow!("Cannot open the requested OCI archive"))?;
        ensure!(
            path.starts_with(workspace) && path.is_file(),
            "The OCI archive must be a file inside the authorized workspace"
        );
        ensure!(
            !path.starts_with(source),
            "Place OCI archives beside the source directory, outside the captured source tree"
        );
        ensure!(
            sources.insert(input.image, path).is_none(),
            "Supply one OCI archive per pinned image"
        );
    }
    Ok(sources)
}

pub(super) fn embed_oci_archives(
    bundle: &PortableApplicationBundle,
    routes: &[ValidatedPortableApplication],
    sources: BTreeMap<String, PathBuf>,
) -> Result<Vec<u8>> {
    ensure!(
        bundle
            .portability
            .as_ref()
            .is_none_or(|transport| transport.external_objects.is_empty()),
        "Embed external dependency objects before preparing an offline OCI publication"
    );
    let mut expected = BTreeMap::new();
    for (image, platform) in routes.iter().flat_map(oci_images) {
        if let Some(previous) = expected.insert(image, platform.clone()) {
            ensure!(
                previous == platform,
                "Each OCI manifest digest must identify a single platform"
            );
        }
    }
    let mut archives: BTreeMap<_, _> = bundle
        .portability
        .iter()
        .flat_map(|transport| transport.oci_archives.iter())
        .map(|archive| (archive.image.clone(), archive.clone()))
        .collect();
    let mut remaining = MAX_ARTIFACT * 3 / 4;
    for (image, path) in sources {
        let platform = expected.get(&image).ok_or_else(|| {
            anyhow::anyhow!(
                "The supplied OCI archive must name a pinned image declared by the artifact"
            )
        })?;
        let archive_bytes = read_limited(std::fs::File::open(path)?, remaining)?;
        remaining -= archive_bytes.len() as u64;
        let archive = PortableOciArchive {
            image: image.clone(),
            platform: platform.clone(),
            bytes: STANDARD.encode(archive_bytes),
        };
        verify_oci_archive(&archive).map_err(|_| {
            anyhow::anyhow!(
                "The OCI archive does not match the declared manifest, platform, and layer closure"
            )
        })?;
        archives.insert(image, archive);
    }
    ensure!(
        expected.keys().all(|image| archives.contains_key(image)),
        "Supply a verified archive for every pinned OCI image declared by the artifact"
    );
    let archives: Vec<_> = archives.into_values().collect();
    let (bytes, _) = repack_portable_dependencies_with_archives(
        bundle,
        PortableDependencyProfile::Offline,
        &BTreeMap::new(),
        &archives,
    )?;
    ensure!(
        bytes.len() as u64 <= MAX_ARTIFACT,
        "This publication workflow supports bundles up to 8 MiB"
    );
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessRoute {
    script: String,
    python_version: String,
    guest_port: u16,
    #[serde(default)]
    arguments: Vec<String>,
}

pub(super) fn build_process_bundle(
    root: &Path,
    title: &str,
    checks: &[HttpCheck],
    proposals: Vec<ProcessRoute>,
) -> Result<Vec<u8>> {
    ensure!(
        !checks.is_empty(),
        "Propose at least one required HTTP check for a Python process route"
    );
    let root = root.canonicalize()?;
    let mut seen = BTreeSet::new();
    let mut routes = Vec::with_capacity(proposals.len());
    for proposed in proposals {
        let script = Path::new(&proposed.script);
        ensure!(
            limited_text(&proposed.script, 250)
                && !proposed.script.contains('\\')
                && script.extension().is_some_and(|value| value == "py")
                && script
                    .components()
                    .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
                && root
                    .join(script)
                    .canonicalize()
                    .is_ok_and(|path| { path.starts_with(&root) && path.is_file() }),
            "Each Python route needs a source-relative .py script in the frozen workspace"
        );
        let version: Vec<_> = proposed.python_version.split('.').collect();
        ensure!(
            matches!(version.len(), 2 | 3)
                && version[0] == "3"
                && version.iter().all(|part| {
                    !part.is_empty() && part.len() <= 3 && part.bytes().all(|b| b.is_ascii_digit())
                }),
            "Pin a Python version such as 3.11 or 3.11.2"
        );
        ensure!(
            proposed.guest_port >= 1024,
            "Declare an application port between 1024 and 65535"
        );
        ensure!(
            proposed.arguments.len() <= 24
                && proposed.arguments.iter().all(|arg| limited_text(arg, 512)),
            "Provide at most 24 nonempty script arguments"
        );
        let mut argv = vec!["python3".to_owned(), format!("./{}", proposed.script)];
        argv.extend(proposed.arguments);
        ensure!(
            seen.insert((
                proposed.python_version.clone(),
                proposed.guest_port,
                argv.clone()
            )),
            "Propose distinct Python process routes"
        );
        routes.push(PortableDynamicRouteSpec {
            realization: PortableRealizationKind::LocalProcess,
            execution: PortableExecutionSpec {
                runtimes: BTreeMap::from([(PYTHON_RUNTIME.into(), proposed.python_version)]),
                argv,
                cwd: ".".into(),
                env: BTreeMap::new(),
            },
            guest_port: proposed.guest_port,
            entry: None,
            spa_fallback: None,
            native: None,
            services: Vec::new(),
            host: None,
            startup: None,
        });
    }
    let spec = PortableAuthoredBundleSpec {
        title: title.to_owned(),
        surface_path: "/".into(),
        routes,
        filesystem_state: None,
        bindings: Vec::new(),
        requirements: checks
            .iter()
            .map(|check| PortableHttpRequirementSpec {
                id: check.id.clone(),
                port: None,
                path: check.path.clone(),
                status: check.status,
                body_digest: check.body_sha256.clone(),
            })
            .collect(),
        tls_requirements: Vec::new(),
        terminal_requirements: Vec::new(),
        surface_port: "app.http".into(),
        effects: EffectClass::Pure,
        model_sets: Vec::new(),
        scenarios: Vec::new(),
    };
    Ok(build_dynamic_routes_bundle(&root, &spec)?.0)
}
