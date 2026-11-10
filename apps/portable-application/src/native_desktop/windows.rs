//! The Windows native desktop Adapter — implemented, and fail-closed.
//!
//! The platform primitives live with the other OS primitives in
//! [`ato_sandbox::windows`]: Authenticode verification through
//! `WinVerifyTrust`, Job Object ownership with `KILL_ON_JOB_CLOSE` and an
//! explicit terminate, and an AppContainer policy probe.
//!
//! Admission is nevertheless refused with a named reason. The mandatory
//! filesystem/state/egress/lifecycle policy needs an AppContainer profile that
//! scopes exactly the Run's workspace, state directory and inbound loopback
//! Ports; that enforcement is not implemented, so claiming containment would
//! be a false statement in the receipt. The refusal names the primitives that
//! are available and the one that is not, and it is produced before anything
//! is launched.

#![cfg(windows)]

use std::path::Path;

use ato_runtime_attempt::realize::{RealizeFailure, Realized};

use super::{NativeRefusal, NativeStart, refusal};

/// The stable admission code while the mandatory Windows policy is not fully
/// enforceable.
pub(crate) const WINDOWS_CONTAINMENT_UNAVAILABLE: &str = "native_windows_containment_unavailable";

/// Observe one file's Authenticode signature. The verdict is evidence; it is
/// never a Contract verdict.
///
/// Used by the tests and by the start path once admission can enforce the
/// mandatory policy; today admission refuses before a file exists to verify.
#[allow(dead_code)]
pub(crate) fn verify_authenticode(path: &Path) -> ato_sandbox::windows::AuthenticodeReport {
    ato_sandbox::windows::verify_authenticode(path)
}

/// The AppContainer policy the mandatory set needs, attempted for real.
pub(crate) fn appcontainer_policy() -> Result<(), NativeRefusal> {
    ato_sandbox::windows::appcontainer_policy_available().map_err(|detail| NativeRefusal {
        code: WINDOWS_CONTAINMENT_UNAVAILABLE,
        detail,
    })
}

/// The platform-specific admission evidence, recorded with the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowsAdmissionEvidence {
    pub authenticode_available: bool,
    pub job_object_available: bool,
    pub appcontainer_detail: String,
    pub state_directory: String,
    pub loopback_ports: Vec<u16>,
}

impl WindowsAdmissionEvidence {
    pub(crate) fn describe(&self) -> String {
        format!(
            "Authenticode verification available: {}; Job Object ownership available: {}; \
             AppContainer policy: {}; planned Run state directory: {}; declared loopback Ports: {}",
            self.authenticode_available,
            self.job_object_available,
            self.appcontainer_detail,
            self.state_directory,
            if self.loopback_ports.is_empty() {
                "none".to_owned()
            } else {
                self.loopback_ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            }
        )
    }
}

/// Probe the primitives this Adapter implements, then refuse.
pub(crate) fn admission_evidence(request: &NativeStart<'_>) -> WindowsAdmissionEvidence {
    WindowsAdmissionEvidence {
        authenticode_available: ato_sandbox::windows::authenticode_verification_available(),
        job_object_available: ato_sandbox::windows::JobObject::create().is_ok(),
        appcontainer_detail: appcontainer_policy()
            .err()
            .map(|refusal| refusal.detail)
            .unwrap_or_else(|| "available".to_owned()),
        state_directory: request
            .runtime_root
            .join("native")
            .join("state")
            .display()
            .to_string(),
        loopback_ports: request.ports.iter().map(|port| port.guest_port).collect(),
    }
}

/// Start is unreachable today: admission refuses first. If it is reached, it
/// probes and refuses again rather than launching without the mandatory set.
pub(crate) fn start(request: NativeStart<'_>) -> Result<Realized, RealizeFailure> {
    let evidence = admission_evidence(&request);
    Err(refusal(
        WINDOWS_CONTAINMENT_UNAVAILABLE,
        format!(
            "Windows native admission is fail-closed until every mandatory policy is enforceable: \
             {}",
            evidence.describe()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_appcontainer_policy_cannot_be_claimed_and_the_refusal_is_named() {
        let refusal = appcontainer_policy().unwrap_err();
        assert_eq!(refusal.code, WINDOWS_CONTAINMENT_UNAVAILABLE);
    }

    #[test]
    fn authenticode_reports_a_verdict_rather_than_assuming_one() {
        let report = verify_authenticode(Path::new(r"C:\Windows\System32\notepad.exe"));
        // Either outcome is acceptable here; the point is that a verdict is
        // observed and carried, not that a system binary is trusted.
        let _ = report.valid;
        assert!(ato_sandbox::windows::authenticode_verification_available());
    }
}
