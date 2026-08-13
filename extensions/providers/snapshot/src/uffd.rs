//! Firecracker UFFD `mem_backend` capability probe (#853, U0).
//!
//! Firecracker's `PUT /snapshot/load` accepts a `mem_backend` of `backend_type`
//! `File` **or** `Uffd`. With `Uffd`, guest memory is faulted in lazily over a
//! Unix socket by a page-server, instead of being `mmap`'d from a file. Ato
//! hard-codes `File` today; **U0 only makes Ato truthfully report whether this
//! host could drive `Uffd`** — there is no restore path, no page-server, and no
//! `ato run` change here (see `docs/ready-state/uffd-mem-backend.md`).
//!
//! The decision is a pure function ([`evaluate`]) of host facts, so it is
//! unit-tested KVM-free; [`host_userfaultfd_present`] is the only side-effecting
//! probe.

/// Minimum Firecracker version whose swagger declares the `Uffd` `mem_backend`
/// type. UFFD memory backends are stable from the 1.x `mem_backend` API; we pin
/// conservatively so older binaries are reported unsupported with a clear reason.
pub(crate) const UFFD_MIN_FC_VERSION: &str = "1.0.0";

/// Decide whether a Firecracker backend can drive a `Uffd` `mem_backend` on a
/// host with these facts (pure). U0 requires **all** of: x86_64, `/dev/kvm`,
/// Firecracker ≥ [`UFFD_MIN_FC_VERSION`], and kernel `userfaultfd`. Anything else
/// is `(false, Some(reason))` — fail-closed but introspectable, never a panic.
pub(crate) fn evaluate(
    arch: &str,
    kvm_present: bool,
    fc_version: Option<&str>,
    userfaultfd_present: bool,
) -> (bool, Option<String>) {
    if arch != "x86_64" {
        // aarch64 UFFD is a separate pass (#852 tracks it).
        return (false, Some(format!("{arch} not in U0 scope (x86_64 only)")));
    }
    if !kvm_present {
        return (false, Some("/dev/kvm not present".to_string()));
    }
    let Some(version) = fc_version else {
        return (false, Some("firecracker binary not found".to_string()));
    };
    if !fc_version_ge(version, UFFD_MIN_FC_VERSION) {
        return (
            false,
            Some(format!("firecracker {version} < {UFFD_MIN_FC_VERSION}")),
        );
    }
    if !userfaultfd_present {
        return (
            false,
            Some("userfaultfd disabled on host (no CONFIG_USERFAULTFD)".to_string()),
        );
    }
    (true, None)
}

/// Whether the host kernel exposes `userfaultfd`. `CONFIG_USERFAULTFD=y` creates
/// `/proc/sys/vm/unprivileged_userfaultfd` (its value 0/1 is the *unprivileged*
/// access policy; presence alone means the syscall exists). Non-Linux → false.
/// U1+ will do the real `userfaultfd(2)` + page-server handshake; U0 only probes
/// kernel support.
pub(crate) fn host_userfaultfd_present() -> bool {
    cfg!(target_os = "linux")
        && std::path::Path::new("/proc/sys/vm/unprivileged_userfaultfd").exists()
}

/// `found >= min` over leading numeric `major.minor.patch` (pre-release/build
/// suffixes are ignored: `"1.10.1-dev"` → `(1, 10, 1)`).
fn fc_version_ge(found: &str, min: &str) -> bool {
    parse_semver(found) >= parse_semver(min)
}

fn parse_semver(v: &str) -> (u32, u32, u32) {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|seg| {
        seg.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse::<u32>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_takes_leading_numeric_components() {
        assert_eq!(parse_semver("1.10.1"), (1, 10, 1));
        assert_eq!(parse_semver("v1.0.0"), (1, 0, 0));
        assert_eq!(parse_semver("1.16.0-dev"), (1, 16, 0));
        assert_eq!(parse_semver("0.25.2"), (0, 25, 2));
        assert_eq!(parse_semver("garbage"), (0, 0, 0));
    }

    #[test]
    fn version_ge_is_numeric_not_lexical() {
        assert!(fc_version_ge("1.10.1", "1.0.0"));
        assert!(fc_version_ge("1.0.0", "1.0.0"));
        assert!(!fc_version_ge("0.25.2", "1.0.0"));
        // lexical "1.9" > "1.10" would be wrong; numeric is correct.
        assert!(fc_version_ge("1.10.0", "1.9.0"));
    }

    #[test]
    fn aarch64_is_out_of_u0_scope() {
        let (ok, reason) = evaluate("aarch64", true, Some("1.10.1"), true);
        assert!(!ok);
        assert!(reason.unwrap().contains("U0 scope"));
    }

    #[test]
    fn x86_64_without_kvm_is_unsupported() {
        let (ok, reason) = evaluate("x86_64", false, Some("1.10.1"), true);
        assert!(!ok);
        assert!(reason.unwrap().contains("/dev/kvm"));
    }

    #[test]
    fn missing_firecracker_binary_is_unsupported() {
        let (ok, reason) = evaluate("x86_64", true, None, true);
        assert!(!ok);
        assert!(reason.unwrap().contains("firecracker binary"));
    }

    #[test]
    fn old_firecracker_is_unsupported_with_versions_in_reason() {
        let (ok, reason) = evaluate("x86_64", true, Some("0.25.2"), true);
        assert!(!ok);
        let r = reason.unwrap();
        assert!(
            r.contains("0.25.2") && r.contains(UFFD_MIN_FC_VERSION),
            "{r}"
        );
    }

    #[test]
    fn no_userfaultfd_is_unsupported() {
        let (ok, reason) = evaluate("x86_64", true, Some("1.10.1"), false);
        assert!(!ok);
        assert!(reason.unwrap().contains("userfaultfd"));
    }

    #[test]
    fn all_preconditions_met_is_supported_with_no_reason() {
        let (ok, reason) = evaluate("x86_64", true, Some("1.10.1"), true);
        assert!(ok);
        assert!(reason.is_none());
    }

    #[test]
    fn host_userfaultfd_present_is_false_off_linux() {
        if !cfg!(target_os = "linux") {
            assert!(!host_userfaultfd_present(), "UFFD is Linux-only");
        }
    }
}
