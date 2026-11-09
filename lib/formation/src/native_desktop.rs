//! The `ato.native@1` step protocol: an Adapter-level realization of one
//! Digestion as a digest-pinned desktop executable.
//!
//! A native route is not a new semantic noun. Its facts live in the serving
//! step's `runtimes` map (so every Derivation formed before this protocol
//! existed keeps its exact bytes) and its logical Ports reuse [`BoundPort`].
//! This module only parses and checks that declaration; it never launches
//! anything, and nothing here is host-specific: platform admission is decided
//! against observed host facts by the runtime, not by the bundle validator.
//!
//! ```text
//! [[derivation]]
//! id = "native-macos"
//! use = "ato.native@1"
//! guest_port = 18080
//! runtimes = {}
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
//!     },
//! }
//! ```

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

/// The only artifact format this increment defines.
pub const NATIVE_FORMAT_TAR_GZ: &str = "tar.gz";
/// Largest extracted artifact a bundle may carry. The raw archive is already
/// covered by the bundle's closure digest; this bounds the one blob a native
/// route adds to it.
pub const MAX_NATIVE_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
/// Largest entry path this build accepts.
pub const MAX_NATIVE_ENTRY_BYTES: usize = 1024;

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
}

/// How the artifact is packed. Only `tar.gz` exists in v0, and an unknown
/// format is refused rather than treated as an opaque blob the runtime could
/// not have extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeArtifactFormat {
    TarGz,
}

impl NativeArtifactFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TarGz => NATIVE_FORMAT_TAR_GZ,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            NATIVE_FORMAT_TAR_GZ => Some(Self::TarGz),
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

/// The validated declaration of one native serving step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDescriptor {
    pub os: NativeOs,
    pub arch: NativeArch,
    /// The `sha256:` reference of the artifact blob carried in the bundle.
    pub artifact: String,
    /// The executable path, relative to the extracted artifact root.
    pub entry: String,
    pub format: NativeArtifactFormat,
    pub signature: NativeSignaturePolicy,
}

fn is_sha256_ref(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

/// A relative, traversal-free path inside the extracted artifact root.
///
/// A leading `/`, a `..`/`.` segment, an empty segment, a backslash and a
/// colon (`drive:` and `stream:name` on Windows) are all refused: the runtime
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
        let artifact = step
            .runtimes
            .get(NATIVE_ARTIFACT_RUNTIME)
            .filter(|value| is_sha256_ref(value))
            .ok_or(NativeDescriptorError("native_artifact_invalid"))?
            .clone();
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
        let signature = match step.runtimes.get(NATIVE_SIGNATURE_RUNTIME) {
            None => NativeSignaturePolicy::RequireValid,
            Some(value) => NativeSignaturePolicy::parse(value)
                .ok_or(NativeDescriptorError("native_signature_policy_invalid"))?,
        };
        Ok(Self {
            os,
            arch,
            artifact,
            entry,
            format,
            signature,
        })
    }

    /// `os/arch`, normalized the way host facts report it.
    pub fn platform(&self) -> String {
        format!("{}/{}", self.os.as_str(), self.arch.as_str())
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
            (NATIVE_ARTIFACT_RUNTIME.to_owned(), self.artifact.clone()),
            (NATIVE_ENTRY_RUNTIME.to_owned(), self.entry.clone()),
        ]);
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
        runtimes
    }
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

    fn valid_step() -> BoundStep {
        let owned = valid_runtimes();
        let borrowed: Vec<(&str, &str)> = owned
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        step(&borrowed)
    }

    #[test]
    fn a_complete_descriptor_parses_with_both_policy_defaults() {
        let descriptor = NativeDescriptor::from_step(&valid_step()).unwrap();
        assert_eq!(descriptor.os, NativeOs::Macos);
        assert_eq!(descriptor.arch, NativeArch::Aarch64);
        assert_eq!(descriptor.format, NativeArtifactFormat::TarGz);
        assert_eq!(descriptor.signature, NativeSignaturePolicy::RequireValid);
        assert_eq!(descriptor.platform(), "macos/aarch64");
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
            (NATIVE_FORMAT_RUNTIME, "zip", "native_format_unsupported"),
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
            let mut candidate = valid_step();
            candidate.runtimes.insert(key.to_owned(), value.to_owned());
            assert_eq!(
                NativeDescriptor::from_step(&candidate).unwrap_err().0,
                code,
                "{key}"
            );
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
            let mut candidate = valid_step();
            candidate
                .runtimes
                .insert(NATIVE_ENTRY_RUNTIME.to_owned(), entry.to_owned());
            assert_eq!(
                NativeDescriptor::from_step(&candidate).unwrap_err().0,
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
}
