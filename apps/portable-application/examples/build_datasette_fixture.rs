use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use ato_portable_application::{
    OCI_CPU_MILLIS_RUNTIME, OCI_IMAGE_RUNTIME, OCI_MEMORY_BYTES_RUNTIME, OCI_PIDS_LIMIT_RUNTIME,
    OCI_PLATFORM_RUNTIME, PYTHON_RUNTIME, PortableDynamicBundleSpec, PortableExecutionSpec,
    PortableHttpRequirementSpec, PortableRealizationKind, build_dynamic_process_oci_bundle,
    bundle_sha256, validate_all_derivations,
};

const ROWS_SHA256: &str = "sha256:8607a7b55d2f0b4f9169a1c63b1554a60a6af1c8424ab591aa9a451265e0ee83";
const TOTAL_SHA256: &str =
    "sha256:b4d3392b69f97f35ad0c927a0401766b538ebbf1baabd4ee14f620b0ac54632f";
const IMAGE: &str = "docker.io/datasetteproject/datasette@sha256:0f57db16cf4eb6cca57f1cedaa0a696bca1c65a1d75b8f7ee372c2dd909a32a0";

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-datasette");
    let output = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("../datasette-cpu.capsule"));
    let spec = PortableDynamicBundleSpec {
        title: "Datasette catalog".to_owned(),
        surface_path: "/".to_owned(),
        guest_port: 8000,
        process: PortableExecutionSpec {
            runtimes: BTreeMap::from([(PYTHON_RUNTIME.to_owned(), "3.12".to_owned())]),
            argv: vec!["python3".to_owned(), "bootstrap.py".to_owned()],
            cwd: ".".to_owned(),
            env: BTreeMap::from([("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned())]),
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
                // The command is part of this Derivation. Do not rely on an
                // image tag's mutable/default Entrypoint or Cmd metadata.
                "datasette".to_owned(),
                "--immutable".to_owned(),
                "/app/catalog.db".to_owned(),
                "--host".to_owned(),
                "0.0.0.0".to_owned(),
                "--port".to_owned(),
                "8000".to_owned(),
            ],
            cwd: ".".to_owned(),
            env: BTreeMap::new(),
        },
        requirements: vec![
            PortableHttpRequirementSpec {
                id: "datasette-entry".to_owned(),
                path: "/".to_owned(),
                status: 200,
                body_digest: None,
            },
            PortableHttpRequirementSpec {
                id: "items-in-id-order".to_owned(),
                path: "/catalog/items.json?_shape=array&_sort=id".to_owned(),
                status: 200,
                body_digest: Some(ROWS_SHA256.to_owned()),
            },
            PortableHttpRequirementSpec {
                id: "quantity-total".to_owned(),
                path: "/catalog.json?sql=select%20sum(quantity)%20as%20total%20from%20items&_shape=array"
                    .to_owned(),
                status: 200,
                body_digest: Some(TOTAL_SHA256.to_owned()),
            },
        ],
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
