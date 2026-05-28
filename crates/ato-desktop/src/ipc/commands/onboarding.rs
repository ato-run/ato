//! `onboarding.*` commands — onboarding status and completion, system-capsule only.

use super::{spec, IpcCommandSpec};
use crate::ipc::policy::IpcVisibility::SystemCapsule;
use crate::system_capsule::broker::Capability;

pub fn specs() -> Vec<IpcCommandSpec> {
    vec![
        spec("onboarding.getStatus", SystemCapsule, &[]),
        spec(
            "onboarding.complete",
            SystemCapsule,
            &[Capability::OnboardingComplete],
        ),
    ]
}
