use std::path::Path;

use ato_portable_application::{build_static_bundle, bundle_sha256};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
    let output = source.join("../interop-static.capsule");
    if output.exists() {
        return Err(format!(
            "{} already exists; the interoperability fixture is generated only once",
            output.display()
        )
        .into());
    }
    let (bytes, bundle) = build_static_bundle(&source, "Ato portability proof")?;
    std::fs::write(&output, &bytes)?;
    println!("bundle_sha256={}", bundle_sha256(&bytes));
    println!("root_contract_ref={}", bundle.index.root_contract_ref);
    println!("derivation_ref={}", bundle.index.derivations[0]);
    println!("output={}", output.display());
    Ok(())
}
