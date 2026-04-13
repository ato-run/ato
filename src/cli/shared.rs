use clap::ValueEnum;

pub(crate) const DEFAULT_RUN_REGISTRY_URL: &str = "https://api.ato.run";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum GitHubAutoFixMode {
    Toml,
    Src,
    All,
}

impl GitHubAutoFixMode {
    pub(crate) fn from_cli_flags(
        auto_fix_toml: bool,
        auto_fix_src: bool,
        auto_fix_all: bool,
    ) -> Option<Self> {
        if auto_fix_all {
            Some(Self::All)
        } else if auto_fix_src {
            Some(Self::Src)
        } else if auto_fix_toml {
            Some(Self::Toml)
        } else {
            None
        }
    }

    pub(crate) fn fixes_generated_toml(self) -> bool {
        matches!(self, Self::Toml | Self::All)
    }
}

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProviderToolchain {
    #[default]
    Auto,
    Uv,
    Npm,
    Bun,
    Pnpm,
}

impl ProviderToolchain {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ProviderToolchain::Auto => "auto",
            ProviderToolchain::Uv => "uv",
            ProviderToolchain::Npm => "npm",
            ProviderToolchain::Bun => "bun",
            ProviderToolchain::Pnpm => "pnpm",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum GitMode {
    /// Pin the current local commit (must be pushed to remote)
    #[default]
    SameCommit,
    /// Fetch the remote branch HEAD at encap time and pin that rev
    LatestAtEncap,
}

impl GitMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            GitMode::SameCommit => "same-commit",
            GitMode::LatestAtEncap => "latest-at-encap",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ShareToolRuntime {
    /// Use ato-managed runtimes for Python/Node; fall back to system for others
    #[default]
    Auto,
    /// Always use ato-managed runtimes (error if not supported)
    Ato,
    /// Always use the system PATH (current v1 behavior)
    System,
}

impl ShareToolRuntime {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ShareToolRuntime::Auto => "auto",
            ShareToolRuntime::Ato => "ato",
            ShareToolRuntime::System => "system",
        }
    }
}

pub(super) fn cli_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Effects};

    clap::builder::Styles::styled()
        .header(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Blue.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Yellow.on_default())
}
