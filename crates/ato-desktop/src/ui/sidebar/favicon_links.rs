//! Hand-rolled `<link rel="icon">` extractor.
//!
//! The naive `/favicon.ico` → `.svg` → `apple-touch-icon.png` probe in
//! `favicon_candidate_urls` fails for SPA / catch-all servers that
//! 200-respond `text/html` for every path (grok.com, many Vite dev
//! servers, single-page apps). For those origins the only reliable
//! signal is the actual HTML head, where authors declare their icons
//! via `<link rel="icon" href="...">` per HTML Living Standard §4.6.6.
//!
//! This module is a pure URL-list builder: it takes raw HTML + a base
//! URL and returns absolute icon URLs in priority order. Network I/O
//! is the caller's job (see `fetch_favicon_image` in `ui/mod.rs`).

use url::Url;

/// Parse `<link rel="icon|shortcut icon|apple-touch-icon|mask-icon">` from
/// an HTML document and resolve hrefs against `base_url`.
///
/// Priority:
///   1. `rel="icon"` (incl. `shortcut icon`) — the standard form.
///   2. `rel="apple-touch-icon"` and `apple-touch-icon-precomposed`.
///   3. `rel="mask-icon"` (Safari pinned-tab; usually monochrome SVG).
/// Within the same rel kind, larger pixel sizes win and `sizes="any"`
/// (typical for SVG) outranks every fixed size.
///
/// Returns an empty Vec on any structural failure (unparseable base
/// URL, no head, no matching links). Callers fall back to the well-
/// known-paths probe.
pub(crate) fn parse_link_icon_candidates(html: &str, base_url: &str) -> Vec<String> {
    let Ok(base) = Url::parse(base_url) else {
        return Vec::new();
    };

    // Restrict the scan to <head>. Favicon links in <body> are
    // non-conformant, and indexing the whole document risks picking up
    // user-generated content (CMS articles linking to images).
    let scan = extract_head(html).unwrap_or(html);
    let bytes = scan.as_bytes();

    let mut candidates: Vec<IconCandidate> = Vec::new();
    let mut cursor = 0;
    while let Some(link_start) = find_next_link_tag(bytes, cursor) {
        let after_link = link_start + 5; // skip "<link"
        let Some(tag_end) = find_tag_end(bytes, after_link) else {
            break;
        };
        let tag_body = &scan[after_link..tag_end];
        let attrs = parse_attributes(tag_body);
        if let Some(candidate) = build_candidate(&attrs, &base) {
            candidates.push(candidate);
        }
        cursor = tag_end + 1;
    }

    candidates.sort_by_key(IconCandidate::priority);
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .map(|c| c.href)
        .filter(|href| seen.insert(href.clone()))
        .collect()
}

#[derive(Debug, Clone)]
struct IconCandidate {
    href: String,
    rel_kind: RelKind,
    /// Largest dimension parsed from `sizes`. `u32::MAX` for `any`.
    /// `0` when no `sizes` attribute or unparseable.
    max_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelKind {
    Icon,
    AppleTouchIcon,
    MaskIcon,
}

impl IconCandidate {
    fn priority(&self) -> (u8, std::cmp::Reverse<u32>) {
        let rel_pri = match self.rel_kind {
            RelKind::Icon => 0,
            RelKind::AppleTouchIcon => 1,
            RelKind::MaskIcon => 2,
        };
        (rel_pri, std::cmp::Reverse(self.max_size))
    }
}

fn build_candidate(attrs: &[(String, String)], base: &Url) -> Option<IconCandidate> {
    let rel = find_attr(attrs, "rel")?;
    let rel_kind = classify_rel(&rel.to_ascii_lowercase())?;
    let href = find_attr(attrs, "href")?;
    let resolved = resolve_href(&href, base)?;
    let max_size = find_attr(attrs, "sizes")
        .as_deref()
        .and_then(parse_max_size)
        .unwrap_or(0);
    Some(IconCandidate {
        href: resolved,
        rel_kind,
        max_size,
    })
}

fn find_attr(attrs: &[(String, String)], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn classify_rel(rel_lower: &str) -> Option<RelKind> {
    let mut tokens = rel_lower.split_ascii_whitespace();
    if rel_lower
        .split_ascii_whitespace()
        .any(|t| t == "apple-touch-icon" || t == "apple-touch-icon-precomposed")
    {
        return Some(RelKind::AppleTouchIcon);
    }
    if rel_lower.split_ascii_whitespace().any(|t| t == "mask-icon") {
        return Some(RelKind::MaskIcon);
    }
    if tokens.any(|t| t == "icon") {
        return Some(RelKind::Icon);
    }
    None
}

fn parse_max_size(sizes: &str) -> Option<u32> {
    let lower = sizes.to_ascii_lowercase();
    if lower.split_ascii_whitespace().any(|t| t == "any") {
        return Some(u32::MAX);
    }
    lower
        .split_ascii_whitespace()
        .filter_map(|tok| {
            let (w, h) = tok.split_once('x')?;
            let w: u32 = w.parse().ok()?;
            let h: u32 = h.parse().ok()?;
            Some(w.max(h))
        })
        .max()
}

fn resolve_href(href: &str, base: &Url) -> Option<String> {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return None;
    }
    let decoded = decode_amp_entities(trimmed);
    base.join(&decoded).ok().map(Into::into)
}

fn decode_amp_entities(s: &str) -> String {
    // `&amp;` is the only entity worth decoding for href URLs in
    // practice; `&` itself is not a legal href character so anything
    // else is either already a percent-encoding or a malformed page
    // we shouldn't be salvaging.
    s.replace("&amp;", "&")
}

fn find_next_link_tag(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 5 <= bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1..i + 5].eq_ignore_ascii_case(b"link") {
            let next = bytes.get(i + 5).copied().unwrap_or(b' ');
            if next.is_ascii_whitespace() || next == b'>' || next == b'/' {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn find_tag_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' => quote = Some(c),
                b'>' => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

fn extract_head(html: &str) -> Option<&str> {
    let lower = html.to_ascii_lowercase();
    let head_open = lower.find("<head")?;
    let after_open = head_open + lower[head_open..].find('>')? + 1;
    let head_close_rel = lower[after_open..].find("</head>")?;
    Some(&html[after_open..after_open + head_close_rel])
}

fn parse_attributes(tag_body: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let bytes = tag_body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'/' {
            break;
        }

        let name_start = i;
        while i < bytes.len()
            && !matches!(bytes[i], b'=' | b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>')
        {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name = tag_body[name_start..i].to_string();

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        let value = if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                String::new()
            } else if bytes[i] == b'"' || bytes[i] == b'\'' {
                let quote = bytes[i];
                i += 1;
                let v_start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let value = tag_body[v_start..i].to_string();
                if i < bytes.len() {
                    i += 1;
                }
                value
            } else {
                // HTML5 unquoted-attribute-value syntax allows `/` mid-value
                // (e.g. `href=/icon.png`). Self-closing `/>` is handled by
                // `find_tag_end` excluding the `>` from `tag_body`, and the
                // `bytes[i] == b'/'` break at the top of the outer loop
                // catches a trailing self-closing `/` that follows
                // whitespace.
                let v_start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                tag_body[v_start..i].to_string()
            }
        } else {
            String::new()
        };

        result.push((name, value));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::parse_link_icon_candidates;

    #[test]
    fn parses_basic_link_icon() {
        let html = r#"<html><head>
            <link rel="icon" href="/favicon.svg">
        </head><body></body></html>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/favicon.svg".to_string()]
        );
    }

    #[test]
    fn handles_legacy_shortcut_icon_rel() {
        let html = r#"<head><link rel="shortcut icon" href="/legacy.ico"></head>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/legacy.ico".to_string()]
        );
    }

    #[test]
    fn prefers_standard_icon_over_apple_touch_icon() {
        let html = r#"
            <head>
                <link rel="apple-touch-icon" sizes="180x180" href="/apple.png">
                <link rel="icon" type="image/svg+xml" href="/icon.svg">
            </head>
        "#;
        let got = parse_link_icon_candidates(html, "https://example.com");
        assert_eq!(got[0], "https://example.com/icon.svg");
        assert!(got.contains(&"https://example.com/apple.png".to_string()));
    }

    #[test]
    fn sorts_by_size_within_same_rel() {
        let html = r#"
            <head>
                <link rel="icon" sizes="16x16" href="/16.png">
                <link rel="icon" sizes="64x64" href="/64.png">
                <link rel="icon" sizes="32x32" href="/32.png">
            </head>
        "#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec![
                "https://example.com/64.png".to_string(),
                "https://example.com/32.png".to_string(),
                "https://example.com/16.png".to_string(),
            ]
        );
    }

    #[test]
    fn sizes_any_outranks_pixel_sizes() {
        let html = r#"
            <head>
                <link rel="icon" sizes="64x64" href="/64.png">
                <link rel="icon" sizes="any" type="image/svg+xml" href="/scalable.svg">
            </head>
        "#;
        let got = parse_link_icon_candidates(html, "https://example.com");
        assert_eq!(got[0], "https://example.com/scalable.svg");
    }

    #[test]
    fn resolves_protocol_relative_and_absolute_hrefs() {
        let html = r#"
            <head>
                <link rel="icon" href="//cdn.example.com/icon.png">
                <link rel="apple-touch-icon" href="https://other.example.com/touch.png">
            </head>
        "#;
        let got = parse_link_icon_candidates(html, "https://example.com");
        assert!(got.contains(&"https://cdn.example.com/icon.png".to_string()));
        assert!(got.contains(&"https://other.example.com/touch.png".to_string()));
    }

    #[test]
    fn handles_single_quoted_attributes() {
        let html = r#"<head><link rel='icon' href='/single.png'></head>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/single.png".to_string()]
        );
    }

    #[test]
    fn handles_unquoted_attribute_values() {
        let html = r#"<head><link rel=icon href=/bare.png></head>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/bare.png".to_string()]
        );
    }

    #[test]
    fn ignores_non_icon_rel_values() {
        let html = r#"
            <head>
                <link rel="stylesheet" href="/style.css">
                <link rel="canonical" href="/canon">
                <link rel="icon" href="/icon.png">
            </head>
        "#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/icon.png".to_string()]
        );
    }

    #[test]
    fn ignores_link_tags_in_body() {
        let html = r#"<head></head><body><link rel="icon" href="/body.png"></body>"#;
        assert!(parse_link_icon_candidates(html, "https://example.com").is_empty());
    }

    #[test]
    fn falls_back_to_full_document_when_head_missing() {
        // HTML5 lets authors omit <head>; some static-site renderers do.
        let html = r#"<!doctype html><html><link rel="icon" href="/x.png"><body></body></html>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/x.png".to_string()]
        );
    }

    #[test]
    fn returns_empty_for_invalid_base_url() {
        let html = r#"<head><link rel="icon" href="/x.png"></head>"#;
        assert!(parse_link_icon_candidates(html, "not a url").is_empty());
    }

    #[test]
    fn decodes_amp_entities_in_href() {
        let html = r#"<head><link rel="icon" href="/icon.png?v=1&amp;t=2"></head>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/icon.png?v=1&t=2".to_string()]
        );
    }

    #[test]
    fn resolves_against_path_base() {
        // `<link href="icon.png">` in `/app/index.html` resolves to `/app/icon.png`.
        let html = r#"<head><link rel="icon" href="icon.png"></head>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com/app/"),
            vec!["https://example.com/app/icon.png".to_string()]
        );
    }

    #[test]
    fn deduplicates_repeated_candidates() {
        let html = r#"
            <head>
                <link rel="icon" href="/icon.svg">
                <link rel="icon" href="/icon.svg">
            </head>
        "#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/icon.svg".to_string()]
        );
    }

    #[test]
    fn tolerates_quoted_gt_inside_attribute_value() {
        // `>` is technically illegal unencoded in attr values but real
        // HTML sometimes carries it (e.g. inline JSON-LD expressions in
        // data-* attrs). Quote-aware tag-end scanning prevents the
        // parser from terminating mid-tag and dropping the href.
        let html = r#"<head><link rel="icon" data-foo="a>b" href="/icon.png"></head>"#;
        assert_eq!(
            parse_link_icon_candidates(html, "https://example.com"),
            vec!["https://example.com/icon.png".to_string()]
        );
    }
}
