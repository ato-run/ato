//! The `ato.native@1` step protocol: an Adapter-level realization of one
//! Digestion as a digest-pinned desktop executable.
//!
//! A native route is not a new semantic noun. Its facts live in the serving
//! step's `runtimes` map (so every Derivation formed before this protocol
//! existed keeps its exact bytes) and its logical Ports reuse `BoundPort`.
//! This module only parses and checks that declaration; it never launches
//! anything, and nothing here is host-specific: platform admission is decided
//! against observed host facts by the runtime, not by the bundle validator.
//!
//! Exactly one artifact source is declared, and the three are mutually
//! exclusive:
//!
//! * **bundled** — the archive is a blob in the bundle closure.
//! * **remote** — the archive is a digest-pinned reference to an upstream
//!   host. Ato is not the distributor; the Run fetches and verifies it.
//! * **source** — a commit-pinned source tarball is built by a managed
//!   toolchain with a fixed `cargo` build command.
//!
//! ```text
//! [[derivation]]
//! id = "prebuilt-bundle"
//! use = "ato.native@1"
//! os = "macos"
//! arch = "aarch64"
//! entry = "app/run.sh"
//! artifact = "dist/app-macos.tar.gz"
//! ```
//!
//! which binds to
//!
//! ```text
//! BoundStep {
//!     protocol: "ato.native@1",
//!     op: "serve",
//!     runtimes: {
//!         "ato.native.os": "macos",
//!         "ato.native.arch": "aarch64",
//!         "ato.native.artifact": "sha256:<blob>",
//!         "ato.native.entry": "app/run.sh",
//!         "ato.native.format": "tar.gz",            // optional, default
//!         "ato.native.signature": "require-valid",  // optional, default
//!         "ato.native.signature_target": "bundle",  // optional, default executable
//!     },
//! }
//! ```
//!
//! A remote artifact replaces `ato.native.artifact` with the three
//! `ato.native.remote_*` facts; a source build replaces it with the seven
//! `ato.native.source_*`, `cargo_lock`, `toolchain`, `build_argv` and
//! `build_cache` facts. Declaring two sources, or only part of one, is
//! refused: there is no precedence to infer and no default to complete.

use crate::authoring::BoundStep;

/// The step protocol a native desktop route's one serving step declares.
pub const NATIVE_PROTOCOL: &str = "ato.native@1";
/// Every native fact is a `runtimes` entry with this prefix. A native step's
/// map is closed: a key this build does not define fails the route.
pub const NATIVE_RUNTIME_PREFIX: &str = "ato.native.";
pub const NATIVE_OS_RUNTIME: &str = "ato.native.os";
pub const NATIVE_ARCH_RUNTIME: &str = "ato.native.arch";
pub const NATIVE_ARTIFACT_RUNTIME: &str = "ato.native.artifact";
pub const NATIVE_ENTRY_RUNTIME: &str = "ato.native.entry";
pub const NATIVE_FORMAT_RUNTIME: &str = "ato.native.format";
pub const NATIVE_SIGNATURE_RUNTIME: &str = "ato.native.signature";
/// Which filesystem object the signature policy is verified against.
pub const NATIVE_SIGNATURE_TARGET_RUNTIME: &str = "ato.native.signature_target";
/// The fixed argument vector a Terminal route launches its executable with.
///
/// A Terminal route is the one route whose program must be addressed (a CLI
/// answers on stdin only in the mode its arguments select). The vector is a
/// bounded, declared fact — the Adapter executes it directly, never through a
/// shell — and only a route that declares a terminal Surface may state it.
pub const NATIVE_ARGV_RUNTIME: &str = "ato.native.argv";
/// Largest number of arguments a Terminal route may declare.
pub const MAX_NATIVE_ARGV: usize = 16;
/// Largest single argument a Terminal route may declare, in bytes.
pub const MAX_NATIVE_ARGV_BYTES: usize = 256;
/// An upstream URL the Run fetches instead of a bundle-carried archive.
pub const NATIVE_REMOTE_URL_RUNTIME: &str = "ato.native.remote_url";
/// The `sha256:` digest the fetched bytes must equal.
pub const NATIVE_REMOTE_SHA256_RUNTIME: &str = "ato.native.remote_sha256";
/// The exact byte count the fetched body must have.
pub const NATIVE_REMOTE_BYTES_RUNTIME: &str = "ato.native.remote_bytes";
/// The commit-pinned source tarball a source build starts from.
pub const NATIVE_SOURCE_URL_RUNTIME: &str = "ato.native.source_url";
pub const NATIVE_SOURCE_COMMIT_RUNTIME: &str = "ato.native.source_commit";
pub const NATIVE_SOURCE_SHA256_RUNTIME: &str = "ato.native.source_sha256";
/// The `sha256:` digest of the `Cargo.lock` the build must consume.
pub const NATIVE_CARGO_LOCK_RUNTIME: &str = "ato.native.cargo_lock";
/// The managed Rust toolchain the build requires (`rust:<version>`).
pub const NATIVE_TOOLCHAIN_RUNTIME: &str = "ato.native.toolchain";
/// The fixed-vocabulary build command. Never a general shell command.
pub const NATIVE_BUILD_ARGV_RUNTIME: &str = "ato.native.build_argv";
/// How build outputs are cached.
pub const NATIVE_BUILD_CACHE_RUNTIME: &str = "ato.native.build_cache";

/// The packed-archive format a bundle-carried or source artifact uses.
pub const NATIVE_FORMAT_TAR_GZ: &str = "tar.gz";
/// A `.zip` archive. Accepted for a remote artifact only.
pub const NATIVE_FORMAT_ZIP: &str = "zip";
/// A `.dmg` disk image. Accepted for a remote artifact only; the Adapter
/// has no disk-image extractor in this increment and refuses it at execution.
pub const NATIVE_FORMAT_DMG: &str = "dmg";

/// The one build cache policy a source route may declare.
pub const NATIVE_BUILD_CACHE_CONTENT_HASH: &str = "content-hash";

/// Hosts a source build may reach at build time. A declared build-phase
/// network requirement outside this list fails the route, so a source route
/// cannot turn the build sandbox into a general egress channel. The build
/// sandbox enforces HTTPS (tcp/443); host-name filtering is not available in
/// the SBPL grant this build renders, and the receipt says so.
pub const NATIVE_BUILD_EGRESS_HOSTS: &[&str] = &[
    "crates.io",
    "index.crates.io",
    "static.crates.io",
    "github.com",
    "codeload.github.com",
    "objects.githubusercontent.com",
];

/// Largest extracted artifact a bundle may carry. The raw archive is already
/// covered by the bundle's closure digest; this bounds the one blob a native
/// route adds to it.
pub const MAX_NATIVE_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
/// Largest remote artifact a Run may fetch. A desktop application's disk image
/// is a few hundred megabytes; the bound keeps a declaration from naming an
/// unbounded download.
pub const MAX_NATIVE_REMOTE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Largest URL this build accepts.
pub const MAX_NATIVE_REMOTE_URL_BYTES: usize = 2048;
/// Largest entry path this build accepts.
pub const MAX_NATIVE_ENTRY_BYTES: usize = 1024;
/// Largest cargo package name this build accepts.
pub const MAX_NATIVE_CARGO_PACKAGE_BYTES: usize = 64;

/// A stable, machine-readable descriptor refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NativeDescriptorError(pub &'static str);

/// The declared operating system, in the same vocabulary host facts use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeOs {
    Macos,
    Windows,
}

impl NativeOs {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "macos" => Some(Self::Macos),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// The declared architecture. `HostArch` already names both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NativeArch {
    X86_64,
    Aarch64,
}

impl NativeArch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "x86_64" => Some(Self::X86_64),
            "aarch64" => Some(Self::Aarch64),
            _ => None,
        }
    }

    /// The Rust target triple this `os/arch` pair denotes.
    pub fn rust_target(self, os: NativeOs) -> Option<&'static str> {
        match (os, self) {
            (NativeOs::Macos, Self::Aarch64) => Some("aarch64-apple-darwin"),
            (NativeOs::Macos, Self::X86_64) => Some("x86_64-apple-darwin"),
            (NativeOs::Windows, Self::X86_64) => Some("x86_64-pc-windows-msvc"),
            (NativeOs::Windows, Self::Aarch64) => Some("aarch64-pc-windows-msvc"),
        }
    }
}

/// How the artifact is packed.
///
/// A bundle-carried artifact and a source archive are `tar.gz`; a remote
/// reference may also name `zip` or `dmg`. An unknown value is refused rather
/// than treated as an opaque blob the runtime could not have extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeArtifactFormat {
    TarGz,
    Zip,
    Dmg,
}

impl NativeArtifactFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TarGz => NATIVE_FORMAT_TAR_GZ,
            Self::Zip => NATIVE_FORMAT_ZIP,
            Self::Dmg => NATIVE_FORMAT_DMG,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            NATIVE_FORMAT_TAR_GZ => Some(Self::TarGz),
            NATIVE_FORMAT_ZIP => Some(Self::Zip),
            NATIVE_FORMAT_DMG => Some(Self::Dmg),
            _ => None,
        }
    }
}

/// What signature the route requires before launch.
///
/// `RequireValid` is the default and fails closed. The override is an explicit
/// local decision that is recorded as receipt evidence; it never relaxes
/// filesystem, state, egress or lifecycle policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSignaturePolicy {
    RequireValid,
    RequireValidOrLocalOverride,
}

impl NativeSignaturePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequireValid => "require-valid",
            Self::RequireValidOrLocalOverride => "require-valid-or-local-override",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "require-valid" => Some(Self::RequireValid),
            "require-valid-or-local-override" => Some(Self::RequireValidOrLocalOverride),
            _ => None,
        }
    }

    pub fn allows_local_override(self) -> bool {
        self == Self::RequireValidOrLocalOverride
    }
}

/// What the signature policy is verified against.
///
/// `Executable` is the pre-existing behavior and the default, so a Derivation
/// that says nothing keeps its exact bytes. `Bundle` verifies the `.app`
/// directory that contains the entry: a bundle's signature covers its
/// contents, so "the entry is signed" is not "the application the Run launches
/// is signed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSignatureTarget {
    Executable,
    Bundle,
}

impl NativeSignatureTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::Bundle => "bundle",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "executable" => Some(Self::Executable),
            "bundle" => Some(Self::Bundle),
            _ => None,
        }
    }

    pub fn is_bundle(self) -> bool {
        self == Self::Bundle
    }
}

/// One digest- and size-pinned upstream archive.
///
/// The digest is the identity: a changed or removed asset is a refusal, never
/// a silent upgrade. `bytes` is checked against the response before the
/// body is trusted, so a truncated or substituted response cannot be mistaken
/// for the artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteArtifact {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

impl RemoteArtifact {
    pub fn new(url: &str, sha256: &str, bytes: u64) -> Result<Self, NativeDescriptorError> {
        if !valid_native_remote_url(url) {
            return Err(NativeDescriptorError("native_remote_url_invalid"));
        }
        if !is_sha256_ref(sha256) {
            return Err(NativeDescriptorError("native_remote_sha256_invalid"));
        }
        if bytes == 0 || bytes > MAX_NATIVE_REMOTE_BYTES {
            return Err(NativeDescriptorError("native_remote_bytes_invalid"));
        }
        Ok(Self {
            url: url.to_owned(),
            sha256: sha256.to_owned(),
            bytes,
        })
    }
}

/// A pinned Rust toolchain a source build requires.
///
/// The version is exact; there is no "latest" and no range. The target triple
/// is taken from the route's declared `os/arch`, so the toolchain and the
/// artifact cannot disagree about the platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustToolchainSpec {
    pub version: String,
}

impl RustToolchainSpec {
    pub const PREFIX: &'static str = "rust:";

    pub fn new(version: &str) -> Result<Self, NativeDescriptorError> {
        if !valid_rust_version(version) {
            return Err(NativeDescriptorError("native_toolchain_invalid"));
        }
        Ok(Self {
            version: version.to_owned(),
        })
    }

    pub fn parse(value: &str) -> Option<Self> {
        let version = value.strip_prefix(Self::PREFIX)?;
        valid_rust_version(version).then(|| Self {
            version: version.to_owned(),
        })
    }

    pub fn as_str(&self) -> String {
        format!("{}{}", Self::PREFIX, self.version)
    }
}

fn valid_rust_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch].iter().all(|part| {
        !part.is_empty()
            && part.len() <= 6
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && !(part.len() > 1 && part.starts_with('0'))
    })
}

/// The one build command vocabulary a source route may declare.
///
/// It is a fixed `cargo build` invocation: `--locked` and `--release` are
/// mandatory, an optional `-p <package>` may pin one package, and no other
/// token exists. There is no shell, no environment prefix, no `--target`, no
/// feature flags and no arbitrary argument, so a declaration can never become
/// a general command channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoBuildInvocation {
    /// The package to build, when the declaration pins one.
    pub package: Option<String>,
}

impl CargoBuildInvocation {
    /// The tokens after the `cargo` binary.
    pub const FIXED_ARGS: [&'static str; 3] = ["build", "--locked", "--release"];

    pub fn new(package: Option<&str>) -> Result<Self, NativeDescriptorError> {
        if package.is_some_and(|package| !valid_cargo_package(package)) {
            return Err(NativeDescriptorError("native_build_argv_invalid"));
        }
        Ok(Self {
            package: package.map(str::to_owned),
        })
    }

    /// Parse the canonical spelling. Non-canonical spacing is accepted but
    /// normalized, so one invocation has exactly one byte form.
    pub fn parse(value: &str) -> Option<Self> {
        let tokens = value
            .split(' ')
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if tokens.len() < 1 + Self::FIXED_ARGS.len() || tokens[0] != "cargo" {
            return None;
        }
        if tokens[1..1 + Self::FIXED_ARGS.len()] != Self::FIXED_ARGS {
            return None;
        }
        match &tokens[1 + Self::FIXED_ARGS.len()..] {
            [] => Some(Self { package: None }),
            ["-p", package] if valid_cargo_package(package) => Some(Self {
                package: Some((*package).to_owned()),
            }),
            _ => None,
        }
    }

    /// The canonical `runtimes` value.
    pub fn as_str(&self) -> String {
        match &self.package {
            None => format!("cargo {}", Self::FIXED_ARGS.join(" ")),
            Some(package) => format!("cargo {} -p {package}", Self::FIXED_ARGS.join(" ")),
        }
    }

    /// The argument vector to run. Built from this value, never from the
    /// declared string.
    pub fn command(&self) -> Vec<String> {
        let mut command = vec!["cargo".to_owned()];
        command.extend(Self::FIXED_ARGS.iter().map(|arg| (*arg).to_owned()));
        if let Some(package) = &self.package {
            command.push("-p".to_owned());
            command.push(package.clone());
        }
        command
    }
}

fn valid_cargo_package(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NATIVE_CARGO_PACKAGE_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && !value.starts_with('-')
}

/// How the built output is cached. One policy exists in this increment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeBuildCachePolicy {
    ContentHash,
}

impl NativeBuildCachePolicy {
    pub fn as_str(self) -> &'static str {
        NATIVE_BUILD_CACHE_CONTENT_HASH
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            NATIVE_BUILD_CACHE_CONTENT_HASH => Some(Self::ContentHash),
            _ => None,
        }
    }
}

/// A commit-pinned source build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBuild {
    /// The commit-pinned source tarball.
    pub url: String,
    /// The commit the tarball must represent.
    pub commit: String,
    /// The `sha256:` digest of the source tarball bytes.
    pub sha256: String,
    /// The `sha256:` digest of the `Cargo.lock` in the extracted source.
    pub cargo_lock: String,
    pub toolchain: RustToolchainSpec,
    pub build: CargoBuildInvocation,
    pub cache: NativeBuildCachePolicy,
}

/// Where the selected route's artifact comes from.
///
/// Exactly one variant is declared. Nothing infers, falls back or prefers one
/// source over another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArtifactSource {
    /// A blob the bundle carries.
    Bundled { artifact: String },
    /// A digest-pinned upstream reference.
    Remote(RemoteArtifact),
    /// A commit-pinned source build.
    Source(Box<SourceBuild>),
}

impl NativeArtifactSource {
    /// `bundled`, `remote` or `source`, for receipts and admission.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bundled { .. } => "bundled",
            Self::Remote(_) => "remote",
            Self::Source(_) => "source",
        }
    }

    /// The bundled blob reference, when the artifact is carried in the bundle.
    pub fn bundled_artifact(&self) -> Option<&str> {
        match self {
            Self::Bundled { artifact } => Some(artifact),
            Self::Remote(_) | Self::Source(_) => None,
        }
    }
}

/// Parse the declared argument vector of a Terminal route.
///
/// The Adapter executes this vector directly with the resolved executable and
/// never through a shell, so each argument is bounded and restricted to
/// printable ASCII: nothing in it can become a shell word, a redirection or a
/// path the route did not declare.
fn parse_native_argv(value: &str) -> Result<Vec<String>, NativeDescriptorError> {
    let argv: Vec<String> =
        serde_json::from_str(value).map_err(|_| NativeDescriptorError("native_argv_invalid"))?;
    if argv.len() > MAX_NATIVE_ARGV {
        return Err(NativeDescriptorError("native_argv_too_long"));
    }
    for argument in &argv {
        if argument.is_empty()
            || argument.len() > MAX_NATIVE_ARGV_BYTES
            || !argument
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(NativeDescriptorError("native_argv_invalid"));
        }
    }
    Ok(argv)
}

/// The validated declaration of one native serving step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDescriptor {
    pub os: NativeOs,
    pub arch: NativeArch,
    /// The executable path, relative to the materialized artifact root.
    pub entry: String,
    pub format: NativeArtifactFormat,
    pub signature: NativeSignaturePolicy,
    /// What the signature verdict is about. `Executable` unless declared.
    pub signature_target: NativeSignatureTarget,
    /// Where the artifact comes from.
    pub source: NativeArtifactSource,
    /// Arguments the executable is launched with. Empty unless declared, and
    /// only a Terminal route may declare a non-empty vector.
    pub argv: Vec<String>,
}

fn is_sha256_ref(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// A commit reference: 40 hex (SHA-1) or 64 hex (SHA-256), lowercase.
pub fn valid_git_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// An absolute `https` URL with a host and no fragment.
///
/// The fetch layer additionally accepts loopback `http` so the transport can be
/// exercised on a local server; a *declaration* never does. A fragment is
/// refused because it is not sent to the server and would make two
/// declarations over the same bytes compare unequal.
pub fn valid_native_remote_url(value: &str) -> bool {
    if value.is_empty()
        || value.len() > MAX_NATIVE_REMOTE_URL_BYTES
        || value.contains(['\0', '\n', '\r', '#', ' '])
    {
        return false;
    }
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let Some((host, _path)) = split_url_host(rest) else {
        return false;
    };
    !host.is_empty()
        && !host.starts_with('.')
        && !host.contains(['@', '?', '\\'])
        && !rest.contains('\\')
}

/// Split a URL body after the scheme into its authority and path.
pub fn split_url_host(rest: &str) -> Option<(&str, &str)> {
    match rest.find('/') {
        Some(index) => Some((&rest[..index], &rest[index..])),
        None if rest.is_empty() => None,
        None => Some((rest, "")),
    }
}

/// A relative, traversal-free path inside the materialized artifact root.
///
/// A leading `/`, a parent-directory segment, an empty segment, a backslash
/// and a colon (`drive:` and `stream:name` on Windows) are refused: the runtime
/// joins this value under the materialized root, so accepting any of them
/// would let the archive name a location outside it.
pub fn valid_native_entry(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NATIVE_ENTRY_BYTES
        && !value.starts_with('/')
        && !value.contains(['\0', '\\', ':'])
        && value
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// The `.app` directory that contains the entry, when there is one.
///
/// This is the outermost path segment ending in `.app`; the entry itself must
/// be a file inside it, so the segment cannot be the last one. The other
/// unsafe forms are already refused by `[valid_native_entry]`, which is what
/// keeps the result inside the artifact root.
pub fn native_bundle_root(entry: &str) -> Option<&str> {
    let mut offset = 0;
    for segment in entry.split('/') {
        let segment_end = offset + segment.len();
        if segment.len() > ".app".len() && segment.ends_with(".app") && segment_end < entry.len() {
            return Some(&entry[..segment_end]);
        }
        offset = segment_end + 1;
    }
    None
}

fn required<'a>(
    runtimes: &'a std::collections::BTreeMap<String, String>,
    key: &str,
    code: &'static str,
) -> Result<&'a str, NativeDescriptorError> {
    runtimes
        .get(key)
        .map(String::as_str)
        .ok_or(NativeDescriptorError(code))
}

impl NativeDescriptor {
    /// Read a native step's declaration. Refuses every key this build does not
    /// define, so a descriptor cannot smuggle facts past the validator.
    pub fn from_step(step: &BoundStep) -> Result<Self, NativeDescriptorError> {
        if step.protocol != NATIVE_PROTOCOL {
            return Err(NativeDescriptorError("native_protocol_required"));
        }
        for key in step.runtimes.keys() {
            if !matches!(
                key.as_str(),
                NATIVE_OS_RUNTIME
                    | NATIVE_ARCH_RUNTIME
                    | NATIVE_ARTIFACT_RUNTIME
                    | NATIVE_ENTRY_RUNTIME
                    | NATIVE_FORMAT_RUNTIME
                    | NATIVE_SIGNATURE_RUNTIME
                    | NATIVE_SIGNATURE_TARGET_RUNTIME
                    | NATIVE_REMOTE_URL_RUNTIME
                    | NATIVE_REMOTE_SHA256_RUNTIME
                    | NATIVE_REMOTE_BYTES_RUNTIME
                    | NATIVE_SOURCE_URL_RUNTIME
                    | NATIVE_SOURCE_COMMIT_RUNTIME
                    | NATIVE_SOURCE_SHA256_RUNTIME
                    | NATIVE_CARGO_LOCK_RUNTIME
                    | NATIVE_TOOLCHAIN_RUNTIME
                    | NATIVE_BUILD_ARGV_RUNTIME
                    | NATIVE_BUILD_CACHE_RUNTIME
                    | NATIVE_ARGV_RUNTIME
            ) {
                return Err(NativeDescriptorError("native_descriptor_unknown_key"));
            }
        }
        let os = step
            .runtimes
            .get(NATIVE_OS_RUNTIME)
            .and_then(|value| NativeOs::parse(value))
            .ok_or(NativeDescriptorError("native_os_invalid"))?;
        let arch = step
            .runtimes
            .get(NATIVE_ARCH_RUNTIME)
            .and_then(|value| NativeArch::parse(value))
            .ok_or(NativeDescriptorError("native_arch_invalid"))?;
        let source = parse_artifact_source(&step.runtimes)?;
        let entry = step
            .runtimes
            .get(NATIVE_ENTRY_RUNTIME)
            .filter(|value| valid_native_entry(value))
            .ok_or(NativeDescriptorError("native_entry_invalid"))?
            .clone();
        let format = match step.runtimes.get(NATIVE_FORMAT_RUNTIME) {
            None => NativeArtifactFormat::TarGz,
            Some(value) => NativeArtifactFormat::parse(value)
                .ok_or(NativeDescriptorError("native_format_unsupported"))?,
        };
        if format != NativeArtifactFormat::TarGz
            && !matches!(source, NativeArtifactSource::Remote(_))
        {
            return Err(NativeDescriptorError("native_format_unsupported"));
        }
        let signature = match step.runtimes.get(NATIVE_SIGNATURE_RUNTIME) {
            None => NativeSignaturePolicy::RequireValid,
            Some(value) => NativeSignaturePolicy::parse(value)
                .ok_or(NativeDescriptorError("native_signature_policy_invalid"))?,
        };
        let signature_target = match step.runtimes.get(NATIVE_SIGNATURE_TARGET_RUNTIME) {
            None => NativeSignatureTarget::Executable,
            Some(value) => NativeSignatureTarget::parse(value)
                .ok_or(NativeDescriptorError("native_signature_target_invalid"))?,
        };
        // A bundle target names a directory the artifact must actually carry.
        // The entry-relative check is host-independent; the adapter proves the
        // bundle exists after extraction and verifies that root.
        if signature_target.is_bundle() && native_bundle_root(&entry).is_none() {
            return Err(NativeDescriptorError(
                "native_signature_target_not_a_bundle",
            ));
        }
        let argv = match step.runtimes.get(NATIVE_ARGV_RUNTIME) {
            None => Vec::new(),
            Some(value) => parse_native_argv(value)?,
        };
        Ok(Self {
            os,
            arch,
            entry,
            format,
            signature,
            signature_target,
            source,
            argv,
        })
    }

    /// `os/arch`, normalized the way host facts report it.
    pub fn platform(&self) -> String {
        format!("{}/{}", self.os.as_str(), self.arch.as_str())
    }

    /// The bundled artifact reference, when there is one.
    pub fn bundled_artifact(&self) -> Option<&str> {
        self.source.bundled_artifact()
    }

    /// The canonical `runtimes` map this descriptor binds to.
    ///
    /// Optional facts are written only when they differ from the defaults, so
    /// one declaration has exactly one byte form.
    pub fn runtimes(&self) -> std::collections::BTreeMap<String, String> {
        let mut runtimes = std::collections::BTreeMap::from([
            (NATIVE_OS_RUNTIME.to_owned(), self.os.as_str().to_owned()),
            (
                NATIVE_ARCH_RUNTIME.to_owned(),
                self.arch.as_str().to_owned(),
            ),
            (NATIVE_ENTRY_RUNTIME.to_owned(), self.entry.clone()),
        ]);
        match &self.source {
            NativeArtifactSource::Bundled { artifact } => {
                runtimes.insert(NATIVE_ARTIFACT_RUNTIME.to_owned(), artifact.clone());
            }
            NativeArtifactSource::Remote(remote) => {
                runtimes.insert(NATIVE_REMOTE_URL_RUNTIME.to_owned(), remote.url.clone());
                runtimes.insert(
                    NATIVE_REMOTE_SHA256_RUNTIME.to_owned(),
                    remote.sha256.clone(),
                );
                runtimes.insert(
                    NATIVE_REMOTE_BYTES_RUNTIME.to_owned(),
                    remote.bytes.to_string(),
                );
            }
            NativeArtifactSource::Source(source) => {
                runtimes.insert(NATIVE_SOURCE_URL_RUNTIME.to_owned(), source.url.clone());
                runtimes.insert(
                    NATIVE_SOURCE_COMMIT_RUNTIME.to_owned(),
                    source.commit.clone(),
                );
                runtimes.insert(
                    NATIVE_SOURCE_SHA256_RUNTIME.to_owned(),
                    source.sha256.clone(),
                );
                runtimes.insert(
                    NATIVE_CARGO_LOCK_RUNTIME.to_owned(),
                    source.cargo_lock.clone(),
                );
                runtimes.insert(
                    NATIVE_TOOLCHAIN_RUNTIME.to_owned(),
                    source.toolchain.as_str(),
                );
                runtimes.insert(NATIVE_BUILD_ARGV_RUNTIME.to_owned(), source.build.as_str());
                runtimes.insert(
                    NATIVE_BUILD_CACHE_RUNTIME.to_owned(),
                    source.cache.as_str().to_owned(),
                );
            }
        }
        if self.format != NativeArtifactFormat::TarGz {
            runtimes.insert(
                NATIVE_FORMAT_RUNTIME.to_owned(),
                self.format.as_str().to_owned(),
            );
        }
        if self.signature != NativeSignaturePolicy::RequireValid {
            runtimes.insert(
                NATIVE_SIGNATURE_RUNTIME.to_owned(),
                self.signature.as_str().to_owned(),
            );
        }
        if self.signature_target != NativeSignatureTarget::Executable {
            runtimes.insert(
                NATIVE_SIGNATURE_TARGET_RUNTIME.to_owned(),
                self.signature_target.as_str().to_owned(),
            );
        }
        if !self.argv.is_empty() {
            runtimes.insert(
                NATIVE_ARGV_RUNTIME.to_owned(),
                serde_json::to_string(&self.argv).expect("a string vector serializes"),
            );
        }
        runtimes
    }
}

/// Read the one declared artifact source.
///
/// The three sources are exclusive and each is all-or-nothing: a declaration
/// names one, or the route is refused. There is no precedence between them and
/// no partial form to complete.
fn parse_artifact_source(
    runtimes: &std::collections::BTreeMap<String, String>,
) -> Result<NativeArtifactSource, NativeDescriptorError> {
    let bundled = runtimes.contains_key(NATIVE_ARTIFACT_RUNTIME);
    let remote = [
        NATIVE_REMOTE_URL_RUNTIME,
        NATIVE_REMOTE_SHA256_RUNTIME,
        NATIVE_REMOTE_BYTES_RUNTIME,
    ]
    .iter()
    .filter(|key| runtimes.contains_key(**key))
    .count();
    let source = [
        NATIVE_SOURCE_URL_RUNTIME,
        NATIVE_SOURCE_COMMIT_RUNTIME,
        NATIVE_SOURCE_SHA256_RUNTIME,
        NATIVE_CARGO_LOCK_RUNTIME,
        NATIVE_TOOLCHAIN_RUNTIME,
        NATIVE_BUILD_ARGV_RUNTIME,
        NATIVE_BUILD_CACHE_RUNTIME,
    ]
    .iter()
    .filter(|key| runtimes.contains_key(**key))
    .count();
    let declared = usize::from(bundled) + usize::from(remote > 0) + usize::from(source > 0);
    if declared == 0 {
        return Err(NativeDescriptorError("native_artifact_source_missing"));
    }
    if declared > 1 {
        return Err(NativeDescriptorError("native_artifact_source_ambiguous"));
    }
    if remote > 0 {
        if remote != 3 {
            return Err(NativeDescriptorError("native_remote_incomplete"));
        }
        let url = required(
            runtimes,
            NATIVE_REMOTE_URL_RUNTIME,
            "native_remote_incomplete",
        )?;
        let sha = required(
            runtimes,
            NATIVE_REMOTE_SHA256_RUNTIME,
            "native_remote_incomplete",
        )?;
        let bytes = required(
            runtimes,
            NATIVE_REMOTE_BYTES_RUNTIME,
            "native_remote_incomplete",
        )?;
        let bytes = bytes
            .parse::<u64>()
            .map_err(|_| NativeDescriptorError("native_remote_bytes_invalid"))?;
        return RemoteArtifact::new(url, sha, bytes).map(NativeArtifactSource::Remote);
    }
    if source > 0 {
        if source != 7 {
            return Err(NativeDescriptorError("native_source_incomplete"));
        }
        let url = required(
            runtimes,
            NATIVE_SOURCE_URL_RUNTIME,
            "native_source_incomplete",
        )?;
        if !valid_native_remote_url(url) {
            return Err(NativeDescriptorError("native_source_url_invalid"));
        }
        let commit = required(
            runtimes,
            NATIVE_SOURCE_COMMIT_RUNTIME,
            "native_source_incomplete",
        )?;
        if !valid_git_commit(commit) {
            return Err(NativeDescriptorError("native_source_commit_invalid"));
        }
        let sha = required(
            runtimes,
            NATIVE_SOURCE_SHA256_RUNTIME,
            "native_source_incomplete",
        )?;
        if !is_sha256_ref(sha) {
            return Err(NativeDescriptorError("native_source_sha256_invalid"));
        }
        let cargo_lock = required(
            runtimes,
            NATIVE_CARGO_LOCK_RUNTIME,
            "native_source_incomplete",
        )?;
        if !is_sha256_ref(cargo_lock) {
            return Err(NativeDescriptorError("native_cargo_lock_invalid"));
        }
        let toolchain = required(
            runtimes,
            NATIVE_TOOLCHAIN_RUNTIME,
            "native_source_incomplete",
        )?;
        let toolchain = RustToolchainSpec::parse(toolchain)
            .ok_or(NativeDescriptorError("native_toolchain_invalid"))?;
        let build = required(
            runtimes,
            NATIVE_BUILD_ARGV_RUNTIME,
            "native_source_incomplete",
        )?;
        let build = CargoBuildInvocation::parse(build)
            .ok_or(NativeDescriptorError("native_build_argv_invalid"))?;
        let cache = required(
            runtimes,
            NATIVE_BUILD_CACHE_RUNTIME,
            "native_source_incomplete",
        )?;
        let cache = NativeBuildCachePolicy::parse(cache)
            .ok_or(NativeDescriptorError("native_build_cache_invalid"))?;
        return Ok(NativeArtifactSource::Source(Box::new(SourceBuild {
            url: url.to_owned(),
            commit: commit.to_owned(),
            sha256: sha.to_owned(),
            cargo_lock: cargo_lock.to_owned(),
            toolchain,
            build,
            cache,
        })));
    }
    let artifact = required(runtimes, NATIVE_ARTIFACT_RUNTIME, "native_artifact_invalid")?;
    if !is_sha256_ref(artifact) {
        return Err(NativeDescriptorError("native_artifact_invalid"));
    }
    Ok(NativeArtifactSource::Bundled {
        artifact: artifact.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn step(runtimes: &[(&str, &str)]) -> BoundStep {
        BoundStep {
            id: "serve".to_owned(),
            protocol: NATIVE_PROTOCOL.to_owned(),
            op: "serve".to_owned(),
            argv: Vec::new(),
            cwd: String::new(),
            env: BTreeMap::new(),
            source: None,
            root: None,
            entry: None,
            spa_fallback: None,
            network: Default::default(),
            runtimes: runtimes
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            state: Vec::new(),
            bindings: Vec::new(),
            peer_ports: Vec::new(),
        }
    }

    fn sha() -> String {
        format!("sha256:{}", "a".repeat(64))
    }

    fn valid_runtimes() -> Vec<(&'static str, String)> {
        vec![
            (NATIVE_OS_RUNTIME, "macos".to_owned()),
            (NATIVE_ARCH_RUNTIME, "aarch64".to_owned()),
            (NATIVE_ARTIFACT_RUNTIME, sha()),
            (NATIVE_ENTRY_RUNTIME, "app/run.sh".to_owned()),
        ]
    }

    /// A Terminal route's argument vector is a bounded, printable fact: the
    /// Adapter executes it directly, so nothing in it may become a shell word.
    #[test]
    fn an_argument_vector_is_bounded_and_printable() {
        assert!(parse_native_argv("[]").unwrap().is_empty());
        assert_eq!(
            parse_native_argv(r#"["mcp","--headless"]"#).unwrap(),
            vec!["mcp".to_owned(), "--headless".to_owned()]
        );
        assert!(parse_native_argv("not json").is_err());
        assert!(parse_native_argv(r#"[""]"#).is_err());
        assert!(parse_native_argv(r#"["a\u0001b"]"#).is_err());
        let too_many = format!(
            "[{}]",
            (0..=MAX_NATIVE_ARGV)
                .map(|_| "\"x\"")
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse_native_argv(&too_many).is_err());
        let too_long = format!("[\"{}\"]", "x".repeat(MAX_NATIVE_ARGV_BYTES + 1));
        assert!(parse_native_argv(&too_long).is_err());
    }

    /// The declared vector is a descriptor fact, not a launch-only aside: it
    /// must serialize back to the exact statement a Run later reads.
    #[test]
    fn an_argument_vector_round_trips_through_the_descriptor() {
        let mut runtimes = valid_runtimes();
        runtimes.push((NATIVE_ARGV_RUNTIME, r#"["mcp","--headless"]"#.to_owned()));
        let borrowed: Vec<(&str, &str)> = runtimes
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let descriptor =
            NativeDescriptor::from_step(&step(&borrowed)).expect("a declared vector parses");
        assert_eq!(
            descriptor.argv,
            vec!["mcp".to_owned(), "--headless".to_owned()]
        );
        assert_eq!(
            descriptor
                .runtimes()
                .get(NATIVE_ARGV_RUNTIME)
                .map(String::as_str),
            Some(r#"["mcp","--headless"]"#)
        );
    }

    fn valid_step() -> BoundStep {
        let owned = valid_runtimes();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        step(&borrowed)
    }

    fn remote_runtimes() -> Vec<(&'static str, String)> {
        vec![
            (NATIVE_OS_RUNTIME, "macos".to_owned()),
            (NATIVE_ARCH_RUNTIME, "aarch64".to_owned()),
            (
                NATIVE_REMOTE_URL_RUNTIME,
                "https://github.com/example/app/releases/download/v1.0.0/app.tar.gz".to_owned(),
            ),
            (NATIVE_REMOTE_SHA256_RUNTIME, sha()),
            (NATIVE_REMOTE_BYTES_RUNTIME, "123456".to_owned()),
            (
                NATIVE_ENTRY_RUNTIME,
                "App.app/Contents/MacOS/App".to_owned(),
            ),
        ]
    }

    fn remote_step() -> BoundStep {
        let owned = remote_runtimes();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        step(&borrowed)
    }

    fn source_runtimes() -> Vec<(&'static str, String)> {
        vec![
            (NATIVE_OS_RUNTIME, "macos".to_owned()),
            (NATIVE_ARCH_RUNTIME, "aarch64".to_owned()),
            (
                NATIVE_SOURCE_URL_RUNTIME,
                "https://codeload.github.com/example/app/tar.gz/8ba46e9e2181396a9b5903b98031399aef61cf26"
                    .to_owned(),
            ),
            (
                NATIVE_SOURCE_COMMIT_RUNTIME,
                "8ba46e9e2181396a9b5903b98031399aef61cf26".to_owned(),
            ),
            (NATIVE_SOURCE_SHA256_RUNTIME, sha()),
            (NATIVE_CARGO_LOCK_RUNTIME, sha()),
            (NATIVE_TOOLCHAIN_RUNTIME, "rust:1.83.0".to_owned()),
            (
                NATIVE_BUILD_ARGV_RUNTIME,
                "cargo build --locked --release -p app".to_owned(),
            ),
            (NATIVE_BUILD_CACHE_RUNTIME, "content-hash".to_owned()),
            (
                NATIVE_ENTRY_RUNTIME,
                ".ato-build/target/release/app".to_owned(),
            ),
        ]
    }

    fn source_step() -> BoundStep {
        let owned = source_runtimes();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        step(&borrowed)
    }

    fn refuse(mut candidate: BoundStep, key: &str, value: &str) -> &'static str {
        candidate.runtimes.insert(key.to_owned(), value.to_owned());
        NativeDescriptor::from_step(&candidate).unwrap_err().0
    }

    #[test]
    fn a_complete_descriptor_parses_with_both_policy_defaults() {
        let descriptor = NativeDescriptor::from_step(&valid_step()).unwrap();
        assert_eq!(descriptor.os, NativeOs::Macos);
        assert_eq!(descriptor.arch, NativeArch::Aarch64);
        assert_eq!(descriptor.format, NativeArtifactFormat::TarGz);
        assert_eq!(descriptor.signature, NativeSignaturePolicy::RequireValid);
        assert_eq!(descriptor.platform(), "macos/aarch64");
        assert_eq!(descriptor.source.kind(), "bundled");
        assert_eq!(descriptor.bundled_artifact(), Some(sha().as_str()));
        assert_eq!(descriptor.runtimes(), valid_step().runtimes);
    }

    #[test]
    fn explicit_optional_facts_round_trip_and_defaults_are_not_written() {
        let mut candidate = valid_step();
        candidate.runtimes.insert(
            NATIVE_SIGNATURE_RUNTIME.to_owned(),
            "require-valid-or-local-override".to_owned(),
        );
        candidate
            .runtimes
            .insert(NATIVE_FORMAT_RUNTIME.to_owned(), "tar.gz".to_owned());
        let descriptor = NativeDescriptor::from_step(&candidate).unwrap();
        assert_eq!(
            descriptor.signature,
            NativeSignaturePolicy::RequireValidOrLocalOverride
        );
        assert!(descriptor.signature.allows_local_override());
        let written = descriptor.runtimes();
        assert_eq!(
            written.get(NATIVE_SIGNATURE_RUNTIME).map(String::as_str),
            Some("require-valid-or-local-override")
        );
        // tar.gz is the default, so it is not written back.
        assert!(!written.contains_key(NATIVE_FORMAT_RUNTIME));
    }

    #[test]
    fn unknown_keys_and_unknown_enum_values_fail_closed() {
        let mut unknown = valid_step();
        unknown
            .runtimes
            .insert("ato.native.smuggle".to_owned(), "x".to_owned());
        assert_eq!(
            NativeDescriptor::from_step(&unknown).unwrap_err().0,
            "native_descriptor_unknown_key"
        );

        let mut foreign = valid_step();
        foreign
            .runtimes
            .insert("python".to_owned(), "3.12".to_owned());
        assert_eq!(
            NativeDescriptor::from_step(&foreign).unwrap_err().0,
            "native_descriptor_unknown_key"
        );

        for (key, value, code) in [
            (NATIVE_OS_RUNTIME, "linux", "native_os_invalid"),
            (NATIVE_ARCH_RUNTIME, "amd64", "native_arch_invalid"),
            (NATIVE_FORMAT_RUNTIME, "7z", "native_format_unsupported"),
            (
                NATIVE_SIGNATURE_RUNTIME,
                "verify-and-record",
                "native_signature_policy_invalid",
            ),
            (
                NATIVE_ARTIFACT_RUNTIME,
                "sha256:NOTAHASH",
                "native_artifact_invalid",
            ),
        ] {
            assert_eq!(refuse(valid_step(), key, value), code, "{key}");
        }
    }

    #[test]
    fn an_entry_outside_the_extracted_root_is_refused() {
        for entry in [
            "",
            "/app/run",
            "../run",
            "app/../run",
            "app/./run",
            "app//run",
            "app\\run",
            "C:/run",
            "app:run",
            "trailing/",
        ] {
            assert!(!valid_native_entry(entry), "{entry:?}");
            assert_eq!(
                refuse(valid_step(), NATIVE_ENTRY_RUNTIME, entry),
                "native_entry_invalid",
                "{entry:?}"
            );
        }
        for entry in ["run", "app/run", "a/b/c"] {
            assert!(valid_native_entry(entry), "{entry:?}");
        }
    }

    #[test]
    fn a_non_native_step_is_refused() {
        let mut candidate = valid_step();
        candidate.protocol = "ato.process@1".to_owned();
        assert_eq!(
            NativeDescriptor::from_step(&candidate).unwrap_err().0,
            "native_protocol_required"
        );
    }

    #[test]
    fn the_default_signature_target_is_executable_and_stays_out_of_the_bytes() {
        let descriptor = NativeDescriptor::from_step(&valid_step()).unwrap();
        assert_eq!(
            descriptor.signature_target,
            NativeSignatureTarget::Executable
        );
        assert_eq!(descriptor.runtimes(), valid_step().runtimes);
        assert!(
            !descriptor
                .runtimes()
                .contains_key(NATIVE_SIGNATURE_TARGET_RUNTIME)
        );

        // Declaring the default explicitly is the same declaration, so it is
        // not written either: one fact has one byte form.
        let mut explicit = valid_step();
        explicit.runtimes.insert(
            NATIVE_SIGNATURE_TARGET_RUNTIME.to_owned(),
            "executable".to_owned(),
        );
        let descriptor = NativeDescriptor::from_step(&explicit).unwrap();
        assert_eq!(
            descriptor.signature_target,
            NativeSignatureTarget::Executable
        );
        assert_eq!(descriptor.runtimes(), valid_step().runtimes);
    }

    #[test]
    fn a_bundle_signature_target_round_trips_for_an_entry_inside_an_app() {
        let mut candidate = valid_step();
        candidate.runtimes.insert(
            NATIVE_ENTRY_RUNTIME.to_owned(),
            "MyApp.app/Contents/MacOS/MyApp".to_owned(),
        );
        candidate.runtimes.insert(
            NATIVE_SIGNATURE_TARGET_RUNTIME.to_owned(),
            "bundle".to_owned(),
        );
        let descriptor = NativeDescriptor::from_step(&candidate).unwrap();
        assert_eq!(descriptor.signature_target, NativeSignatureTarget::Bundle);
        assert_eq!(native_bundle_root(&descriptor.entry), Some("MyApp.app"));
        assert_eq!(
            descriptor
                .runtimes()
                .get(NATIVE_SIGNATURE_TARGET_RUNTIME)
                .map(String::as_str),
            Some("bundle")
        );

        // A bundle at a nested path keeps its whole prefix as the target.
        assert_eq!(
            native_bundle_root("apps/Tools/Editor.app/Contents/MacOS/editor"),
            Some("apps/Tools/Editor.app")
        );
    }

    #[test]
    fn a_bundle_signature_target_without_an_app_ancestor_is_refused() {
        for entry in [
            "app/run.sh",
            "MyApp.app",
            "Contents/MacOS/MyApp",
            "MyApp.app/../run.sh",
        ] {
            let mut candidate = valid_step();
            candidate
                .runtimes
                .insert(NATIVE_ENTRY_RUNTIME.to_owned(), entry.to_owned());
            candidate.runtimes.insert(
                NATIVE_SIGNATURE_TARGET_RUNTIME.to_owned(),
                "bundle".to_owned(),
            );
            let error = NativeDescriptor::from_step(&candidate).unwrap_err();
            // An unsafe entry is refused first; a safe one must fail for the
            // bundle requirement.
            assert!(
                matches!(
                    error.0,
                    "native_signature_target_not_a_bundle" | "native_entry_invalid"
                ),
                "{entry}: {}",
                error.0
            );
        }
    }

    #[test]
    fn an_unknown_signature_target_is_refused() {
        for value in ["codesign", "application", "BUNDLE", ""] {
            assert_eq!(
                refuse(valid_step(), NATIVE_SIGNATURE_TARGET_RUNTIME, value),
                "native_signature_target_invalid",
                "{value:?}"
            );
        }
    }

    #[test]
    fn a_plain_entry_has_no_bundle_root() {
        for entry in [
            "run",
            "app/run.sh",
            "MyApp.appx/Contents/run",
            "a.b.app.c/run",
        ] {
            assert_eq!(native_bundle_root(entry), None, "{entry}");
        }
    }

    #[test]
    fn a_remote_record_round_trips_and_declares_no_bundled_blob() {
        let descriptor = NativeDescriptor::from_step(&remote_step()).unwrap();
        let NativeArtifactSource::Remote(remote) = &descriptor.source else {
            panic!("a remote declaration must parse as a remote source");
        };
        assert_eq!(remote.bytes, 123456);
        assert_eq!(descriptor.bundled_artifact(), None);
        assert_eq!(descriptor.source.kind(), "remote");
        assert_eq!(descriptor.runtimes(), remote_step().runtimes);
        assert_eq!(descriptor.signature, NativeSignaturePolicy::RequireValid);
    }

    #[test]
    fn a_source_record_round_trips_with_its_fixed_build_vocabulary() {
        let descriptor = NativeDescriptor::from_step(&source_step()).unwrap();
        let NativeArtifactSource::Source(source) = &descriptor.source else {
            panic!("a source declaration must parse as a source build");
        };
        assert_eq!(source.toolchain.as_str(), "rust:1.83.0");
        assert_eq!(
            source.build.as_str(),
            "cargo build --locked --release -p app"
        );
        assert_eq!(source.cache, NativeBuildCachePolicy::ContentHash);
        assert_eq!(descriptor.bundled_artifact(), None);
        assert_eq!(descriptor.runtimes(), source_step().runtimes);
    }

    #[test]
    fn two_artifact_sources_are_ambiguous_and_none_is_missing() {
        // bundle + remote
        let mut both = remote_step();
        both.runtimes
            .insert(NATIVE_ARTIFACT_RUNTIME.to_owned(), sha());
        assert_eq!(
            NativeDescriptor::from_step(&both).unwrap_err().0,
            "native_artifact_source_ambiguous"
        );

        // bundle + source
        let mut both = source_step();
        both.runtimes
            .insert(NATIVE_ARTIFACT_RUNTIME.to_owned(), sha());
        assert_eq!(
            NativeDescriptor::from_step(&both).unwrap_err().0,
            "native_artifact_source_ambiguous"
        );

        // remote + source: every remote fact is present, so only mutual
        // exclusivity can refuse it.
        let mut both = source_step();
        let borrowed = remote_runtimes();
        for (key, value) in &borrowed {
            if key.starts_with(NATIVE_RUNTIME_PREFIX) && *key != NATIVE_OS_RUNTIME {
                both.runtimes.insert((*key).to_owned(), value.clone());
            }
        }
        assert_eq!(
            NativeDescriptor::from_step(&both).unwrap_err().0,
            "native_artifact_source_ambiguous"
        );

        // An entry and a platform alone name no artifact at all.
        let mut none = valid_step();
        none.runtimes.remove(NATIVE_ARTIFACT_RUNTIME);
        assert_eq!(
            NativeDescriptor::from_step(&none).unwrap_err().0,
            "native_artifact_source_missing"
        );
    }

    #[test]
    fn a_partial_remote_record_is_refused_rather_than_completed() {
        for missing in [
            NATIVE_REMOTE_URL_RUNTIME,
            NATIVE_REMOTE_SHA256_RUNTIME,
            NATIVE_REMOTE_BYTES_RUNTIME,
        ] {
            let mut candidate = remote_step();
            candidate.runtimes.remove(missing);
            assert_eq!(
                NativeDescriptor::from_step(&candidate).unwrap_err().0,
                "native_remote_incomplete",
                "{missing}"
            );
        }
        for missing in [
            NATIVE_SOURCE_URL_RUNTIME,
            NATIVE_SOURCE_COMMIT_RUNTIME,
            NATIVE_SOURCE_SHA256_RUNTIME,
            NATIVE_CARGO_LOCK_RUNTIME,
            NATIVE_TOOLCHAIN_RUNTIME,
            NATIVE_BUILD_ARGV_RUNTIME,
            NATIVE_BUILD_CACHE_RUNTIME,
        ] {
            let mut candidate = source_step();
            candidate.runtimes.remove(missing);
            assert_eq!(
                NativeDescriptor::from_step(&candidate).unwrap_err().0,
                "native_source_incomplete",
                "{missing}"
            );
        }
    }

    #[test]
    fn remote_and_source_fields_fail_closed_individually() {
        for (key, value, code) in [
            (
                NATIVE_REMOTE_URL_RUNTIME,
                "http://github.com/example/app.tar.gz",
                "native_remote_url_invalid",
            ),
            (
                NATIVE_REMOTE_URL_RUNTIME,
                "https://github.com/example/app.tar.gz#frag",
                "native_remote_url_invalid",
            ),
            (
                NATIVE_REMOTE_URL_RUNTIME,
                "https:///app.tar.gz",
                "native_remote_url_invalid",
            ),
            (
                NATIVE_REMOTE_SHA256_RUNTIME,
                "sha256:ABC",
                "native_remote_sha256_invalid",
            ),
            (
                NATIVE_REMOTE_BYTES_RUNTIME,
                "0",
                "native_remote_bytes_invalid",
            ),
            (
                NATIVE_REMOTE_BYTES_RUNTIME,
                "not-a-number",
                "native_remote_bytes_invalid",
            ),
            (
                NATIVE_REMOTE_BYTES_RUNTIME,
                "4398046511105",
                "native_remote_bytes_invalid",
            ),
        ] {
            assert_eq!(refuse(remote_step(), key, value), code, "{key}={value}");
        }

        for (key, value, code) in [
            (
                NATIVE_SOURCE_URL_RUNTIME,
                "http://codeload.github.com/example/app/tar.gz/x",
                "native_source_url_invalid",
            ),
            (
                NATIVE_SOURCE_COMMIT_RUNTIME,
                "8ba46e9e",
                "native_source_commit_invalid",
            ),
            (
                NATIVE_SOURCE_COMMIT_RUNTIME,
                "8BA46E9E2181396A9B5903B98031399AEF61CF26",
                "native_source_commit_invalid",
            ),
            (
                NATIVE_SOURCE_SHA256_RUNTIME,
                "sha256:zz",
                "native_source_sha256_invalid",
            ),
            (
                NATIVE_CARGO_LOCK_RUNTIME,
                "not-a-digest",
                "native_cargo_lock_invalid",
            ),
            (
                NATIVE_TOOLCHAIN_RUNTIME,
                "rust:latest",
                "native_toolchain_invalid",
            ),
            (
                NATIVE_TOOLCHAIN_RUNTIME,
                "python:3.12.0",
                "native_toolchain_invalid",
            ),
            (
                NATIVE_BUILD_ARGV_RUNTIME,
                "bash -c 'cargo build'",
                "native_build_argv_invalid",
            ),
            (
                NATIVE_BUILD_CACHE_RUNTIME,
                "mtime",
                "native_build_cache_invalid",
            ),
        ] {
            assert_eq!(refuse(source_step(), key, value), code, "{key}={value}");
        }
    }

    #[test]
    fn the_format_vocabulary_is_scoped_to_the_artifact_source() {
        for (format, expected) in [
            ("tar.gz", NativeArtifactFormat::TarGz),
            ("zip", NativeArtifactFormat::Zip),
            ("dmg", NativeArtifactFormat::Dmg),
        ] {
            let mut candidate = remote_step();
            candidate
                .runtimes
                .insert(NATIVE_FORMAT_RUNTIME.to_owned(), format.to_owned());
            let descriptor = NativeDescriptor::from_step(&candidate).unwrap();
            assert_eq!(descriptor.format, expected, "{format}");
            if expected != NativeArtifactFormat::TarGz {
                assert_eq!(
                    descriptor
                        .runtimes()
                        .get(NATIVE_FORMAT_RUNTIME)
                        .map(String::as_str),
                    Some(format)
                );
            }
        }
        // Only a remote reference may name a non-tar.gz payload.
        assert_eq!(
            refuse(valid_step(), NATIVE_FORMAT_RUNTIME, "zip"),
            "native_format_unsupported"
        );
        assert_eq!(
            refuse(source_step(), NATIVE_FORMAT_RUNTIME, "dmg"),
            "native_format_unsupported"
        );
    }

    #[test]
    fn the_build_command_vocabulary_is_fixed() {
        let plain = CargoBuildInvocation::parse("cargo build --locked --release").unwrap();
        assert_eq!(plain.package, None);
        assert_eq!(
            plain.command(),
            vec!["cargo", "build", "--locked", "--release"]
        );
        assert_eq!(plain.as_str(), "cargo build --locked --release");

        let packed =
            CargoBuildInvocation::parse("cargo   build --locked --release -p solidcraft").unwrap();
        assert_eq!(packed.package.as_deref(), Some("solidcraft"));
        assert_eq!(
            packed.as_str(),
            "cargo build --locked --release -p solidcraft"
        );
        assert_eq!(
            packed.command(),
            vec![
                "cargo",
                "build",
                "--locked",
                "--release",
                "-p",
                "solidcraft"
            ]
        );

        for refused in [
            "",
            "cargo",
            "build --locked --release",
            "cargo build --release",
            "cargo build --locked",
            "cargo build --locked --release --features x",
            "cargo build --locked --release --target aarch64-apple-darwin",
            "cargo build --locked --release -p",
            "cargo build --locked --release -p --all",
            "cargo build --locked --release -p ../escape",
            "cargo build --locked --release extra",
            "sh -c cargo build --locked --release",
            "cargo run --locked --release",
            "cargo build --locked --release --",
            "cargo build --locked --release -p name --offline",
        ] {
            assert!(
                CargoBuildInvocation::parse(refused).is_none(),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn the_toolchain_spec_is_exact_and_maps_to_a_rust_target() {
        let spec = RustToolchainSpec::parse("rust:1.83.0").unwrap();
        assert_eq!(spec.version, "1.83.0");
        assert_eq!(spec.as_str(), "rust:1.83.0");
        for refused in [
            "rust:1.83",
            "rust:1.83.0.1",
            "rust:",
            "rust:1.083.0",
            "rustup:1.83.0",
            "1.83.0",
            "rust:stable",
        ] {
            assert!(RustToolchainSpec::parse(refused).is_none(), "{refused:?}");
        }

        assert_eq!(
            NativeArch::Aarch64.rust_target(NativeOs::Macos),
            Some("aarch64-apple-darwin")
        );
        assert_eq!(
            NativeArch::X86_64.rust_target(NativeOs::Macos),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            NativeArch::X86_64.rust_target(NativeOs::Windows),
            Some("x86_64-pc-windows-msvc")
        );
    }

    #[test]
    fn urls_and_commits_are_checked_by_shape() {
        for accepted in [
            "https://github.com/example/app.tar.gz",
            "https://static.crates.io/crates/app/app-1.2.3.crate",
            "https://example.com/a",
        ] {
            assert!(valid_native_remote_url(accepted), "{accepted}");
        }
        for refused in [
            "",
            "https://",
            "http://example.com/a",
            "file:///etc/passwd",
            "https://user@example.com/a",
            "https://example.com/a#f",
            "https://example.com/a b",
            "https://example.com\\a",
        ] {
            assert!(!valid_native_remote_url(refused), "{refused:?}");
        }
        assert!(!valid_native_remote_url(&format!(
            "https://example.com/{}",
            "a".repeat(MAX_NATIVE_REMOTE_URL_BYTES)
        )));

        assert!(valid_git_commit("8ba46e9e2181396a9b5903b98031399aef61cf26"));
        assert!(valid_git_commit(&"a".repeat(64)));
        assert!(!valid_git_commit("8ba46e9e"));
        assert!(!valid_git_commit(
            "8BA46E9E2181396A9B5903B98031399AEF61CF26"
        ));
    }
}
