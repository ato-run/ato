//! v1.2 PR 2: the Ready-State **binding grant surface** — where a declared
//! secret binding meets the user's SecretStore.
//!
//! A grant is *the secret value being present in the capsule-scoped SecretStore
//! namespace* (`rs-<hash16>`): granting = `ato secrets set <NAME> --namespace
//! <ns>`, revoking = `ato secrets delete <NAME> --namespace <ns>` (or a store
//! rekey). There is no separate grant registry to drift from the value — a
//! revoked/deleted value IS the revoked grant, so launch preflight and the
//! renewal loop both fail closed on exactly the state the user controls.
//! Scoping by capsule-manifest hash means another app can never resolve this
//! app's grant (the namespace differs), satisfying the v1.2 contract's
//! per-app grant requirement.

use anyhow::Result;
use protocol::binding_lease::SecretValue;

use super::secret_resolver::SecretResolver;

/// Max description length surfaced in UI/preflight (v1.2 contract §7.1 — the
/// #944 review's non-blocker, decided here): longer text is truncated with an
/// ellipsis. Descriptions are PLAIN TEXT: render as a text node, never HTML.
pub(crate) const DESCRIPTION_MAX_CHARS: usize = 200;

/// The SecretStore namespace for a capsule's Ready-State binding grants:
/// `rs-<first 16 hex of the capsule manifest hash>`. Hash-scoped (not
/// name-scoped) so a different manifest — even same-named — cannot read
/// another app's grants.
pub(crate) fn binding_namespace(capsule_manifest_hash: &str) -> String {
    let hex: String = capsule_manifest_hash
        .strip_prefix("blake3:")
        .unwrap_or(capsule_manifest_hash)
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(16)
        .collect();
    format!("rs-{hex}")
}

/// The exact command a user runs to grant a binding (shown in preflight
/// failures — actionable, never generic).
pub(crate) fn grant_hint(binding_name: &str, namespace: &str) -> String {
    format!("ato secrets set {binding_name} --namespace {namespace}")
}

/// Sanitize a manifest-declared secret description for display: control
/// characters (incl. newlines) collapse to single spaces, whitespace is
/// squeezed, and the result is truncated to [`DESCRIPTION_MAX_CHARS`]. The
/// output is plain text — UIs must render it as a text node (never markup).
pub(crate) fn sanitize_description(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(DESCRIPTION_MAX_CHARS + 1));
    let mut last_space = true; // leading whitespace is dropped
    for c in raw.chars() {
        let c = if c.is_control() || c.is_whitespace() { ' ' } else { c };
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
        if out.chars().count() > DESCRIPTION_MAX_CHARS {
            break;
        }
    }
    let mut out = out.trim_end().to_string();
    if out.chars().count() > DESCRIPTION_MAX_CHARS {
        out = out.chars().take(DESCRIPTION_MAX_CHARS).collect::<String>();
        out.push('…');
    }
    out
}

/// Launch preflight: resolve EVERY declared binding name to its value BEFORE
/// the restore starts, aggregating **all** missing grants into one actionable
/// error (name + sanitized description + the exact grant command) instead of
/// failing on the first. Values are returned for lease issuance and never
/// logged; the error carries names/reasons only.
pub(crate) fn preflight_resolve(
    resolver: &dyn SecretResolver,
    names: &[String],
    manifest: &capsule::types::CapsuleManifest,
    namespace: &str,
) -> Result<Vec<(String, SecretValue)>> {
    let mut resolved = Vec::with_capacity(names.len());
    let mut missing: Vec<String> = Vec::new();
    for name in names {
        match resolver.resolve(name) {
            Ok(value) => resolved.push((name.clone(), value)),
            Err(e) => {
                let description = manifest
                    .secrets
                    .get(name)
                    .and_then(|s| s.description.as_deref())
                    .map(sanitize_description)
                    .filter(|d| !d.is_empty());
                let purpose = description.map(|d| format!(" — {d}")).unwrap_or_default();
                missing.push(format!(
                    "  {name}{purpose}\n    reason: {e}\n    grant:  {}",
                    grant_hint(name, namespace)
                ));
            }
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "Ready-State launch blocked: {} binding(s) not granted (resolver: {}).\n{}\n\
             Grant the missing binding(s), then re-run. Nothing was restored.",
            missing.len(),
            resolver.kind(),
            missing.join("\n")
        );
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_is_hash_scoped_and_short() {
        let ns = binding_namespace("blake3:252d51aabbccdd00112233445566778899aabbccddeeff00");
        assert_eq!(ns, "rs-252d51aabbccdd00");
        // Missing prefix / short input still yields a usable namespace.
        assert_eq!(binding_namespace("abc"), "rs-abc");
    }

    #[test]
    fn description_sanitizes_control_chars_and_truncates() {
        assert_eq!(sanitize_description("  line1\nline2\t x  "), "line1 line2 x");
        assert_eq!(sanitize_description("<b>bold</b>"), "<b>bold</b>"); // plain text, not stripped — render as text node
        let long = "あ".repeat(300);
        let s = sanitize_description(&long);
        assert_eq!(s.chars().count(), DESCRIPTION_MAX_CHARS + 1); // 200 + ellipsis
        assert!(s.ends_with('…'));
    }

    struct OnlyFoo;
    impl SecretResolver for OnlyFoo {
        fn resolve(&self, name: &str) -> Result<SecretValue> {
            if name == "FOO" {
                Ok(SecretValue::new("v"))
            } else {
                anyhow::bail!("no grant for '{name}'")
            }
        }
        fn kind(&self) -> &'static str {
            "test"
        }
    }

    #[test]
    fn preflight_aggregates_all_missing_bindings_with_hints() {
        let manifest = capsule::types::CapsuleManifest::from_toml(
            r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "web"

[targets.web]
runtime = "source"
run = "python3 app.py"
port = 8080

[secrets.BAR]
required = true
description = "Bar key\nwith a newline"
"#,
        )
        .expect("parse");
        let names = vec!["FOO".to_string(), "BAR".to_string(), "BAZ".to_string()];
        let err = preflight_resolve(&OnlyFoo, &names, &manifest, "rs-abc")
            .expect_err("missing grants must fail");
        let msg = format!("{err}");
        assert!(msg.contains("2 binding(s) not granted"), "{msg}");
        assert!(msg.contains("BAR — Bar key with a newline"), "{msg}");
        assert!(msg.contains("ato secrets set BAZ --namespace rs-abc"), "{msg}");
        assert!(!msg.contains('\u{0}'));

        let ok = preflight_resolve(&OnlyFoo, &["FOO".to_string()], &manifest, "rs-abc")
            .expect("granted binding resolves");
        assert_eq!(ok.len(), 1);
    }
}
