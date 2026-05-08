//! Built-in tool artifact registry.
//!
//! Maps a stable tool ID (e.g. `"postgresql"`) to a pinned
//! [`ToolArtifactManifest`] for the current host platform. The
//! orchestrator uses this when a provider's target declares
//! `tool_artifacts = [...]`.
//!
//! The registry is intentionally code, not data: each entry is a
//! reviewed pin — URL, sha256, layout, and `provides` set — that has
//! been validated against the upstream artifact (see #120 Phase 1).
//! Adding or bumping an entry is a deliberate code change.

use super::manifest::{
    host_platform, ArchiveFormat, ArtifactLayout, ToolArtifactManifest,
};

/// Look up a pinned manifest by stable tool ID + the current host
/// platform. Returns `None` when:
///
/// - the tool ID is unknown, or
/// - we know the tool but do not have a pinned artifact for this
///   host's platform (e.g. zonky's darwin-arm64 pin not applicable
///   on Linux; landing Linux is a follow-up issue).
///
/// The orchestrator turns the second case into a typed
/// [`crate::application::tool_artifact::ToolArtifactError::UnsupportedArtifactPlatform`]
/// for clearer messaging than a "not found" hit.
pub fn well_known_tool_artifact(tool_id: &str) -> Option<ToolArtifactManifest> {
    let host = host_platform()?;
    match (tool_id, host) {
        ("postgresql", "darwin-aarch64") => Some(postgresql_darwin_aarch64()),
        _ => None,
    }
}

/// Returns the list of supported tool IDs. Used by validation paths
/// that need to reject unknown tool IDs at lock or preflight time
/// without requiring a host-platform match.
pub fn known_tool_ids() -> &'static [&'static str] {
    &["postgresql"]
}

/// PostgreSQL 16.9.0, darwin-aarch64.
///
/// Source: zonky-test/embedded-postgres-binaries on Maven Central.
/// JAR-wraps a `.txz` of a relocatable bin/lib/share tree. Universal
/// (x86_64 + arm64) binaries; rpath-clean (`@loader_path/../lib`),
/// so no `DYLD_LIBRARY_PATH` injection is needed on macOS. Hashes
/// and layout pinned in the #120 Phase 1 investigation.
///
/// `pg_isready` is intentionally absent from `provides`: the zonky
/// distribution does not ship it. Readiness is the orchestrator's
/// concern via [`crate::application::dependency_runtime::ready::ReadyProbeKind::Postgres`],
/// not a per-binary dependency.
fn postgresql_darwin_aarch64() -> ToolArtifactManifest {
    ToolArtifactManifest {
        schema_version: "1".to_string(),
        name: "postgresql".to_string(),
        version: "16.9.0".to_string(),
        platform: "darwin-aarch64".to_string(),
        url: "https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-darwin-arm64v8/16.9.0/embedded-postgres-binaries-darwin-arm64v8-16.9.0.jar".to_string(),
        sha256: "53b2672c602e16e4c94fb56b9aa68cc26a0bbb0df851f256f41a2cdbeccc9cb6".to_string(),
        archive_format: ArchiveFormat::JarTxz,
        inner_member: Some("postgres-darwin-arm_64.txz".to_string()),
        inner_sha256: Some(
            "090e91773217f8d3d222699a6da2bf5533ffab8c6b65b14df63cba3b1b63ea5a".to_string(),
        ),
        strip_prefix: None,
        layout: ArtifactLayout {
            bin_dir: "bin".to_string(),
            lib_dir: "lib".to_string(),
            share_dir: "share".to_string(),
        },
        provides: vec![
            "initdb".to_string(),
            "postgres".to_string(),
            "pg_ctl".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgresql_pin_validates_against_manifest_invariants() {
        let m = postgresql_darwin_aarch64();
        m.validate().expect("postgresql pin must satisfy invariants");
        // Sanity-check the most likely human-error fields.
        assert_eq!(m.name, "postgresql");
        assert_eq!(m.platform, "darwin-aarch64");
        assert_eq!(m.sha256.len(), 64);
        assert!(m.provides.contains(&"initdb".to_string()));
        assert!(m.provides.contains(&"postgres".to_string()));
        assert!(m.provides.contains(&"pg_ctl".to_string()));
        assert!(
            !m.provides.contains(&"pg_isready".to_string()),
            "pg_isready is intentionally absent — readiness moves to ato-cli"
        );
    }

    #[test]
    fn known_tool_ids_lists_postgresql() {
        assert!(known_tool_ids().contains(&"postgresql"));
    }

    #[test]
    fn well_known_returns_none_for_unknown_tool() {
        assert!(well_known_tool_artifact("not-a-tool").is_none());
    }
}
