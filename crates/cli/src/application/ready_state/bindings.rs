//! Ready-State binding-required guard (Phase 5.5).
//!
//! The Ready-State developer-preview restores a sealed artifact but does **not**
//! yet inject runtime bindings (`BindingLease`). A sealed artifact is by contract
//! **pre-bind and secret-free**: no secret/binding/credential values are written
//! into the manifest, CapsuleFS, rootfs, memory, or vmstate. Until binding
//! injection exists, a capsule that *requires* runtime bindings must NOT be run
//! through Ready-State (it would serve without its credentials) — this guard
//! fails it closed with a clear message.
//!
//! Conservative detection: any non-empty `[secrets.*]`, `[bindings.*]`, or
//! `[external.*]` in the manifest means the capsule needs runtime bindings. If
//! future schema adds binding categories not covered here, prefer extending this
//! function rather than letting an unguarded category through.

use anyhow::Result;
use capsule::types::CapsuleManifest;

/// What runtime bindings a capsule declares it needs (names only — never values).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct BindingRequirementReport {
    pub secrets: Vec<String>,
    pub bindings: Vec<String>,
    pub external: Vec<String>,
}

impl BindingRequirementReport {
    pub(crate) fn requires_bindings(&self) -> bool {
        !self.secrets.is_empty() || !self.bindings.is_empty() || !self.external.is_empty()
    }

    /// Non-leaking summary (declared NAMES only, never values).
    pub(crate) fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.secrets.is_empty() {
            parts.push(format!("secrets={:?}", self.secrets));
        }
        if !self.bindings.is_empty() {
            parts.push(format!("bindings={:?}", self.bindings));
        }
        if !self.external.is_empty() {
            parts.push(format!("external={:?}", self.external));
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(" ")
        }
    }
}

/// Which runtime bindings (by declared name) this capsule needs.
pub(crate) fn requires_runtime_bindings(manifest: &CapsuleManifest) -> BindingRequirementReport {
    BindingRequirementReport {
        secrets: manifest.secrets.keys().cloned().collect(),
        bindings: manifest.bindings.keys().cloned().collect(),
        external: manifest.external.keys().cloned().collect(),
    }
}

/// The phase a binding guard is applied in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingGuardMode {
    /// Build/seal: producing a pre-bind, secret-free artifact. Binding *schema*
    /// may be present; binding VALUES are never injected/recorded — so a
    /// binding-required capsule may still be sealed.
    BuildSeal,
    /// Verify-only restore (no user traffic). A binding-required capsule fails
    /// clearly once detected — we can't (and won't) run it without its bindings.
    VerifyOnly,
    /// Long-lived serving (exposes user traffic). A binding-required capsule MUST
    /// fail until `BindingLease` injection is implemented. (Reserved for the
    /// long-lived-serving fast follow; not yet wired into a call site.)
    #[allow(dead_code)]
    Serve,
}

/// Fail closed when a capsule requires runtime bindings that are not wired yet.
/// `BuildSeal` is permitted (the sealed artifact is pre-bind/secret-free);
/// `VerifyOnly` and `Serve` reject a binding-required capsule with a clear error.
pub(crate) fn ensure_no_unwired_runtime_bindings(
    manifest: &CapsuleManifest,
    mode: BindingGuardMode,
) -> Result<()> {
    let report = requires_runtime_bindings(manifest);
    if !report.requires_bindings() {
        return Ok(());
    }
    match mode {
        // Sealing records binding *requirements* (schema) only; it never accepts
        // or writes binding VALUES, so a pre-bind artifact for a binding-required
        // capsule is fine.
        BindingGuardMode::BuildSeal => Ok(()),
        BindingGuardMode::VerifyOnly | BindingGuardMode::Serve => Err(anyhow::anyhow!(
            "Ready-State runtime bindings are not wired yet. This capsule requires runtime \
             bindings: {}. Run without ATO_READY_STATE_ENABLED or use the legacy path until \
             BindingLease injection is implemented.",
            report.summary()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> CapsuleManifest {
        let base = r#"
schema_version = "0.3"
name = "demo"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
run = "python app.py"
port = 8080

[snapshot]
mode = "warm"
"#;
        CapsuleManifest::from_toml(&format!("{base}\n{extra}")).expect("parse")
    }

    #[test]
    fn no_bindings_passes_every_mode() {
        let m = manifest("");
        assert!(!requires_runtime_bindings(&m).requires_bindings());
        for mode in [BindingGuardMode::BuildSeal, BindingGuardMode::VerifyOnly, BindingGuardMode::Serve] {
            assert!(ensure_no_unwired_runtime_bindings(&m, mode).is_ok(), "{mode:?}");
        }
    }

    #[test]
    fn secret_binding_fails_verify_and_serve_but_seal_ok() {
        let m = manifest("[secrets.openai]\nenv = \"OPENAI_API_KEY\"\n");
        let report = requires_runtime_bindings(&m);
        assert!(report.requires_bindings());
        assert!(report.summary().contains("secrets"));
        assert!(ensure_no_unwired_runtime_bindings(&m, BindingGuardMode::BuildSeal).is_ok());
        let err = ensure_no_unwired_runtime_bindings(&m, BindingGuardMode::VerifyOnly).unwrap_err();
        assert!(err.to_string().contains("runtime bindings are not wired"));
        assert!(err.to_string().contains("openai"), "summary names the requirement: {err}");
        assert!(ensure_no_unwired_runtime_bindings(&m, BindingGuardMode::Serve).is_err());
    }

    #[test]
    fn external_capability_requires_bindings() {
        let m = manifest("[external.gdrive]\ntype = \"user_drive\"\n");
        let report = requires_runtime_bindings(&m);
        assert!(report.external.iter().any(|e| e == "gdrive"), "{report:?}");
        assert!(ensure_no_unwired_runtime_bindings(&m, BindingGuardMode::Serve).is_err());
    }

    #[test]
    fn explicit_binding_requires_bindings() {
        let m = manifest("[bindings.session]\nkind = \"oauth\"\n");
        assert!(requires_runtime_bindings(&m).bindings.iter().any(|b| b == "session"));
        assert!(ensure_no_unwired_runtime_bindings(&m, BindingGuardMode::VerifyOnly).is_err());
    }

    #[test]
    fn summary_lists_names_not_values() {
        let m = manifest("[secrets.openai]\nenv = \"OPENAI_API_KEY\"\n");
        let s = requires_runtime_bindings(&m).summary();
        // names only; the guard must never echo a value (there are none here, but
        // the contract is: declared NAMES only).
        assert!(s.contains("openai"));
    }
}
