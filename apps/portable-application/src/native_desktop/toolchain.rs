//! Managed Rust toolchain provisioning for a source-built native route.
//!
//! The flow is the one Nacelle already uses for a pinned language runtime:
//! read the official checksum, download the pinned artifact, verify it, install
//! it under Ato's own directory, and hand the caller the paths. What it never
//! does is the part the contract forbids:
//!
//! * no system installation and no `sudo`;
//! * no change to the user's `PATH`, shell profile or `~/.cargo`;
//! * no `latest` and no range — the declaration pins one exact version.
//!
//! Installing a toolchain is a decision, so it needs the caller's approval. The
//! approval is a run-time input, never a bundle fact: a bundle can require a
//! toolchain but cannot grant the right to install one.
//!
//! Everything a build receives is passed in the child environment
//! (`CARGO_HOME`, `RUSTUP_HOME`), so two builds with different pinned
//! versions cannot see each other's toolchain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ato_formation::native_desktop::RustToolchainSpec;

/// Where `rustup-init` is published for each target.
pub const RUSTUP_DIST: &str = "https://static.rust-lang.org/rustup/dist";

/// Whether this Run may install a managed toolchain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolchainConsent {
    /// The user approved installing under `@@ATO_HOME`.
    Approved,
    /// The user was asked and did not approve.
    #[default]
    Denied,
    /// There is no one to ask (a non-interactive Run).
    Unavailable,
}

/// Why a toolchain could not be provided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolchainFailure {
    #[error(
        "native_toolchain_consent_required: this route needs the managed Rust toolchain {spec}, which is not installed on this host. Re-run with the explicit approval to fetch it into {root}: a toolchain is never installed without one."
    )]
    ConsentRequired { spec: String, root: String },
    #[error(
        "native_toolchain_consent_unavailable: this route needs the managed Rust toolchain {spec}, which is not installed on this host, and this Run has no one to ask for approval to fetch it into {root}."
    )]
    ConsentUnavailable { spec: String, root: String },
    #[error(
        "native_toolchain_platform_unsupported: the declared os/arch has no Rust target triple"
    )]
    PlatformUnsupported,
    #[error("native_toolchain_install_failed: {0}")]
    Install(String),
    #[error("native_toolchain_incomplete: {0}")]
    Incomplete(String),
}

/// One installed, pinned toolchain and the environment a build must receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustToolchain {
    pub version: String,
    pub target: String,
    /// `@@ATO_HOME/toolchains/rust/<version>-<target>`.
    pub root: PathBuf,
    /// `root/cargo/bin/cargo` — always invoked by absolute path.
    pub cargo: PathBuf,
    /// The `CARGO_HOME` the build child receives.
    pub cargo_home: PathBuf,
    /// The `RUSTUP_HOME` the build child receives.
    pub rustup_home: PathBuf,
    /// The directory `cargo` finds its `rustc` in.
    pub bin_dir: PathBuf,
}

impl RustToolchain {
    /// The child-only environment overrides. The caller's own environment is
    /// never written, so no other process on this host sees this toolchain.
    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "CARGO_HOME".to_owned(),
                self.cargo_home.display().to_string(),
            ),
            (
                "RUSTUP_HOME".to_owned(),
                self.rustup_home.display().to_string(),
            ),
            (
                "RUSTUP_TOOLCHAIN".to_owned(),
                format!("{}-{}", self.version, self.target),
            ),
        ])
    }
}

/// The root a pinned toolchain installs into.
pub fn toolchain_root(home: &Path, spec: &RustToolchainSpec, target: &str) -> PathBuf {
    home.join("toolchains")
        .join("rust")
        .join(format!("{}-{target}", spec.version))
}

/// The environment any child of this provisioning receives.
fn installer_environment(root: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "CARGO_HOME".to_owned(),
            root.join("cargo").display().to_string(),
        ),
        (
            "RUSTUP_HOME".to_owned(),
            root.join("rustup").display().to_string(),
        ),
        // `rustup-init` installs the shims it needs; the managed bin directory
        // is on this child's PATH only so the installer can find what it just
        // wrote. The user's PATH and shell profile are never touched, and
        // `--no-modify-path` keeps the installer from trying.
        (
            "PATH".to_owned(),
            format!(
                "{}:{}",
                root.join("cargo/bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        ),
    ])
}

/// The exact `rustup-init` invocation for one toolchain.
pub fn install_plan(
    root: &Path,
    spec: &RustToolchainSpec,
    target: &str,
) -> (Vec<String>, BTreeMap<String, String>) {
    let args = vec![
        "--no-modify-path".to_owned(),
        "-y".to_owned(),
        "--profile".to_owned(),
        "minimal".to_owned(),
        "--default-toolchain".to_owned(),
        spec.version.clone(),
        "--target".to_owned(),
        target.to_owned(),
    ];
    (args, installer_environment(root))
}

/// A transport for the two small official artifacts provisioning needs.
pub trait ToolchainFetcher {
    /// Fetch `url` into `sink` and return the byte count.
    fn fetch(&self, url: &str, sink: &mut dyn std::io::Write) -> Result<u64, String>;
}

/// The provisioner that actually installs. Tests supply their own.
pub trait ToolchainProvisioner {
    /// Install the pinned toolchain under `root`.
    fn install(
        &self,
        root: &Path,
        spec: &RustToolchainSpec,
        target: &str,
        fetcher: &dyn ToolchainFetcher,
    ) -> Result<(), ToolchainFailure>;
}

/// Where `rustup-init` for one target is published.
pub fn rustup_init_url(target: &str) -> String {
    format!("{RUSTUP_DIST}/{target}/rustup-init")
}

/// Read the digest out of an official `.sha256` sibling file.
pub fn parse_sha256_text(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    (token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| token.to_ascii_lowercase())
}

/// The installed layout of one pinned toolchain, when it is complete.
pub fn inspect(home: &Path, spec: &RustToolchainSpec, target: &str) -> Option<RustToolchain> {
    let root = toolchain_root(home, spec, target);
    let cargo = root.join("cargo").join("bin").join("cargo");
    let rustc = root
        .join("rustup")
        .join("toolchains")
        .join(format!("{}-{target}", spec.version))
        .join("bin")
        .join("rustc");
    if !cargo.is_file() || !rustc.is_file() {
        return None;
    }
    Some(RustToolchain {
        version: spec.version.clone(),
        target: target.to_owned(),
        cargo_home: root.join("cargo"),
        rustup_home: root.join("rustup"),
        bin_dir: root.join("cargo").join("bin"),
        cargo,
        root,
    })
}

/// Provide the pinned toolchain, installing it only with approval.
///
/// A toolchain that is already installed needs no approval: nothing is
/// acquired, so there is no decision to make.
pub fn provide(
    home: &Path,
    spec: &RustToolchainSpec,
    target: Option<&str>,
    consent: ToolchainConsent,
    provisioner: &dyn ToolchainProvisioner,
    fetcher: &dyn ToolchainFetcher,
) -> Result<RustToolchain, ToolchainFailure> {
    let target = target.ok_or(ToolchainFailure::PlatformUnsupported)?;
    let root = toolchain_root(home, spec, target);
    if let Some(toolchain) = inspect(home, spec, target) {
        return Ok(toolchain);
    }
    match consent {
        ToolchainConsent::Approved => {}
        ToolchainConsent::Denied => {
            return Err(ToolchainFailure::ConsentRequired {
                spec: spec.as_str(),
                root: root.display().to_string(),
            });
        }
        ToolchainConsent::Unavailable => {
            return Err(ToolchainFailure::ConsentUnavailable {
                spec: spec.as_str(),
                root: root.display().to_string(),
            });
        }
    }
    provisioner.install(&root, spec, target, fetcher)?;
    inspect(home, spec, target).ok_or_else(|| {
        ToolchainFailure::Incomplete(format!(
            "the installer returned successfully but {} has no cargo and rustc",
            root.display()
        ))
    })
}

/// The production provisioner: verify the official installer, then run it with
/// `--no-modify-path` inside Ato's own directory.
pub struct RustupProvisioner;

impl ToolchainProvisioner for RustupProvisioner {
    fn install(
        &self,
        root: &Path,
        spec: &RustToolchainSpec,
        target: &str,
        fetcher: &dyn ToolchainFetcher,
    ) -> Result<(), ToolchainFailure> {
        std::fs::create_dir_all(root).map_err(|error| {
            ToolchainFailure::Install(format!("could not create {}: {error}", root.display()))
        })?;
        let url = rustup_init_url(target);
        let mut checksum_text = Vec::new();
        fetcher
            .fetch(&format!("{url}.sha256"), &mut checksum_text)
            .map_err(ToolchainFailure::Install)?;
        let text = String::from_utf8(checksum_text).map_err(|error| {
            ToolchainFailure::Install(format!("the .sha256 body is not UTF-8: {error}"))
        })?;
        let expected = parse_sha256_text(&text).ok_or_else(|| {
            ToolchainFailure::Install(format!("{url}.sha256 does not carry a sha256 digest"))
        })?;
        let binary = root.join("rustup-init");
        let _ = std::fs::remove_file(&binary);
        let mut bytes = Vec::new();
        fetcher
            .fetch(&url, &mut bytes)
            .map_err(ToolchainFailure::Install)?;
        let actual = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes));
        if actual != expected {
            let _ = std::fs::remove_file(&binary);
            return Err(ToolchainFailure::Install(format!(
                "the downloaded rustup-init is {actual}, the official checksum is {expected}"
            )));
        }
        std::fs::write(&binary, &bytes).map_err(|error| {
            ToolchainFailure::Install(format!("could not write {}: {error}", binary.display()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).map_err(
                |error| {
                    ToolchainFailure::Install(format!(
                        "could not make {} executable: {error}",
                        binary.display()
                    ))
                },
            )?;
        }
        let (args, environment) = install_plan(root, spec, target);
        let output = std::process::Command::new(&binary)
            .args(&args)
            .envs(&environment)
            .output()
            .map_err(|error| {
                ToolchainFailure::Install(format!("could not run {}: {error}", binary.display()))
            })?;
        let _ = std::fs::remove_file(&binary);
        if !output.status.success() {
            return Err(ToolchainFailure::Install(format!(
                "rustup-init exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(400)
                    .collect::<String>()
            )));
        }
        Ok(())
    }
}

/// Borrow the artifact transport for the two small official toolchain files.
///
/// Provisioning needs no second HTTP client: the same streaming transport that
/// fetches an artifact writes @@BT@@rustup-init@@BT@@ and its checksum, so there is one
/// place that opens a URL.
pub struct ArtifactToolchainFetcher<'a> {
    pub inner: &'a dyn super::remote::NativeFetcher,
}

impl ToolchainFetcher for ArtifactToolchainFetcher<'_> {
    fn fetch(&self, url: &str, sink: &mut dyn std::io::Write) -> Result<u64, String> {
        if !super::remote::valid_fetch_url(url) {
            return Err(format!("{url} is not an https URL"));
        }
        self.inner
            .fetch(url, sink)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoFetch;

    impl ToolchainFetcher for NoFetch {
        fn fetch(&self, _url: &str, _sink: &mut dyn std::io::Write) -> Result<u64, String> {
            Err("no transport in this test".to_owned())
        }
    }

    /// A provisioner that lays out a plausible toolchain, standing in for the
    /// real installer so the decision paths can be exercised without a network
    /// or a compiler.
    struct FakeInstall {
        layout: bool,
    }

    impl ToolchainProvisioner for FakeInstall {
        fn install(
            &self,
            root: &Path,
            spec: &RustToolchainSpec,
            target: &str,
            _fetcher: &dyn ToolchainFetcher,
        ) -> Result<(), ToolchainFailure> {
            if !self.layout {
                return Ok(());
            }
            let cargo = root.join("cargo").join("bin");
            let rustc = root
                .join("rustup")
                .join("toolchains")
                .join(format!("{}-{target}", spec.version))
                .join("bin");
            std::fs::create_dir_all(&cargo).unwrap();
            std::fs::create_dir_all(&rustc).unwrap();
            std::fs::write(cargo.join("cargo"), b"#!/bin/sh\n").unwrap();
            std::fs::write(rustc.join("rustc"), b"#!/bin/sh\n").unwrap();
            Ok(())
        }
    }

    fn spec() -> RustToolchainSpec {
        RustToolchainSpec::parse("rust:1.83.0").unwrap()
    }

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn the_toolchain_installs_under_the_ato_home_only() {
        let root = toolchain_root(Path::new("/home/u/.ato"), &spec(), "aarch64-apple-darwin");
        assert_eq!(
            root,
            PathBuf::from("/home/u/.ato/toolchains/rust/1.83.0-aarch64-apple-darwin")
        );
    }

    #[test]
    fn an_unapproved_run_refuses_to_install_and_says_why() {
        let home = home();
        let error = provide(
            home.path(),
            &spec(),
            Some("aarch64-apple-darwin"),
            ToolchainConsent::Denied,
            &FakeInstall { layout: true },
            &NoFetch,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("native_toolchain_consent_required"),
            "{message}"
        );
        assert!(message.contains("rust:1.83.0"), "{message}");
        assert!(
            message.contains("toolchains/rust/1.83.0-aarch64-apple-darwin"),
            "{message}"
        );
        assert!(
            !home
                .path()
                .join("toolchains/rust/1.83.0-aarch64-apple-darwin")
                .exists(),
            "a refused run must not install anything"
        );
    }

    #[test]
    fn a_non_interactive_run_gets_its_own_refusal() {
        let home = home();
        let error = provide(
            home.path(),
            &spec(),
            Some("aarch64-apple-darwin"),
            ToolchainConsent::Unavailable,
            &FakeInstall { layout: true },
            &NoFetch,
        )
        .unwrap_err();
        assert!(matches!(error, ToolchainFailure::ConsentUnavailable { .. }));
        assert!(error.to_string().contains("no one to ask"));
    }

    #[test]
    fn an_approved_run_installs_and_an_installed_one_needs_no_approval() {
        let home = home();
        let installed = provide(
            home.path(),
            &spec(),
            Some("aarch64-apple-darwin"),
            ToolchainConsent::Approved,
            &FakeInstall { layout: true },
            &NoFetch,
        )
        .unwrap();
        assert_eq!(installed.version, "1.83.0");
        assert_eq!(installed.target, "aarch64-apple-darwin");
        assert!(installed.cargo.is_file());

        // A second Run with no approval, on the same host, reuses it: nothing
        // is acquired, so there is no decision to ask about.
        let again = provide(
            home.path(),
            &spec(),
            Some("aarch64-apple-darwin"),
            ToolchainConsent::Denied,
            &FakeInstall { layout: false },
            &NoFetch,
        )
        .unwrap();
        assert_eq!(again.root, installed.root);
    }

    #[test]
    fn an_installer_that_leaves_no_toolchain_is_a_failure_not_a_success() {
        let home = home();
        let error = provide(
            home.path(),
            &spec(),
            Some("aarch64-apple-darwin"),
            ToolchainConsent::Approved,
            &FakeInstall { layout: false },
            &NoFetch,
        )
        .unwrap_err();
        assert!(matches!(error, ToolchainFailure::Incomplete(_)), "{error}");
    }

    #[test]
    fn a_platform_without_a_rust_target_is_refused_before_any_install() {
        let home = home();
        let error = provide(
            home.path(),
            &spec(),
            None,
            ToolchainConsent::Approved,
            &FakeInstall { layout: true },
            &NoFetch,
        )
        .unwrap_err();
        assert!(matches!(error, ToolchainFailure::PlatformUnsupported));
    }

    #[test]
    fn the_installer_plan_is_fixed_and_never_touches_the_user_environment() {
        let root = Path::new("/home/u/.ato/toolchains/rust/1.83.0-aarch64-apple-darwin");
        let (args, environment) = install_plan(root, &spec(), "aarch64-apple-darwin");
        assert_eq!(
            args,
            vec![
                "--no-modify-path",
                "-y",
                "--profile",
                "minimal",
                "--default-toolchain",
                "1.83.0",
                "--target",
                "aarch64-apple-darwin",
            ]
        );
        assert_eq!(
            environment.get("CARGO_HOME").map(String::as_str),
            Some("/home/u/.ato/toolchains/rust/1.83.0-aarch64-apple-darwin/cargo")
        );
        assert_eq!(
            environment.get("RUSTUP_HOME").map(String::as_str),
            Some("/home/u/.ato/toolchains/rust/1.83.0-aarch64-apple-darwin/rustup")
        );
        // The managed bin directory leads the child PATH; there is no HOME or
        // shell-profile entry, because the installer is told not to write one.
        assert!(
            environment
                .get("PATH")
                .unwrap()
                .starts_with("/home/u/.ato/toolchains/rust/1.83.0-aarch64-apple-darwin/cargo/bin:")
        );
        assert!(!environment.contains_key("HOME"));
        assert!(!environment.contains_key("SHELL"));
    }

    #[test]
    fn the_build_environment_is_child_scoped() {
        let home = home();
        let toolchain = provide(
            home.path(),
            &spec(),
            Some("aarch64-apple-darwin"),
            ToolchainConsent::Approved,
            &FakeInstall { layout: true },
            &NoFetch,
        )
        .unwrap();
        let environment = toolchain.environment();
        assert_eq!(environment.len(), 3, "{environment:?}");
        assert_eq!(
            environment.get("RUSTUP_TOOLCHAIN").map(String::as_str),
            Some("1.83.0-aarch64-apple-darwin")
        );
        assert!(!environment.contains_key("PATH"));
    }

    #[test]
    fn the_official_checksum_text_is_read_strictly() {
        assert_eq!(
            parse_sha256_text(&format!("{}  rustup-init\n", "a".repeat(64))),
            Some("a".repeat(64))
        );
        assert_eq!(
            parse_sha256_text(&format!("{}\n", "A".repeat(64))),
            Some("a".repeat(64))
        );
        assert_eq!(parse_sha256_text(""), None);
        assert_eq!(parse_sha256_text("not-a-digest  rustup-init\n"), None);
        assert_eq!(parse_sha256_text(&format!("{}\n", "a".repeat(63))), None);
        assert_eq!(
            rustup_init_url("aarch64-apple-darwin"),
            "https://static.rust-lang.org/rustup/dist/aarch64-apple-darwin/rustup-init"
        );
    }
}
