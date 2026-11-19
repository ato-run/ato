//! Compile agent-proposed, source-owned Python entrypoints without executing them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use anyhow::{Result, ensure};
use ato_formation::authoring::EffectClass;
use ato_portable_application::{
    PYTHON_RUNTIME, PortableAuthoredBundleSpec, PortableDynamicRouteSpec, PortableExecutionSpec,
    PortableHttpRequirementSpec, PortableRealizationKind, build_dynamic_routes_bundle,
};
use serde::Deserialize;

use super::{HttpCheck, limited_text};

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
