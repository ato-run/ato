use clap::ValueEnum;

pub(crate) const DEFAULT_RUN_REGISTRY_URL: &str = "https://api.ato.run";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum EnforcementMode {
    Strict,
    BestEffort,
}

impl EnforcementMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EnforcementMode::Strict => "strict",
            EnforcementMode::BestEffort => "best_effort",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum CompatibilityFallbackBackend {
    Host,
}

impl CompatibilityFallbackBackend {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CompatibilityFallbackBackend::Host => "host",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum RunAgentMode {
    #[default]
    Auto,
    Off,
    Force,
}

pub(super) fn cli_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Effects};

    clap::builder::Styles::styled()
        .header(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Blue.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Yellow.on_default())
}
