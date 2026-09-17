//! Python wheel source discovery and digest-checked sparse-bundle hydration.
//! URLs are transport hints; the bundle's object references remain authority.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use ato_objects::{PortableApplicationBundle, PortableOciArchive};
use ato_portable_application::{
    OCI_IMAGE_RUNTIME, OCI_PLATFORM_RUNTIME, PortableRealizationKind,
    oci_archive::verify_oci_archive, validate_all_derivations,
};
use base64::Engine;

pub use ato_portable_application::dependency_transport::{
    discover_wheel_sources, hydrate_external_objects,
};

pub fn oci_archive_from_file(
    bundle: &PortableApplicationBundle,
    path: &Path,
) -> Result<Vec<PortableOciArchive>> {
    let images = validate_all_derivations(bundle)?
        .into_iter()
        .filter(|route| route.realization == PortableRealizationKind::OciContainer)
        .map(|route| {
            Ok((
                route
                    .derivation
                    .runtimes
                    .get(OCI_IMAGE_RUNTIME)
                    .context("OCI image missing")?
                    .clone(),
                route
                    .derivation
                    .runtimes
                    .get(OCI_PLATFORM_RUNTIME)
                    .context("OCI platform missing")?
                    .clone(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        images.len() == 1,
        "--oci-archive requires exactly one OCI Derivation"
    );
    let size = std::fs::metadata(path)
        .with_context(|| format!("inspect {}", path.display()))?
        .len();
    ensure!(
        size <= 128 * 1024 * 1024,
        "OCI archive exceeds bounded transport size"
    );
    let archive = PortableOciArchive {
        image: images[0].0.clone(),
        platform: images[0].1.clone(),
        bytes: base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(path).with_context(|| format!("read {}", path.display()))?),
    };
    verify_oci_archive(&archive).context("verify supplied OCI archive")?;
    Ok(vec![archive])
}
