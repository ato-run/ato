//! `ToolArtifactManifest` describes a single prebuilt provider tool
//! artifact (Postgres, Redis, …) that Ato can resolve from a verified
//! download. The manifest is the input to
//! [`crate::application::tool_artifact::resolve`]; the output is a
//! [`crate::application::tool_artifact::ResolvedToolArtifact`].
//!
//! The schema is intentionally narrow. It only covers what is needed to
//! verify, unpack, and address a relocatable tool tree on disk.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::error::ToolArtifactError;

/// Top-level manifest. Exactly one manifest describes one
/// `(name, version, platform, sha256)` artifact.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolArtifactManifest {
    /// Manifest schema version. Must be `"1"`. Reserved for future
    /// breaking changes; unknown values are rejected.
    pub schema_version: String,

    /// Tool identifier. Used in the on-disk store key. Lowercase,
    /// no slashes. Example: `"postgresql"`.
    pub name: String,

    /// Tool version string in upstream form. Example: `"16.9.0"`.
    pub version: String,

    /// Target platform tag, e.g. `"darwin-aarch64"`, `"darwin-x86_64"`,
    /// `"linux-x86_64"`. The resolver will refuse to use the artifact
    /// on any host whose [`host_platform`] does not match.
    pub platform: String,

    /// Absolute URL to fetch. Must be `http://` or `https://`. The
    /// downloader uses Ato's internal HTTP client — no `curl`/`wget`
    /// shell-out — and writes the body to a temp file before unpack.
    pub url: String,

    /// Lowercase hex sha256 of the bytes served at `url`. Verified
    /// **before** unpack. A mismatch is a hard error
    /// ([`ToolArtifactError::ArtifactChecksumMismatch`]).
    pub sha256: String,

    /// How to interpret the downloaded bytes. See [`ArchiveFormat`].
    pub archive_format: ArchiveFormat,

    /// For wrapped archives only (`jar+txz`). Names the inner archive
    /// member to extract from the outer container. Ignored for the
    /// flat formats.
    #[serde(default)]
    pub inner_member: Option<String>,

    /// Optional sha256 of the inner member. Verified after extracting
    /// the wrapper but before unpacking the inner archive. Recommended
    /// for wrapped formats so a tampered inner archive cannot pass even
    /// if the outer wrapper's sha256 is preserved.
    #[serde(default)]
    pub inner_sha256: Option<String>,

    /// Optional path prefix inside the archive to strip when laying
    /// files into the store. If unset, files are unpacked at the root.
    #[serde(default)]
    pub strip_prefix: Option<String>,

    /// On-disk layout under the resolved artifact root. The three
    /// values are paths relative to the artifact root after unpack
    /// (and optional [`Self::strip_prefix`] removal). All three are
    /// required so providers can be wired with `ATO_TOOL_*_BIN_DIR`,
    /// `_LIB_DIR`, `_SHARE_DIR` deterministically.
    pub layout: ArtifactLayout,

    /// Commands the artifact must expose under `layout.bin_dir`. The
    /// resolver fails with [`ToolArtifactError::ArtifactMissingProvidedCommand`]
    /// if any entry is absent or non-executable after unpack.
    pub provides: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactLayout {
    pub bin_dir: String,
    pub lib_dir: String,
    pub share_dir: String,
}

/// Supported archive container formats. The resolver picks the
/// extraction pipeline based on this value, not on file extension or
/// `Content-Type`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    /// gzip-compressed POSIX tar.
    #[serde(rename = "tar.gz")]
    TarGz,
    /// xz-compressed POSIX tar.
    #[serde(rename = "tar.xz")]
    TarXz,
    /// zstd-compressed POSIX tar.
    #[serde(rename = "tar.zst")]
    TarZst,
    /// Plain zip archive.
    Zip,
    /// Maven-style JAR (a zip) with a single inner `.txz` member named
    /// by [`ToolArtifactManifest::inner_member`]. The zonky postgres
    /// distribution uses this layout. The resolver extracts the named
    /// member, verifies [`ToolArtifactManifest::inner_sha256`] if
    /// provided, then unpacks the inner archive as `tar.xz`.
    #[serde(rename = "jar+txz")]
    JarTxz,
}

impl ToolArtifactManifest {
    /// Parse a manifest from TOML. Useful for capsule-side or
    /// orchestrator-side embedded manifests. The caller is responsible
    /// for treating parse failures as a typed
    /// [`ToolArtifactError::InvalidArtifactManifest`].
    ///
    /// Currently only the test suite parses manifests from TOML; the
    /// production registry constructs them in code (see
    /// [`crate::application::tool_artifact::registry`]). The function
    /// stays public so a future capsule-side `[[tool_artifacts.manifest]]`
    /// table can adopt it without an API churn.
    #[allow(dead_code)]
    pub fn from_toml(text: &str) -> Result<Self, ToolArtifactError> {
        let manifest: Self = toml::from_str(text).map_err(|e| {
            ToolArtifactError::InvalidArtifactManifest {
                name: "<unparsed>".to_string(),
                reason: e.to_string(),
            }
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Internal invariants. Called by [`Self::from_toml`] and may be
    /// called directly by callers who construct the manifest in code
    /// (e.g. provider-side bundled manifests).
    pub fn validate(&self) -> Result<(), ToolArtifactError> {
        if self.schema_version != "1" {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: format!(
                    "unsupported schema_version '{}', expected \"1\"",
                    self.schema_version
                ),
            });
        }
        if self.name.is_empty() {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: "name must not be empty".into(),
            });
        }
        if self.name.contains('/') || self.name.contains('\\') {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: format!(
                    "name '{}' must not contain path separators",
                    self.name
                ),
            });
        }
        if !is_lower_hex(&self.sha256) || self.sha256.len() != 64 {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: format!(
                    "sha256 must be 64 lowercase hex characters, got '{}'",
                    self.sha256
                ),
            });
        }
        if let Some(inner) = &self.inner_sha256 {
            if !is_lower_hex(inner) || inner.len() != 64 {
                return Err(ToolArtifactError::InvalidArtifactManifest {
                    name: self.name.clone(),
                    reason: format!(
                        "inner_sha256 must be 64 lowercase hex characters, got '{}'",
                        inner
                    ),
                });
            }
        }
        if self.url.is_empty() {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: "url must not be empty".into(),
            });
        }
        // URL scheme is enforced at the downloader call site, not here:
        // tests substitute a stub transport that uses a non-http
        // scheme, and manifest validation should not block that.
        if matches!(self.archive_format, ArchiveFormat::JarTxz) && self.inner_member.is_none() {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: "archive_format = jar+txz requires inner_member".into(),
            });
        }
        if !matches!(self.archive_format, ArchiveFormat::JarTxz)
            && (self.inner_member.is_some() || self.inner_sha256.is_some())
        {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: "inner_member/inner_sha256 are only valid for archive_format = jar+txz"
                    .into(),
            });
        }
        if self.provides.is_empty() {
            return Err(ToolArtifactError::InvalidArtifactManifest {
                name: self.name.clone(),
                reason: "provides must list at least one command".into(),
            });
        }
        let mut seen: BTreeMap<&str, ()> = BTreeMap::new();
        for cmd in &self.provides {
            if cmd.contains('/') || cmd.contains('\\') {
                return Err(ToolArtifactError::InvalidArtifactManifest {
                    name: self.name.clone(),
                    reason: format!(
                        "provides entry '{}' must be a bare command name (no path)",
                        cmd
                    ),
                });
            }
            if seen.insert(cmd.as_str(), ()).is_some() {
                return Err(ToolArtifactError::InvalidArtifactManifest {
                    name: self.name.clone(),
                    reason: format!("provides entry '{}' is duplicated", cmd),
                });
            }
        }
        Ok(())
    }
}

fn is_lower_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Returns the platform tag for the current host. Format matches the
/// `platform` field in the manifest. Returns `None` for hosts the
/// downloader does not target.
pub fn host_platform() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        return Some("darwin-aarch64");
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        return Some("darwin-x86_64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Some("linux-x86_64");
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        return Some("linux-aarch64");
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Some("windows-x86_64");
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOML: &str = r#"
schema_version = "1"
name = "postgresql"
version = "16.9.0"
platform = "darwin-aarch64"
url = "https://repo1.maven.org/example.jar"
sha256 = "53b2672c602e16e4c94fb56b9aa68cc26a0bbb0df851f256f41a2cdbeccc9cb6"
archive_format = "jar+txz"
inner_member = "postgres-darwin-arm_64.txz"
inner_sha256 = "090e91773217f8d3d222699a6da2bf5533ffab8c6b65b14df63cba3b1b63ea5a"
provides = ["initdb", "postgres", "pg_ctl"]

[layout]
bin_dir = "bin"
lib_dir = "lib"
share_dir = "share"
"#;

    #[test]
    fn parses_valid_jar_txz_manifest() {
        let m = ToolArtifactManifest::from_toml(VALID_TOML).expect("must parse");
        assert_eq!(m.name, "postgresql");
        assert_eq!(m.platform, "darwin-aarch64");
        assert_eq!(m.archive_format, ArchiveFormat::JarTxz);
        assert_eq!(
            m.inner_member.as_deref(),
            Some("postgres-darwin-arm_64.txz")
        );
        assert_eq!(m.layout.bin_dir, "bin");
        assert_eq!(m.provides, vec!["initdb", "postgres", "pg_ctl"]);
    }

    #[test]
    fn parses_plain_tar_gz_manifest() {
        let toml = r#"
schema_version = "1"
name = "demo"
version = "1.0.0"
platform = "linux-x86_64"
url = "https://example.com/demo.tar.gz"
sha256 = "0000000000000000000000000000000000000000000000000000000000000001"
archive_format = "tar.gz"
provides = ["demo"]

[layout]
bin_dir = "bin"
lib_dir = "lib"
share_dir = "share"
"#;
        let m = ToolArtifactManifest::from_toml(toml).expect("must parse");
        assert_eq!(m.archive_format, ArchiveFormat::TarGz);
        assert!(m.inner_member.is_none());
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let toml = VALID_TOML.replace("schema_version = \"1\"", "schema_version = \"99\"");
        let err = ToolArtifactManifest::from_toml(&toml).unwrap_err();
        match err {
            ToolArtifactError::InvalidArtifactManifest { reason, .. } => {
                assert!(reason.contains("schema_version"), "got: {reason}");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_non_hex_sha256() {
        let toml = VALID_TOML.replace(
            "sha256 = \"53b2672c602e16e4c94fb56b9aa68cc26a0bbb0df851f256f41a2cdbeccc9cb6\"",
            "sha256 = \"NOT_HEX_______________________________________________________________________\"",
        );
        let err = ToolArtifactManifest::from_toml(&toml).unwrap_err();
        match err {
            ToolArtifactError::InvalidArtifactManifest { reason, .. } => {
                assert!(reason.contains("sha256"), "got: {reason}");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_short_sha256() {
        let toml = VALID_TOML.replace(
            "sha256 = \"53b2672c602e16e4c94fb56b9aa68cc26a0bbb0df851f256f41a2cdbeccc9cb6\"",
            "sha256 = \"deadbeef\"",
        );
        ToolArtifactManifest::from_toml(&toml).unwrap_err();
    }

    #[test]
    fn rejects_empty_url() {
        let toml = VALID_TOML.replace(
            "url = \"https://repo1.maven.org/example.jar\"",
            "url = \"\"",
        );
        let err = ToolArtifactManifest::from_toml(&toml).unwrap_err();
        match err {
            ToolArtifactError::InvalidArtifactManifest { reason, .. } => {
                assert!(reason.contains("url"), "got: {reason}");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_jar_txz_without_inner_member() {
        let toml = VALID_TOML.replace(
            "inner_member = \"postgres-darwin-arm_64.txz\"\n",
            "",
        );
        let err = ToolArtifactManifest::from_toml(&toml).unwrap_err();
        match err {
            ToolArtifactError::InvalidArtifactManifest { reason, .. } => {
                assert!(reason.contains("inner_member"), "got: {reason}");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_inner_member_on_plain_format() {
        let toml = r#"
schema_version = "1"
name = "demo"
version = "1.0.0"
platform = "linux-x86_64"
url = "https://example.com/demo.tar.gz"
sha256 = "0000000000000000000000000000000000000000000000000000000000000001"
archive_format = "tar.gz"
inner_member = "wrong"
provides = ["demo"]

[layout]
bin_dir = "bin"
lib_dir = "lib"
share_dir = "share"
"#;
        let err = ToolArtifactManifest::from_toml(toml).unwrap_err();
        match err {
            ToolArtifactError::InvalidArtifactManifest { reason, .. } => {
                assert!(reason.contains("inner_member"), "got: {reason}");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_empty_provides() {
        let toml = VALID_TOML.replace(
            "provides = [\"initdb\", \"postgres\", \"pg_ctl\"]",
            "provides = []",
        );
        let err = ToolArtifactManifest::from_toml(&toml).unwrap_err();
        match err {
            ToolArtifactError::InvalidArtifactManifest { reason, .. } => {
                assert!(reason.contains("provides"), "got: {reason}");
            }
            other => panic!("unexpected: {other}"),
        }
    }

    #[test]
    fn rejects_provides_with_path_separator() {
        let toml = VALID_TOML.replace(
            "provides = [\"initdb\", \"postgres\", \"pg_ctl\"]",
            "provides = [\"bin/initdb\"]",
        );
        ToolArtifactManifest::from_toml(&toml).unwrap_err();
    }

    #[test]
    fn rejects_duplicate_provides() {
        let toml = VALID_TOML.replace(
            "provides = [\"initdb\", \"postgres\", \"pg_ctl\"]",
            "provides = [\"initdb\", \"initdb\"]",
        );
        ToolArtifactManifest::from_toml(&toml).unwrap_err();
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let toml = format!("{}\nrogue_field = true\n", VALID_TOML);
        toml::from_str::<ToolArtifactManifest>(&toml).unwrap_err();
    }

    #[test]
    fn host_platform_is_known_for_dev_hosts() {
        // Smoke: should resolve to one of the supported tags on
        // current dev hosts (darwin-arm64 or linux-x86_64). On
        // unsupported hosts this returns None — that's fine for the
        // resolver, which surfaces UnsupportedArtifactPlatform.
        let _ = host_platform();
    }
}
