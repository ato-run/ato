use std::path::Path;

use capsule_wire::handle::CapsuleDisplayStrategy;

use crate::logging::TARGET_FAVICON;
use crate::orchestrator::CapsuleLaunchSession;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShareIconSource {
    Direct(String),
    FaviconOrigin(String),
}

pub(crate) fn resolve_share_icon(session: &CapsuleLaunchSession) -> Option<ShareIconSource> {
    tracing::info!(
        target: TARGET_FAVICON,
        session_id = %session.session_id,
        handle = %session.handle,
        manifest_path = %session.manifest_path.display(),
        app_root = %session.app_root.display(),
        display_strategy = %session.display_strategy.as_str(),
        local_url = ?session.local_url,
        "resolving share icon"
    );

    if let Some(source) = resolve_capsule_icon_source(&session.manifest_path, &session.app_root) {
        tracing::info!(
            target: TARGET_FAVICON,
            session_id = %session.session_id,
            source = %source,
            "resolved share icon from capsule metadata"
        );
        return Some(ShareIconSource::Direct(source));
    }

    if session.display_strategy == CapsuleDisplayStrategy::WebUrl || session.local_url.is_some() {
        if let Some(local_url) = session.local_url.as_deref() {
            match web_favicon_origin(local_url) {
                Some(origin) => {
                    tracing::info!(
                        target: TARGET_FAVICON,
                        session_id = %session.session_id,
                        local_url,
                        origin,
                        "resolved share icon to web favicon origin"
                    );
                    return Some(ShareIconSource::FaviconOrigin(origin));
                }
                None => {
                    tracing::error!(
                        target: TARGET_FAVICON,
                        session_id = %session.session_id,
                        local_url,
                        "failed to resolve share icon favicon origin from local_url"
                    );
                }
            }
        } else {
            tracing::error!(
                target: TARGET_FAVICON,
                session_id = %session.session_id,
                "web share icon fallback requested but session has no local_url"
            );
        }
    }

    tracing::error!(
        target: TARGET_FAVICON,
        session_id = %session.session_id,
        manifest_path = %session.manifest_path.display(),
        "failed to resolve share icon source"
    );
    None
}

/// Read `[metadata].icon` from a capsule manifest and resolve it to a value
/// the sidebar can fetch: an absolute filesystem path for relative entries, or
/// the raw string for `http(s)://`, `file://`, and `data:` image references.
pub(crate) fn resolve_capsule_icon_source(manifest_path: &Path, app_root: &Path) -> Option<String> {
    let raw = match std::fs::read_to_string(manifest_path) {
        Ok(raw) => raw,
        Err(error) => {
            tracing::error!(
                target: TARGET_FAVICON,
                manifest_path = %manifest_path.display(),
                error = %error,
                "failed to read capsule manifest while resolving icon"
            );
            return None;
        }
    };
    let manifest: capsule_core::types::CapsuleManifest = match toml::from_str(&raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::error!(
                target: TARGET_FAVICON,
                manifest_path = %manifest_path.display(),
                error = %error,
                "failed to parse capsule manifest while resolving icon"
            );
            return None;
        }
    };
    let Some(icon) = manifest.metadata.icon.filter(|s| !s.is_empty()) else {
        tracing::info!(
            target: TARGET_FAVICON,
            manifest_path = %manifest_path.display(),
            "capsule manifest has no metadata.icon"
        );
        return None;
    };
    tracing::info!(
        target: TARGET_FAVICON,
        manifest_path = %manifest_path.display(),
        icon,
        "found capsule metadata.icon"
    );
    if is_direct_image_reference(&icon) {
        tracing::info!(
            target: TARGET_FAVICON,
            manifest_path = %manifest_path.display(),
            source = %icon,
            "using direct capsule metadata icon reference"
        );
        return Some(icon);
    }

    // Published registry installs materialize source files under `source/`;
    // local dev manifests keep assets next to `capsule.toml`.
    let with_source = app_root.join("source").join(&icon);
    if with_source.exists() {
        let absolute = with_source.canonicalize().unwrap_or(with_source);
        tracing::info!(
            target: TARGET_FAVICON,
            manifest_path = %manifest_path.display(),
            source = %absolute.display(),
            "resolved capsule metadata icon from materialized source path"
        );
        return Some(absolute.to_string_lossy().to_string());
    }
    let bare = app_root.join(&icon);
    if bare.exists() {
        let absolute = bare.canonicalize().unwrap_or(bare);
        tracing::info!(
            target: TARGET_FAVICON,
            manifest_path = %manifest_path.display(),
            source = %absolute.display(),
            "resolved capsule metadata icon from app root path"
        );
        return Some(absolute.to_string_lossy().to_string());
    }
    tracing::error!(
        target: TARGET_FAVICON,
        manifest_path = %manifest_path.display(),
        app_root = %app_root.display(),
        icon,
        source_candidate = %with_source.display(),
        bare_candidate = %bare.display(),
        "capsule metadata icon relative path did not exist"
    );
    None
}

pub(crate) fn web_favicon_origin(local_url: &str) -> Option<String> {
    let parsed = match url::Url::parse(local_url) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::error!(
                target: TARGET_FAVICON,
                local_url,
                error = %error,
                "failed to parse local_url for favicon origin"
            );
            return None;
        }
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        tracing::error!(
            target: TARGET_FAVICON,
            local_url,
            scheme = parsed.scheme(),
            "local_url scheme cannot provide a web favicon"
        );
        return None;
    }
    let origin = parsed.origin().ascii_serialization();
    tracing::info!(
        target: TARGET_FAVICON,
        local_url,
        origin,
        "normalized web favicon origin"
    );
    Some(origin)
}

fn is_direct_image_reference(value: &str) -> bool {
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("file://")
        || value.starts_with("data:")
}

#[cfg(test)]
mod tests {
    use super::{resolve_capsule_icon_source, web_favicon_origin};

    fn write_manifest(root: &std::path::Path, icon: &str) -> std::path::PathBuf {
        let manifest_path = root.join("capsule.toml");
        std::fs::write(
            &manifest_path,
            format!(
                r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
runtime = "web/static"
run = "dist"

[metadata]
icon = "{icon}"
"#
            ),
        )
        .expect("write manifest");
        manifest_path
    }

    #[test]
    fn metadata_icon_direct_references_pass_through() {
        let tmp = tempfile::tempdir().expect("tempdir");

        for icon in [
            "https://example.com/icon.png",
            "http://example.com/icon.svg",
            "file:///Users/example/icon.png",
            "data:image/png;base64,AAAA",
        ] {
            let manifest_path = write_manifest(tmp.path(), icon);
            assert_eq!(
                resolve_capsule_icon_source(&manifest_path, tmp.path()).as_deref(),
                Some(icon)
            );
        }
    }

    #[test]
    fn metadata_icon_relative_path_resolves_against_app_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = write_manifest(tmp.path(), "assets/icon.png");
        let icon_path = tmp.path().join("assets/icon.png");
        std::fs::create_dir_all(icon_path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&icon_path, b"png").expect("write icon");

        assert_eq!(
            resolve_capsule_icon_source(&manifest_path, tmp.path()),
            Some(
                icon_path
                    .canonicalize()
                    .expect("canonical")
                    .to_string_lossy()
                    .to_string()
            )
        );
    }

    #[test]
    fn metadata_icon_prefers_materialized_source_layout() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest_path = write_manifest(tmp.path(), "assets/icon.png");
        let bare = tmp.path().join("assets/icon.png");
        std::fs::create_dir_all(bare.parent().expect("bare parent")).expect("mkdir bare");
        std::fs::write(&bare, b"bare").expect("write bare");

        let source = tmp.path().join("source/assets/icon.png");
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir source");
        std::fs::write(&source, b"source").expect("write source");

        assert_eq!(
            resolve_capsule_icon_source(&manifest_path, tmp.path()),
            Some(
                source
                    .canonicalize()
                    .expect("canonical")
                    .to_string_lossy()
                    .to_string()
            )
        );
    }

    #[test]
    fn web_favicon_origin_normalizes_http_local_url() {
        assert_eq!(
            web_favicon_origin("http://127.0.0.1:5173/foo?bar=baz").as_deref(),
            Some("http://127.0.0.1:5173")
        );
        assert_eq!(
            web_favicon_origin("https://example.com/path").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn web_favicon_origin_ignores_non_http_urls() {
        assert!(web_favicon_origin("file:///tmp/app/index.html").is_none());
        assert!(web_favicon_origin("capsule://ato.run/koh0920/app").is_none());
        assert!(web_favicon_origin("not a url").is_none());
    }
}
