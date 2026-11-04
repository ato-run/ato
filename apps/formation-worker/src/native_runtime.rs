//! Operator-configured native Pixel Formation execution. Default is disabled.
//! No app, lease or authoring input may supply the trusted display helper.

use anyhow::Result;
#[cfg(unix)]
use anyhow::{Context, ensure};
use ato_portable_application::portable_run::{PortableBundleAttempt, PortableBundleAttemptOutcome};
use ato_runtime_attempt::journal::AttemptPermit;

#[cfg(unix)]
pub fn run(
    request: &PortableBundleAttempt<'_>,
    permit: Box<dyn AttemptPermit>,
) -> Result<PortableBundleAttemptOutcome> {
    use ato_connected_realization_worker::{
        NativePixelConfig,
        runtime_launch::native::formation::{private_surface_config, run_portable_native_attempt},
    };
    use sha2::{Digest, Sha256};
    use std::{io::Read, path::PathBuf};

    ensure!(
        std::env::var("ATO_FORMATION_NATIVE_PIXEL").as_deref() == Ok("1"),
        "native_surface_unavailable: native Formation is disabled"
    );
    let value = |name: &str| -> Result<String> {
        std::env::var(name)
            .ok()
            .filter(|s| !s.is_empty())
            .with_context(|| format!("native Formation requires {name}"))
    };
    let work_root = request
        .runtime_root
        .parent()
        .context("native Formation work root missing")?;
    let mut pixel = NativePixelConfig::default();
    pixel.enabled = true;
    pixel.helper_archive = Some(PathBuf::from(value("ATO_FORMATION_NATIVE_HELPER_ARCHIVE")?));
    pixel.helper_image = Some(value("ATO_FORMATION_NATIVE_HELPER_IMAGE")?);
    pixel.cgroup_parent = Some(value("ATO_FORMATION_NATIVE_CGROUP_PARENT")?);
    let optional_number = |name: &str, default: u64| -> Result<u64> {
        match std::env::var(name) {
            Ok(value) => value.parse().with_context(|| format!("invalid {name}")),
            Err(std::env::VarError::NotPresent) => Ok(default),
            Err(error) => Err(error).with_context(|| format!("invalid {name}")),
        }
    };
    pixel.helper_memory_mib = optional_number(
        "ATO_FORMATION_NATIVE_HELPER_MEMORY_MIB",
        pixel.helper_memory_mib,
    )?;
    pixel.helper_cpu_millis = optional_number(
        "ATO_FORMATION_NATIVE_HELPER_CPU_MILLIS",
        pixel.helper_cpu_millis,
    )?;
    pixel.helper_pids = optional_number("ATO_FORMATION_NATIVE_HELPER_PIDS", pixel.helper_pids)?;
    pixel.prepare(work_root, 1)?;
    let helper = pixel.helper(work_root)?;
    if request.authorization
        == ato_runtime_attempt::admission::EffectAuthorization::PrivateIsolatedFormation
    {
        helper.verify_private_formation_boundary()?;
    }

    let archive_file = std::fs::File::open(value("ATO_FORMATION_NATIVE_APP_ARCHIVE")?)?;
    let max = (ato_portable_application::oci_archive::MAX_ARCHIVE_BYTES as u64 * 4 / 3) + 4096;
    let archive_size = archive_file.metadata()?.len();
    ensure!(
        archive_size <= max,
        "native application archive exceeds transport bound"
    );
    helper.admit_archive_loading(archive_size)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(usize::try_from(archive_size)?)?;
    archive_file
        .take(archive_size + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == archive_size,
        "native application archive changed during read"
    );
    let archive: ato_objects::PortableOciArchive =
        serde_json::from_slice(&bytes).context("invalid native application archive")?;
    drop(bytes);

    let route = ato_portable_application::validate_bundle_for_derivation(
        request.bundle,
        request.derivation_ref,
    )?;
    let surface = route
        .application
        .surfaces
        .first()
        .context("native application surface missing")?;
    let runner_hash = format!("{:x}", Sha256::digest(request.runtime_id.as_bytes()));
    let owner = ato_adapter_oci::OciOwner {
        runner_id: format!("formation-{}", &runner_hash[..24]),
        slot_id: "native-pixel".into(),
        lease_id: request.attempt_id.to_owned(),
        run_id: format!("formation-{}", request.attempt_id),
        incarnation: ato_connected_realization_worker::runtime_launch::recovery::incarnation()
            .to_owned(),
    };
    let config = private_surface_config(&owner.run_id, &surface.id)?;
    run_portable_native_attempt(request, owner, helper, archive, config, permit)
}

#[cfg(not(unix))]
pub fn run(
    _request: &PortableBundleAttempt<'_>,
    _permit: Box<dyn AttemptPermit>,
) -> Result<PortableBundleAttemptOutcome> {
    anyhow::bail!("native_surface_unavailable: native Formation requires Linux")
}
