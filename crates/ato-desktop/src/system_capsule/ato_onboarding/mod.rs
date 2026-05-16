use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::config::{load_config, save_config, DesktopConfig};
use crate::system_capsule::broker::{BrokerError, Capability};

pub const ONBOARDING_VERSION: u16 = 1;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnboardingCommand {
    Complete {
        version: u16,
        #[serde(default)]
        skipped: bool,
    },
}

impl OnboardingCommand {
    pub fn required_capability(&self) -> Capability {
        Capability::OnboardingComplete
    }
}

pub fn should_show_onboarding(config: &DesktopConfig) -> bool {
    !config.desktop.onboarding.completed && config.desktop.onboarding.version < ONBOARDING_VERSION
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: OnboardingCommand,
) -> Result<(), BrokerError> {
    match command {
        OnboardingCommand::Complete { version, skipped } => {
            let mut config = load_config();
            config.desktop.onboarding.completed = true;
            config.desktop.onboarding.skipped = skipped;
            config.desktop.onboarding.version = version.max(ONBOARDING_VERSION);
            let startup_surface = config.desktop.startup_surface;
            save_config(&config);

            let _ = host.update(cx, |_, window, _| window.remove_window());

            crate::window::open_configured_startup_surface(cx, startup_surface)
                .map_err(|err| BrokerError::Internal(err.to_string()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DesktopConfig;
    use crate::system_capsule::broker::Capability;

    #[test]
    fn should_show_onboarding_for_default_config() {
        assert!(should_show_onboarding(&DesktopConfig::default()));
    }

    #[test]
    fn should_not_show_when_completed() {
        let mut cfg = DesktopConfig::default();
        cfg.desktop.onboarding.completed = true;
        cfg.desktop.onboarding.version = ONBOARDING_VERSION;
        assert!(!should_show_onboarding(&cfg));
    }

    #[test]
    fn skipped_and_completed_stays_hidden() {
        let mut cfg = DesktopConfig::default();
        cfg.desktop.onboarding.completed = true;
        cfg.desktop.onboarding.skipped = true;
        cfg.desktop.onboarding.version = ONBOARDING_VERSION;
        assert!(!should_show_onboarding(&cfg));
    }

    #[test]
    fn complete_requires_onboarding_capability() {
        let cmd = OnboardingCommand::Complete {
            version: ONBOARDING_VERSION,
            skipped: false,
        };
        assert_eq!(cmd.required_capability(), Capability::OnboardingComplete);
    }
}
