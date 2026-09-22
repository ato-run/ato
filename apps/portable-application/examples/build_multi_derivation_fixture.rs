use std::path::Path;

use ato_portable_application::{
    PortableRealizationKind, build_multi_derivation_bundle, bundle_sha256, validate_all_derivations,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-multi-derivation");
    let output = source.join("../interop-multi-derivation.capsule");
    if output.exists() {
        return Err(format!(
            "{} already exists; the interoperability fixture is generated only once",
            output.display()
        )
        .into());
    }
    let (bytes, bundle) = build_multi_derivation_bundle(&source, "Ato multi-route proof")?;
    let validated = validate_all_derivations(&bundle)?;
    std::fs::write(&output, &bytes)?;
    println!("bundle_sha256={}", bundle_sha256(&bytes));
    println!("root_contract_ref={}", bundle.index.root_contract_ref);
    for route in validated {
        let name = match route.realization {
            PortableRealizationKind::StaticWeb => "static_derivation_ref",
            PortableRealizationKind::LocalProcess => "process_derivation_ref",
            PortableRealizationKind::OciContainer => "oci_derivation_ref",
            PortableRealizationKind::OciServiceGroup => "oci_service_group_derivation_ref",
        };
        println!("{name}={}", route.derivation_ref);
    }
    println!("output={}", output.display());
    Ok(())
}
