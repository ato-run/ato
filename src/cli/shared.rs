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

/// Visibility scope for `ato encap` uploads.
///
/// `Public` maps to the API string `"unlisted"` — the share URL is accessible to anyone with
/// the link but is not listed in any public index. When a true public-index feature is added,
/// the API mapping will change without touching this enum name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EncapVisibility {
    /// Anyone with the URL can access (API: "unlisted"). Default.
    #[default]
    Public,
    /// Organisation-internal access (API: "internal").
    Internal,
    /// Authenticated owner only (API: "private").
    Private,
    /// Local save only — no upload.
    Local,
}

impl EncapVisibility {
    pub(crate) fn as_api_str(self) -> &'static str {
        match self {
            // Maps to "unlisted": URL-accessible but not indexed.
            EncapVisibility::Public => "unlisted",
            EncapVisibility::Internal => "internal",
            EncapVisibility::Private => "private",
            EncapVisibility::Local => "local",
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
