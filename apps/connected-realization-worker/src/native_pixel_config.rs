//! Trusted Runner materialization settings. Never accepted from a lease,
//! Capsule, request, or environment inside a workload.
#[cfg(unix)]
use anyhow::Context;
use anyhow::{Result, ensure};
use std::{
    fmt,
    path::{Path, PathBuf},
};

#[derive(Clone, clap::Args)]
pub struct NativePixelConfig {
    #[arg(
        long = "native-pixel",
        env = "ATO_RUNNER_NATIVE_PIXEL",
        default_value_t = false
    )]
    pub enabled: bool,
    /// Trusted operator opt-in; never read from a lease or application input.
    #[arg(
        long = "native-pixel-text-paste",
        env = "ATO_NATIVE_PIXEL_TEXT_PASTE",
        default_value_t = false
    )]
    pub text_paste: bool,
    #[arg(
        long = "native-pixel-framebuffer-resize",
        env = "ATO_NATIVE_PIXEL_FRAMEBUFFER_RESIZE",
        default_value_t = false
    )]
    pub framebuffer_resize: bool,
    #[arg(
        long = "native-pixel-helper-archive",
        env = "ATO_NATIVE_PIXEL_HELPER_ARCHIVE"
    )]
    pub helper_archive: Option<PathBuf>,
    #[arg(
        long = "native-pixel-helper-image",
        env = "ATO_NATIVE_PIXEL_HELPER_IMAGE"
    )]
    pub helper_image: Option<String>,
    /// Existing cgroup v2 parent applied to both physical containers.
    #[arg(
        long = "native-pixel-cgroup-parent",
        env = "ATO_NATIVE_PIXEL_CGROUP_PARENT"
    )]
    pub cgroup_parent: Option<String>,
    #[arg(
        long,
        env = "ATO_NATIVE_PIXEL_HELPER_MEMORY_MIB",
        default_value_t = 256
    )]
    pub helper_memory_mib: u64,
    #[arg(
        long,
        env = "ATO_NATIVE_PIXEL_HELPER_CPU_MILLIS",
        default_value_t = 500
    )]
    pub helper_cpu_millis: u64,
    #[arg(
        long = "native-pixel-helper-pids",
        env = "ATO_NATIVE_PIXEL_HELPER_PIDS",
        default_value_t = 32
    )]
    pub helper_pids: u64,
    #[cfg(unix)]
    #[arg(skip)]
    prepared: Option<crate::runtime_launch::native::pixel::PixelHelper>,
}
impl Default for NativePixelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            text_paste: false,
            framebuffer_resize: false,
            helper_archive: None,
            helper_image: None,
            cgroup_parent: None,
            helper_memory_mib: 256,
            helper_cpu_millis: 500,
            helper_pids: 32,
            #[cfg(unix)]
            prepared: None,
        }
    }
}
impl fmt::Debug for NativePixelConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativePixelConfig")
            .field("enabled", &self.enabled)
            .field("helper_image", &self.helper_image)
            .finish_non_exhaustive()
    }
}
impl NativePixelConfig {
    pub(crate) fn prepare(&mut self, work_root: &Path, slots: u32) -> Result<()> {
        ensure!(
            self.enabled || (!self.text_paste && !self.framebuffer_resize),
            "native_surface_controls_unsupported: controls require enabled Native Pixel"
        );
        if !self.enabled {
            return Ok(());
        }
        ensure!(
            cfg!(target_os = "linux") && slots == 1,
            "Native Pixel requires one owned local Linux slot"
        );
        #[cfg(unix)]
        {
            use crate::runtime_launch::native::pixel::PixelHelper;
            use std::io::Read;
            ensure_local_recovery_endpoint()?;
            let path = self
                .helper_archive
                .as_ref()
                .context("Native Pixel requires a trusted helper archive")?;
            let expected = self
                .helper_image
                .as_deref()
                .context("Native Pixel requires an exact helper image reference")?;
            let mut bytes = Vec::new();
            const MAX: u64 = 256 * 1024 * 1024;
            std::fs::File::open(path)?
                .take(MAX + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= MAX,
                "Native helper archive input exceeds configuration bound"
            );
            let archive: ato_objects::PortableOciArchive = serde_json::from_slice(&bytes)
                .context("invalid trusted helper archive transport")?;
            ensure!(
                archive.image == expected,
                "trusted helper differs from configured immutable image"
            );
            let helper = PixelHelper::from_archive_with_controls(
                archive,
                ato_adapter_oci::OciResourceLimits {
                    memory_bytes: self
                        .helper_memory_mib
                        .checked_mul(1024 * 1024)
                        .context("helper memory overflow")?,
                    cpu_limit_millis: self.helper_cpu_millis,
                    pids_limit: self.helper_pids,
                },
                self.requested_controls(),
            )?
            .with_runtime_user(work_root)?
            .with_local_owner(
                PathBuf::from("/var/run/docker.sock"),
                self.cgroup_parent
                    .clone()
                    .context("Native Pixel requires an explicit existing cgroup parent")?,
            )?;
            // A missing warm cache does not prevent existing label recovery.
            // It does prevent advertisement and every new Pixel attempt.
            if let Err(error) = helper.check_ready(work_root) {
                eprintln!("[runtime-launch] Native Pixel unavailable: {error:#}");
            }
            self.prepared = Some(helper);
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = work_root;
            anyhow::bail!("Native Pixel requires Linux")
        }
    }
    #[cfg(unix)]
    fn requested_controls(&self) -> netd::pixel_gateway::PixelGatewayControls {
        netd::pixel_gateway::PixelGatewayControls {
            text_paste: self.text_paste,
            framebuffer_resize: self.framebuffer_resize,
        }
    }
    pub(crate) fn prepared_controls(&self) -> netd::pixel_gateway::PixelGatewayControls {
        #[cfg(unix)]
        {
            self.prepared
                .as_ref()
                .map(|helper| helper.controls())
                .unwrap_or_default()
        }
        #[cfg(not(unix))]
        {
            netd::pixel_gateway::PixelGatewayControls::default()
        }
    }
    pub(crate) fn available(&self, work_root: &Path, oci: bool) -> bool {
        if !self.enabled || !oci || !cfg!(target_os = "linux") {
            return false;
        }
        #[cfg(unix)]
        {
            ensure_local_recovery_endpoint().is_ok()
                && self
                    .prepared
                    .as_ref()
                    .is_some_and(|helper| helper.check_ready(work_root).is_ok())
        }
        #[cfg(not(unix))]
        {
            let _ = work_root;
            false
        }
    }
    #[cfg(unix)]
    pub(crate) fn prepared_helper(
        &self,
    ) -> Option<crate::runtime_launch::native::pixel::PixelHelper> {
        self.prepared.clone()
    }
    #[cfg(unix)]
    pub(crate) fn helper(
        &self,
        work_root: &Path,
    ) -> Result<crate::runtime_launch::native::pixel::PixelHelper> {
        ensure!(
            self.enabled,
            "native_surface_unavailable: Pixel is disabled"
        );
        ensure_local_recovery_endpoint()?;
        let helper = self
            .prepared
            .as_ref()
            .context("native_surface_unavailable: Pixel helper is not verified")?;
        helper.check_ready(work_root)?;
        Ok(helper.clone())
    }
}

/// Label recovery still uses the existing ambient owner client. Limit this
/// first backend to the exact default local daemon so recovery cannot drift
/// to a different daemon from the explicitly bound launch handles.
#[cfg(unix)]
fn ensure_local_recovery_endpoint() -> Result<()> {
    for name in [
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        ensure!(
            std::env::var_os(name).is_none(),
            "Native Pixel does not admit ambient Docker endpoint overrides"
        );
    }
    let output = ato_adapter_oci::command_control::CommandControl::cleanup(
        std::time::Duration::from_millis(500),
    )
    .output_with_limit(
        std::process::Command::new("docker").args([
            "context",
            "inspect",
            "--format",
            "{{json .Endpoints.docker.Host}}",
        ]),
        4096,
    )?;
    ensure!(output.status.success(), "unknown Docker recovery endpoint");
    let endpoint: String =
        serde_json::from_slice(&output.stdout).context("invalid Docker recovery endpoint")?;
    let socket = endpoint
        .strip_prefix("unix://")
        .context("Native Pixel requires local Unix Docker recovery")?;
    ensure!(
        std::fs::canonicalize(socket)? == std::fs::canonicalize("/var/run/docker.sock")?,
        "Native Pixel recovery daemon differs from its launch daemon"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_controls_are_explicit_operator_options() {
        use clap::Parser;
        #[derive(clap::Parser)]
        struct OperatorArgs {
            #[command(flatten)]
            pixel: NativePixelConfig,
        }
        assert!(OperatorArgs::try_parse_from(["worker", "--native-pixel-text-paste"]).is_ok());
        assert!(
            OperatorArgs::try_parse_from(["worker", "--native-pixel-framebuffer-resize"]).is_ok()
        );
        assert!(
            OperatorArgs::try_parse_from(["worker", "--native-pixel-text-paste=arbitrary"])
                .is_err()
        );
    }
    #[test]
    fn controls_default_off_and_disabled_pixel_cannot_silently_ignore_opt_in() {
        let mut config = NativePixelConfig::default();
        assert!(!config.text_paste && !config.framebuffer_resize);
        config.text_paste = true;
        let error = config
            .prepare(Path::new("missing-helper-root"), 1)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("native_surface_controls_unsupported:")
        );
    }
    #[test]
    fn disabled_or_unprepared_pixel_never_advertises_or_supplies_a_helper() {
        let root = Path::new("missing-native-fixture-root");
        let mut config = NativePixelConfig::default();
        assert!(!config.available(root, true));
        #[cfg(unix)]
        assert!(config.helper(root).is_err());
        config.enabled = true;
        assert!(!config.available(root, true));
        assert!(!config.available(root, false));
        assert!(config.prepare(root, 2).is_err());
        assert!(!format!("{config:?}").contains("archive"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires real local Linux Docker, existing cgroup v2 parent, and explicit verified warm helper archive"]
    fn actual_local_parent_and_warm_helper_preflight_refuses_aggregate_overcommit() {
        let root = tempfile::tempdir().unwrap();
        let mut config = NativePixelConfig {
            enabled: true,
            helper_archive: Some(
                std::env::var_os("ATO_NATIVE_PIXEL_HELPER_ARCHIVE")
                    .unwrap()
                    .into(),
            ),
            helper_image: Some(std::env::var("ATO_NATIVE_PIXEL_HELPER_IMAGE").unwrap()),
            cgroup_parent: Some(
                std::env::var("ATO_NATIVE_PIXEL_TEST_CGROUP_PARENT")
                    .unwrap_or_else(|_| "system.slice".into()),
            ),
            ..Default::default()
        };
        config.prepare(root.path(), 1).unwrap();
        assert!(config.available(root.path(), true));
        let helper = config.helper(root.path()).unwrap();
        let (mut spec, _, _, _) = crate::runtime_launch::native::tests::fixture(&format!(
            "example.invalid/app@sha256:{}",
            "11".repeat(32)
        ));
        helper.admit_spec(&spec).unwrap();
        let ato_ipc::runtime_launch::LaunchRealizationV1::Oci(oci) = &mut spec.realization else {
            unreachable!()
        };
        oci.resource_limits.as_mut().unwrap().memory_bytes = u64::MAX;
        assert!(helper.admit_spec(&spec).is_err());
        assert!(
            std::fs::read_dir(root.path()).unwrap().next().is_none(),
            "preflight must not create workload resources or retained config"
        );
    }
}
