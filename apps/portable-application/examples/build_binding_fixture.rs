use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use ato_portable_application::{
    ApplicationBindingV1, OCI_CPU_MILLIS_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME,
    OCI_PIDS_LIMIT_RUNTIME, OCI_PLATFORM_RUNTIME, PYTHON_RUNTIME, PortableDynamicBundleSpec,
    PortableExecutionSpec, PortableHttpRequirementSpec, build_dynamic_process_oci_bundle,
};

const IMAGE: &str = "docker.io/library/python@sha256:1c44018d7eb40488f29e7c6ad4991d3200507e14dca71b94fe61011815e98155";
const HEALTH_SHA256: &str =
    "sha256:c7948e4dbe48ca72db6ccdabe6660fa01a0946c462a298a874326d29b388d91d";

fn main() -> Result<()> {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/portable-binding-echo");
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../portable-binding-echo.capsule"));
    let execution = PortableExecutionSpec {
        runtimes: BTreeMap::from([(PYTHON_RUNTIME.to_owned(), "3.12".to_owned())]),
        argv: vec!["python3".to_owned(), "app.py".to_owned()],
        cwd: ".".to_owned(),
        env: BTreeMap::from([("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned())]),
    };
    let spec = PortableDynamicBundleSpec {
        title: "Portable Binding Echo".to_owned(),
        surface_path: "/health".to_owned(),
        guest_port: 8000,
        process: execution,
        oci: PortableExecutionSpec {
            runtimes: BTreeMap::from([
                (OCI_IMAGE_RUNTIME.to_owned(), IMAGE.to_owned()),
                (OCI_PLATFORM_RUNTIME.to_owned(), "linux/amd64".to_owned()),
                (OCI_MEMORY_BYTES_RUNTIME.to_owned(), "268435456".to_owned()),
                (OCI_CPU_MILLIS_RUNTIME.to_owned(), "1000".to_owned()),
                (OCI_PIDS_LIMIT_RUNTIME.to_owned(), "128".to_owned()),
            ]),
            argv: vec!["python3".to_owned(), "/app/app.py".to_owned()],
            cwd: ".".to_owned(),
            env: BTreeMap::from([("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned())]),
        },
        filesystem_state: None,
        bindings: vec![ApplicationBindingV1 {
            id: "service".to_owned(),
            protocol: "ato.http-api@1".to_owned(),
            required: true,
        }],
        requirements: vec![PortableHttpRequirementSpec {
            id: "binding-health".to_owned(),
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
    println!("{}", output.display());
    println!(
        "bundle_sha256={}",
        ato_portable_application::bundle_sha256(&bytes)
    );
    println!("contract_ref={}", bundle.index.root_contract_ref);
    for derivation in bundle.index.derivations {
        println!("derivation_ref={derivation}");
    }
    Ok(())
}
