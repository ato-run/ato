use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use ato_portable_application::{
    OCI_CPU_MILLIS_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME, OCI_PIDS_LIMIT_RUNTIME,
    OCI_PLATFORM_RUNTIME, PYTHON_RUNTIME, PortableDynamicBundleSpec, PortableExecutionSpec,
    PortableFilesystemStateSpec, PortableHttpRequirementSpec, PortableRealizationKind,
    build_dynamic_process_oci_bundle, bundle_sha256, validate_all_derivations,
};

const IMAGE: &str = "docker.io/library/python@sha256:1c44018d7eb40488f29e7c6ad4991d3200507e14dca71b94fe61011815e98155";
const HEALTH_SHA256: &str =
    "sha256:4062edaf750fb8074e7e83e0c9028c94e32468a8b6f1614774328ef045150f93";

fn main() -> Result<()> {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/portable-stateful-notes");
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../portable-stateful-notes.capsule"));
    let environment = BTreeMap::from([
        ("APP_DB_PATH".to_owned(), "/data/notes.sqlite".to_owned()),
        ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
    ]);
    let spec = PortableDynamicBundleSpec {
        title: "Portable Notes".to_owned(),
        surface_path: "/".to_owned(),
        guest_port: 8000,
        process: PortableExecutionSpec {
            runtimes: BTreeMap::from([(PYTHON_RUNTIME.to_owned(), "3.12".to_owned())]),
            argv: vec!["python3".to_owned(), "-B".to_owned(), "app.py".to_owned()],
            cwd: ".".to_owned(),
            env: environment.clone(),
        },
        oci: PortableExecutionSpec {
            runtimes: BTreeMap::from([
                (OCI_IMAGE_RUNTIME.to_owned(), IMAGE.to_owned()),
                (OCI_PLATFORM_RUNTIME.to_owned(), "linux/amd64".to_owned()),
                (OCI_MEMORY_BYTES_RUNTIME.to_owned(), "268435456".to_owned()),
                (OCI_CPU_MILLIS_RUNTIME.to_owned(), "1000".to_owned()),
                (OCI_PIDS_LIMIT_RUNTIME.to_owned(), "128".to_owned()),
            ]),
            argv: vec![
                "python3".to_owned(),
                "-B".to_owned(),
                "/app/app.py".to_owned(),
            ],
            cwd: ".".to_owned(),
            env: environment,
        },
        filesystem_state: Some(PortableFilesystemStateSpec {
            id: "data".to_owned(),
            mount: "/data".to_owned(),
        }),
        bindings: Vec::new(),
        requirements: vec![PortableHttpRequirementSpec {
            id: "notes-health".to_owned(),
            path: "/health".to_owned(),
            status: 200,
            body_digest: Some(HEALTH_SHA256.to_owned()),
        }],
    };
    let (bytes, bundle) = build_dynamic_process_oci_bundle(&root, &spec)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, &bytes).with_context(|| format!("write {}", output.display()))?;
    let routes = validate_all_derivations(&bundle)?;
    let process = routes
        .iter()
        .find(|route| route.realization == PortableRealizationKind::LocalProcess)
        .context("process route")?;
    let oci = routes
        .iter()
        .find(|route| route.realization == PortableRealizationKind::OciContainer)
        .context("OCI route")?;
    println!("file={}", output.display());
    println!("bundle_sha256={}", bundle_sha256(&bytes));
    println!("contract_ref={}", process.contract_ref);
    println!("python_derivation_ref={}", process.derivation_ref);
    println!("oci_derivation_ref={}", oci.derivation_ref);
    println!("bytes={}", bytes.len());
    Ok(())
}
