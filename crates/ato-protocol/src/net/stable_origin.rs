//! Pure stable-origin key derivation — no I/O, no allocator.
//!
//! This module holds only the deterministic, side-effect-free functions that
//! derive a DNS-safe host label from an opaque `logical_key` string. The
//! key derivation is shared between:
//!
//! - `ato-netd` (daemon) — allocator maps keys to ports, accept loops use
//!   the label for diagnostics.
//! - `ato-desktop` (slice **C**, #298) — `logical_capsule_key_for_stable_origin`
//!   will migrate here once the Desktop switches from `stable_origin_proxy`
//!   to `ato-netd`.
//!
//! **Invariant**: same key → same label, forever. Changing the derivation
//! would invalidate every persisted `stable_origin_ports.json` entry and
//! break bookmarked `http://127.0.0.1:<port>/` origins.
//!
//! # Key conflict note (slice C task)
//!
//! `ato-desktop` currently has two key-derivation paths that disagree on
//! `CapsuleUrl`: `logical_capsule_key_for_stable_origin` (excludes `CapsuleUrl`)
//! vs `stable_origin_key_for_route` (includes it). That conflict is resolved
//! in slice **C** (#298) when the Desktop migrates to `ato-netd`. Until then,
//! this module stays free of `GuestRoute` dependencies.

/// Maximum length of a DNS label (RFC 1035, §2.3.4).
pub const MAX_DNS_LABEL_LEN: usize = 63;

/// Maximum number of characters taken from the readable slug prefix before
/// the fixed-width hash suffix.
pub const MAX_SLUG_LEN: usize = 24;

/// Derive a DNS-safe host label from an opaque `logical_key` string.
///
/// The label has the form `<slug>-<hash16>` where:
/// - `<slug>` is up to [`MAX_SLUG_LEN`] ASCII-alphanumeric chars from the key
/// - `<hash16>` is 16 lowercase hex digits (64-bit FNV-1a hash of the key)
/// - The combined label is truncated to [`MAX_DNS_LABEL_LEN`] if necessary
///
/// The output satisfies RFC 1035 DNS label syntax: all lowercase, starts and
/// ends with alphanumeric characters, contains only `[a-z0-9-]`.
///
/// # Stability
///
/// This function is stable. Same input always produces the same output.
/// Do not change the algorithm or the hash seed.
pub fn stable_host_label_for_key(logical_key: &str) -> String {
    let slug = host_slug(logical_key);
    let hash = fnv1a64(logical_key.as_bytes());
    let label = format!("{slug}-{hash:016x}");
    label[..label.len().min(MAX_DNS_LABEL_LEN)].to_string()
}

fn host_slug(key: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in key.chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if normalized == '-' {
            if !prev_dash {
                slug.push('-');
                prev_dash = true;
            }
        } else {
            slug.push(normalized);
            prev_dash = false;
        }
        if slug.len() >= MAX_SLUG_LEN {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "capsule".to_string()
    } else {
        slug.to_string()
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Construct the stable logical key for a capsule identified by its handle.
///
/// The returned key is the canonical form used in
/// `${ATO_HOME}/state/netd/stable_origin_ports.json`. Changing this
/// function invalidates all persisted port assignments.
pub fn logical_key_for_handle(handle: &str) -> String {
    format!("handle:{handle}")
}

/// Construct the stable logical key for an ephemeral capsule session ID.
///
/// Used for `GuestRoute::Capsule { session, .. }` routes where the
/// identity is the session ID rather than the handle.
pub fn logical_key_for_session(session_id: &str) -> String {
    format!("session:{session_id}")
}

/// Check whether `label` is a valid DNS label per RFC 1035 §2.3.4.
///
/// Public so callers can validate host labels they receive on the wire
/// without depending on the full validation stack.
pub fn is_valid_dns_label(label: &str) -> bool {
    if label.is_empty() || label.len() > MAX_DNS_LABEL_LEN {
        return false;
    }
    let bytes = label.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic() {
        let key = "handle:capsule://ato.run/org/demo@1.2.3";
        assert_eq!(
            stable_host_label_for_key(key),
            stable_host_label_for_key(key)
        );
    }

    #[test]
    fn output_is_a_valid_dns_label() {
        let cases = [
            "handle:capsule://ato.run/org/demo@1.2.3",
            "session:abc-def-0123",
            "handle:my-capsule",
            "",       // empty key → slug falls back to "capsule"
            "!!!---", // all non-alnum → slug falls back to "capsule"
        ];
        for key in cases {
            let label = stable_host_label_for_key(key);
            assert!(
                is_valid_dns_label(&label),
                "key={key:?} produced invalid DNS label: {label:?}"
            );
        }
    }

    #[test]
    fn different_keys_produce_different_labels() {
        let a = stable_host_label_for_key("handle:org/a@1.0.0");
        let b = stable_host_label_for_key("handle:org/b@1.0.0");
        assert_ne!(a, b);
    }

    #[test]
    fn logical_key_for_handle_formats_correctly() {
        assert_eq!(
            logical_key_for_handle("capsule://org/demo@1.0.0"),
            "handle:capsule://org/demo@1.0.0"
        );
        assert_eq!(logical_key_for_handle(""), "handle:");
    }

    #[test]
    fn logical_key_for_session_formats_correctly() {
        assert_eq!(
            logical_key_for_session("abc-def-0123"),
            "session:abc-def-0123"
        );
        assert_eq!(logical_key_for_session(""), "session:");
    }

    #[test]
    fn logical_key_for_handle_and_session_do_not_collide() {
        let same_suffix = "abc";
        assert_ne!(
            logical_key_for_handle(same_suffix),
            logical_key_for_session(same_suffix),
        );
    }
}
