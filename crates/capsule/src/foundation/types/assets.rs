//! Authoring metadata asset contracts.
//!
//! Mirrors the ato-api authoring asset contract (ato-api#459): the media-type
//! allowlist and the strict passive-SVG inspection profile. An authoring
//! metadata asset is one of `image/png`, `image/jpeg`, `image/webp` or
//! `image/svg+xml`; SVG is only accepted when it survives the passive profile
//! below. Binary types are checked by magic bytes only (their pixels are not
//! decoded here).

use std::path::Path;

use thiserror::Error;

use super::manifest_v1::ManifestV1Error;

/// The SVG namespace every passive SVG document must live in.
pub const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
/// Hard cap on the number of elements in a passive SVG document.
pub const MAX_SVG_ELEMENTS: usize = 10_000;
/// Hard cap on XML nesting depth.
pub const MAX_SVG_DEPTH: usize = 64;
/// Hard cap on attributes per element.
pub const MAX_SVG_ATTRS_PER_ELEMENT: usize = 40;
/// Dimensions (fixed or viewBox) are clamped to this inclusive maximum.
pub const MAX_DIMENSION: i64 = 16_384;

/// The media types accepted as authoring metadata assets, in the same order
/// as the ato-api `AUTHORING_IMAGE_MEDIA_TYPES`.
pub const AUTHORING_IMAGE_MEDIA_TYPES: [&str; 4] =
    ["image/png", "image/jpeg", "image/webp", "image/svg+xml"];

/// A validated authoring metadata asset media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetMediaType {
    Png,
    Jpeg,
    Webp,
    Svg,
}

impl AssetMediaType {
    /// Parse a media type string. Unknown values are refused: an asset URL
    /// descriptor may only carry a media type the authoring surface accepts.
    pub fn parse(media_type: &str) -> Result<Self, AssetError> {
        match media_type {
            "image/png" => Ok(Self::Png),
            "image/jpeg" => Ok(Self::Jpeg),
            "image/webp" => Ok(Self::Webp),
            "image/svg+xml" => Ok(Self::Svg),
            _ => Err(AssetError::UnsupportedMediaType(media_type.to_string())),
        }
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Svg => "image/svg+xml",
        }
    }

    /// Detect the media type from bytes (magic signatures) or a file extension.
    /// Used where a path asset declares no media type.
    pub fn detect(bytes: &[u8], path: &str) -> Option<Self> {
        if bytes.starts_with(&PNG_MAGIC) {
            return Some(Self::Png);
        }
        if bytes.starts_with(&JPEG_MAGIC) {
            return Some(Self::Jpeg);
        }
        if bytes.starts_with(&WEBP_MAGIC) {
            return Some(Self::Webp);
        }
        match Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref()
        {
            Some("svg") => Some(Self::Svg),
            Some("png") => Some(Self::Png),
            Some("jpg") | Some("jpeg") => Some(Self::Jpeg),
            Some("webp") => Some(Self::Webp),
            _ => None,
        }
    }
}

/// Errors from asset validation.
#[derive(Debug, Error)]
pub enum AssetError {
    #[error("unsupported asset media type: {0}")]
    UnsupportedMediaType(String),
    #[error("asset is not decodable {media_type} bytes")]
    InvalidBytes { media_type: &'static str },
    #[error("SVG is not well-formed XML: {0}")]
    NotWellFormed(String),
    #[error("SVG must not contain {0}")]
    DisallowedConstruct(&'static str),
    #[error("SVG element <{0}> is not allowed in a static image")]
    ElementNotAllowed(String),
    #[error("SVG element <{0}> is not in the SVG namespace")]
    ForeignNamespace(String),
    #[error("SVG document root must be <svg> in the SVG namespace")]
    InvalidRoot,
    #[error("SVG attribute {0} is not allowed")]
    AttributeNotAllowed(String),
    #[error("SVG {0} must reference a local fragment")]
    ExternalReference(String),
    #[error("SVG exceeds the {0} limit")]
    LimitExceeded(&'static str),
    #[error("SVG has no usable dimensions")]
    MissingDimensions,
}

impl From<AssetError> for ManifestV1Error {
    fn from(error: AssetError) -> Self {
        ManifestV1Error::Invalid {
            field: "metadata.assets",
            reason: error.to_string(),
        }
    }
}

/// The intrinsic dimensions of a passive SVG, or a reject reason.
pub struct SvgInspection {
    pub width: i64,
    pub height: i64,
}

/// Accept the static elements the browser can render without executing or
/// fetching anything. Allowlist (not denylist): new browser features are
/// rejected until they are deliberately reviewed.
const STATIC_SVG_ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "symbol",
    "marker",
    "clipPath",
    "mask",
    "pattern",
    "view",
    "switch",
    "use",
    "desc",
    "title",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "textPath",
    "linearGradient",
    "radialGradient",
    "stop",
];

/// `href` / `xlink:href` / `src` may only reference a same-document fragment.
const FRAGMENT_ONLY_ATTRS: &[&str] = &["href", "src"];

/// Parse an SVG document from UTF-8 bytes and return its intrinsic dimensions.
///
/// The document must be a single well-formed XML tree whose root is `<svg>` in
/// the SVG namespace, built from the static element allowlist, with every
/// attribute value scanned for external references. No bytes are rewritten;
/// callers keep the original bytes for content digesting.
pub fn inspect_svg_markup(bytes: &[u8]) -> Result<SvgInspection, AssetError> {
    let text = strip_utf8_bom(bytes)?;
    reject_disallowed_constructs(text)?;

    let document = roxmltree::Document::parse(text)
        .map_err(|error| AssetError::NotWellFormed(error.to_string()))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NAMESPACE) {
        return Err(AssetError::InvalidRoot);
    }

    let mut visitor = TreeVisitor::default();
    visitor.visit(root)?;

    let root_attrs = root.attributes();
    let attrs: Vec<(&str, &str)> = root_attrs
        .map(|attribute| (attribute.name(), attribute.value()))
        .collect();
    svg_dimensions(&attrs)
}

/// Validate asset bytes against their declared media type. Binary formats are
/// magic-byte checked only (their pixels are never decoded); SVG is run through
/// the passive inspection profile. Mirrors ato-api's `inspectAuthoringImage`.
pub fn validate_asset_bytes(
    media_type: AssetMediaType,
    bytes: &[u8],
) -> Result<SvgInspection, AssetError> {
    match media_type {
        AssetMediaType::Svg => inspect_svg_markup(bytes),
        AssetMediaType::Png => {
            if !bytes.starts_with(&PNG_MAGIC) {
                return Err(AssetError::InvalidBytes {
                    media_type: "image/png",
                });
            }
            Ok(SvgInspection {
                width: 0,
                height: 0,
            })
        }
        AssetMediaType::Jpeg => {
            if !bytes.starts_with(&JPEG_MAGIC) {
                return Err(AssetError::InvalidBytes {
                    media_type: "image/jpeg",
                });
            }
            Ok(SvgInspection {
                width: 0,
                height: 0,
            })
        }
        AssetMediaType::Webp => {
            if !bytes.starts_with(&WEBP_MAGIC)
                || bytes.get(8..12).is_none_or(|tail| tail != b"WEBP")
            {
                return Err(AssetError::InvalidBytes {
                    media_type: "image/webp",
                });
            }
            Ok(SvgInspection {
                width: 0,
                height: 0,
            })
        }
    }
}

/// PNG signature: 0x89 'P' 'N' 'G' 0x0D 0x0A 0x1A 0x0A.
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
/// JPEG signature: 0xFF 0xD8 0xFF.
const JPEG_MAGIC: [u8; 3] = [0xff, 0xd8, 0xff];
/// WebP container: "RIFF" .... "WEBP".
const WEBP_MAGIC: [u8; 4] = [0x52, 0x49, 0x46, 0x46];

/// Decode bytes as strict UTF-8, tolerating a leading UTF-8 BOM. UTF-16
/// encodings fail the UTF-8 check outright.
fn strip_utf8_bom(bytes: &[u8]) -> Result<&str, AssetError> {
    let bytes = if bytes.len() >= 3 && bytes[..3] == [0xef, 0xbb, 0xbf] {
        &bytes[3..]
    } else {
        bytes
    };
    std::str::from_utf8(bytes).map_err(|_| AssetError::InvalidBytes {
        media_type: "image/svg+xml",
    })
}

/// Raw-text fail-closed scan for constructs the parser drops or tolerates but
/// the passive profile forbids: CDATA, processing instructions other than the
/// single leading XML declaration, and markup/entity declarations.
fn reject_disallowed_constructs(text: &str) -> Result<(), AssetError> {
    let mut index = 0usize;
    let mut xml_declaration_seen = false;
    while let Some(lt) = text[index..].find('<') {
        let start = index + lt;
        let rest = &text[start..];

        if rest.starts_with("<!--") {
            let end = text[start + 4..]
                .find("-->")
                .map(|offset| start + 4 + offset);
            let Some(end) = end else {
                return Err(AssetError::DisallowedConstruct("unterminated comment"));
            };
            index = end + 3;
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            return Err(AssetError::DisallowedConstruct("CDATA sections"));
        }
        if rest.starts_with("<!") {
            return Err(AssetError::DisallowedConstruct("markup declarations"));
        }
        if rest.starts_with("<?") {
            let end = text[start + 2..]
                .find("?>")
                .map(|offset| start + 2 + offset);
            let Some(end) = end else {
                return Err(AssetError::DisallowedConstruct(
                    "unterminated processing instruction",
                ));
            };
            let body = text[start + 2..end].trim_start();
            if !xml_declaration_seen && body.to_ascii_lowercase().starts_with("xml") {
                validate_xml_declaration(body)?;
                xml_declaration_seen = true;
            } else {
                return Err(AssetError::DisallowedConstruct("processing instructions"));
            }
            index = end + 2;
            continue;
        }
        index = start + 1;
    }
    Ok(())
}

/// Validate an XML declaration: `xml` must be the first token and any
/// `encoding` attribute must be UTF-8 or US-ASCII.
fn validate_xml_declaration(body: &str) -> Result<(), AssetError> {
    let lower = body.to_ascii_lowercase();
    if !lower.starts_with("xml") {
        return Err(AssetError::DisallowedConstruct("processing instructions"));
    }
    if let Some(start) = lower.find("encoding") {
        let after = &lower[start + "encoding".len()..];
        let Some(rest) = after.strip_prefix(|c: char| c == ' ' || c == '\t') else {
            return Err(AssetError::DisallowedConstruct("malformed XML declaration"));
        };
        let rest = rest
            .strip_prefix('=')
            .ok_or(AssetError::DisallowedConstruct("malformed XML declaration"))?;
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix('"')
            .or_else(|| rest.strip_prefix('\''))
            .ok_or(AssetError::DisallowedConstruct("malformed XML declaration"))?;
        let value = rest
            .split_once('"')
            .or_else(|| rest.split_once('\''))
            .map(|(value, _)| value)
            .ok_or(AssetError::DisallowedConstruct("malformed XML declaration"))?;
        if !value.eq_ignore_ascii_case("utf-8") && !value.eq_ignore_ascii_case("us-ascii") {
            return Err(AssetError::DisallowedConstruct("non-UTF-8 encoding"));
        }
    }
    Ok(())
}

/// A depth/count-bounded walk over the parsed tree enforcing the allowlist,
/// namespace identity, and attribute rules.
#[derive(Default)]
struct TreeVisitor {
    depth: usize,
    count: usize,
}

impl TreeVisitor {
    fn visit(&mut self, node: roxmltree::Node<'_, '_>) -> Result<(), AssetError> {
        self.depth += 1;
        self.count += 1;
        if self.count > MAX_SVG_ELEMENTS {
            return Err(AssetError::LimitExceeded("element count"));
        }
        if self.depth > MAX_SVG_DEPTH {
            return Err(AssetError::LimitExceeded("nesting depth"));
        }

        let tag = node.tag_name();
        if tag.namespace() != Some(SVG_NAMESPACE) {
            return Err(AssetError::ForeignNamespace(tag.name().to_string()));
        }
        if !STATIC_SVG_ELEMENTS.contains(&tag.name()) {
            return Err(AssetError::ElementNotAllowed(tag.name().to_string()));
        }

        let attributes = node.attributes();
        if attributes.len() > MAX_SVG_ATTRS_PER_ELEMENT {
            return Err(AssetError::LimitExceeded("attribute count"));
        }
        for attribute in attributes {
            validate_attribute(attribute.name(), attribute.namespace(), attribute.value())?;
        }

        for child in node.children() {
            if child.is_element() {
                self.visit(child)?;
            } else if child.is_pi() {
                return Err(AssetError::DisallowedConstruct("processing instructions"));
            }
        }
        self.depth -= 1;
        Ok(())
    }
}

/// Validate a single attribute on an SVG element. The local name is judged
/// against the namespace-aware rule set.
fn validate_attribute(name: &str, namespace: Option<&str>, value: &str) -> Result<(), AssetError> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("on") {
        return Err(AssetError::AttributeNotAllowed(format!(
            "{name} event handler"
        )));
    }
    if lower == "xml:base"
        || (namespace == Some("http://www.w3.org/XML/1998/namespace") && name == "base")
    {
        return Err(AssetError::AttributeNotAllowed("xml:base".to_string()));
    }

    let trimmed = value.trim();
    if FRAGMENT_ONLY_ATTRS.contains(&name) {
        let fragment = trimmed.strip_prefix('#').unwrap_or("");
        if !is_fragment_name(fragment) {
            return Err(AssetError::ExternalReference(format!(
                "{name} must be a same-document fragment (#local)"
            )));
        }
        return Ok(());
    }

    if lower == "style" || value.to_ascii_lowercase().contains("url(") {
        validate_css_value(name, value)?;
    }

    let lower_value = trimmed.to_ascii_lowercase();
    for prefix in ["javascript:", "data:", "file:", "//", "\\"] {
        if lower_value.starts_with(prefix) {
            return Err(AssetError::ExternalReference(format!(
                "{name} must not carry a scripted or external value"
            )));
        }
    }
    Ok(())
}

/// A fragment id must be a bare NCName: a letter or `_` first, then letters,
/// digits, `_`, `-`, `.` or `:`.
fn is_fragment_name(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':'))
}

/// Scan every `url(...)` in a value and require fragment-only targets. Also
/// reject `@import` and backslash escapes.
fn validate_css_value(name: &str, value: &str) -> Result<(), AssetError> {
    if value.to_ascii_lowercase().contains("@import") {
        return Err(AssetError::AttributeNotAllowed(format!(
            "{name} must not import external CSS"
        )));
    }
    if value.contains('\\') {
        return Err(AssetError::AttributeNotAllowed(format!(
            "{name} contains a disallowed escape"
        )));
    }
    let mut rest = value;
    while let Some(start) = rest.to_ascii_lowercase().find("url(") {
        let tail = &rest[start + 4..];
        let tail = tail
            .trim_start()
            .strip_prefix('"')
            .or_else(|| tail.trim_start().strip_prefix('\''))
            .unwrap_or(tail.trim_start());
        let end = tail.find([')', '"', '\'', ' ']).unwrap_or(tail.len());
        let raw = tail[..end].trim();
        let id = raw.strip_prefix('#').unwrap_or(raw);
        if !is_fragment_name(id) {
            return Err(AssetError::ExternalReference(format!(
                "{name} url() must reference a local fragment"
            )));
        }
        rest = &tail[end..];
    }
    Ok(())
}

/// Derive intrinsic dimensions: fixed width+height (bare or `px`) first, then
/// `viewBox` (`min-x min-y width height`), else reject.
fn svg_dimensions(attrs: &[(&str, &str)]) -> Result<SvgInspection, AssetError> {
    let get = |key: &str| {
        attrs
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| *value)
    };

    let fixed_width = get("width").and_then(parse_fixed_dimension);
    let fixed_height = get("height").and_then(parse_fixed_dimension);
    if let (Some(width), Some(height)) = (fixed_width, fixed_height) {
        return Ok(SvgInspection { width, height });
    }

    if let Some(view_box) = get("viewBox").or_else(|| get("viewbox")) {
        let (width, height) = parse_view_box(view_box)?;
        return Ok(SvgInspection { width, height });
    }

    Err(AssetError::MissingDimensions)
}

/// Parse a fixed dimension: a positive number, optionally `px`-suffixed.
fn parse_fixed_dimension(value: &str) -> Option<i64> {
    let value = value.trim().trim_end_matches("px").trim();
    let raw: f64 = value.parse().ok()?;
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    let dim = raw.ceil() as i64;
    if dim <= 0 || dim > MAX_DIMENSION {
        return None;
    }
    Some(dim)
}

/// Parse a `viewBox` — four whitespace/comma separated numbers. The last two
/// are the width and height.
fn parse_view_box(value: &str) -> Result<(i64, i64), AssetError> {
    let parts: Vec<f64> = value
        .split([' ', ',', '\t', '\n', '\r'])
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    if parts.len() != 4 {
        return Err(AssetError::MissingDimensions);
    }
    let width = parts[2].ceil() as i64;
    let height = parts[3].ceil() as i64;
    if width <= 0 || height <= 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(AssetError::MissingDimensions);
    }
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(markup: &str) -> Result<SvgInspection, AssetError> {
        inspect_svg_markup(markup.as_bytes())
    }

    #[test]
    fn accepts_a_safe_static_svg() {
        let inspected = svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32"/></svg>"#,
        )
        .expect("safe svg");
        assert_eq!(inspected.width, 32);
        assert_eq!(inspected.height, 32);
    }

    #[test]
    fn derives_dimensions_from_view_box_when_no_fixed_size() {
        let inspected =
            svg(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 200"></svg>"#)
                .expect("viewBox svg");
        assert_eq!((inspected.width, inspected.height), (100, 200));
    }

    #[test]
    fn falls_back_to_view_box_for_relative_dimensions() {
        let inspected = svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%" viewBox="0 0 256 256"></svg>"#,
        )
        .expect("relative svg");
        assert_eq!((inspected.width, inspected.height), (256, 256));
    }

    #[test]
    fn accepts_xml_declaration_without_encoding() {
        let inspected = svg(
            "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\"></svg>",
        )
        .expect("declaration svg");
        assert_eq!((inspected.width, inspected.height), (24, 24));
    }

    #[test]
    fn rejects_doctype_and_entities() {
        assert!(
            svg(
                "<!DOCTYPE svg><svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>"
            )
            .is_err()
        );
        assert!(
            svg("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\">&foo;</svg>")
                .is_err()
        );
    }

    #[test]
    fn rejects_cdata_and_processing_instructions() {
        assert!(matches!(
            svg(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><![CDATA[\n]]></svg>"
            ),
            Err(AssetError::DisallowedConstruct("CDATA sections"))
        ));
        assert!(svg("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><?php x() ?></svg>").is_err());
    }

    #[test]
    fn rejects_mismatched_and_multiple_roots() {
        assert!(svg("<svg xmlns=\"http://www.w3.org/2000/svg\"><g></svg>").is_err());
        assert!(svg("<svg xmlns=\"http://www.w3.org/2000/svg\"/><svg xmlns=\"http://www.w3.org/2000/svg\"/>").is_err());
    }

    #[test]
    fn rejects_non_svg_root_and_foreign_namespace() {
        assert!(svg("<div width=\"1\" height=\"1\"/>").is_err());
        assert!(matches!(
            svg(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><m:xhtml xmlns:m=\"http://www.w3.org/1999/xhtml\"/></svg>"
            ),
            Err(AssetError::ForeignNamespace(_))
        ));
    }

    #[test]
    fn rejects_active_content_via_allowlist() {
        for active in [
            "<script>alert(1)</script>",
            "<foreignObject/>",
            "<iframe/>",
            "<object/>",
            "<animate/>",
            "<set/>",
            "<style>body{}</style>",
            "<a>x</a>",
        ] {
            assert!(
                svg(&format!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"32\">{active}</svg>"
                ))
                .is_err(),
                "should reject {active}"
            );
        }
    }

    #[test]
    fn rejects_namespace_aliased_active_content() {
        assert!(svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:s="http://www.w3.org/2000/svg" width="32" height="32"><s:script>alert(1)</s:script></svg>"#,
        )
        .is_err());
        assert!(svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:f="http://www.w3.org/2000/svg" width="32" height="32"><f:foreignObject><div>hi</div></f:foreignObject></svg>"#,
        )
        .is_err());
    }

    #[test]
    fn rejects_event_handlers_and_xml_base() {
        assert!(svg("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\" onload=\"x()\"/>").is_err());
        assert!(svg("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\" xml:base=\"https://evil\"><use href=\"#x\"/></svg>").is_err());
    }

    #[test]
    fn rejects_external_and_allows_fragment_references() {
        assert!(svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><image href="https://example.com/x.png"/></svg>"#,
        )
        .is_err());
        assert!(svg(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><defs><linearGradient id="g"/></defs><rect fill="url(#g)"/><use href="#g"/></svg>"##,
        )
        .is_ok());
    }

    #[test]
    fn accepts_the_static_logo_fixture_pattern() {
        let inspected = svg(
            "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100%\" height=\"100%\" viewBox=\"0 0 256 256\"><defs><linearGradient id=\"bg\"><stop offset=\"0\" stop-color=\"#22d3ee\"/></linearGradient></defs><rect width=\"256\" height=\"256\" rx=\"48\" fill=\"url(#bg)\"/></svg>",
        )
        .expect("logo fixture");
        assert_eq!((inspected.width, inspected.height), (256, 256));
    }

    #[test]
    fn rejects_non_utf8_bytes() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend_from_slice(
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>",
        );
        assert!(matches!(
            inspect_svg_markup(&bytes),
            Err(AssetError::InvalidBytes { .. })
        ));
    }

    #[test]
    fn validates_media_type_strings() {
        assert_eq!(
            AssetMediaType::parse("image/png").expect("png"),
            AssetMediaType::Png
        );
        assert_eq!(
            AssetMediaType::parse("image/jpeg").expect("jpeg"),
            AssetMediaType::Jpeg
        );
        assert_eq!(
            AssetMediaType::parse("image/webp").expect("webp"),
            AssetMediaType::Webp
        );
        assert_eq!(
            AssetMediaType::parse("image/svg+xml").expect("svg"),
            AssetMediaType::Svg
        );
        assert!(matches!(
            AssetMediaType::parse("image/gif"),
            Err(AssetError::UnsupportedMediaType(_))
        ));
        assert_eq!(AssetMediaType::Svg.as_str(), "image/svg+xml");
    }

    #[test]
    fn validates_binary_magic_bytes() {
        let png: Vec<u8> = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
            .into_iter()
            .chain([0x00, 0x00, 0x00, 0x0d])
            .collect();
        assert!(validate_asset_bytes(AssetMediaType::Png, &png).is_ok());
        assert!(validate_asset_bytes(AssetMediaType::Png, b"not a png").is_err());

        let jpeg: Vec<u8> = vec![0xff, 0xd8, 0xff, 0xe0];
        assert!(validate_asset_bytes(AssetMediaType::Jpeg, &jpeg).is_ok());
        assert!(validate_asset_bytes(AssetMediaType::Jpeg, b"gif").is_err());

        let mut webp = vec![0x52, 0x49, 0x46, 0x46, 0x00, 0x00, 0x00, 0x00];
        webp.extend_from_slice(b"WEBPVP8 ");
        assert!(validate_asset_bytes(AssetMediaType::Webp, &webp).is_ok());
        assert!(validate_asset_bytes(AssetMediaType::Webp, b"RIFF0000XXXX").is_err());

        assert!(
            validate_asset_bytes(
                AssetMediaType::Svg,
                b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"/>"
            )
            .is_ok()
        );
        assert!(validate_asset_bytes(
            AssetMediaType::Svg,
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"><script/></svg>"
        )
        .is_err());
    }
}
