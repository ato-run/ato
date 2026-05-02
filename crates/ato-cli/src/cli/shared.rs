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

/// Cache strategy selector for `ato run`. Maps onto
/// `dependency_materializer::CacheStrategy` once the per-call resolution
/// (CLI flag → env var → built-in default) has run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum CacheStrategyArg {
    /// Honor `ATO_CACHE_STRATEGY` if set, otherwise default to `none`.
    #[default]
    Auto,
    /// Disable the dependency cache for this run.
    None,
    /// Use the A1 derivation cache: hit/miss on `derivation_hash`.
    Derivation,
}

const ENV_CACHE_STRATEGY: &str = "ATO_CACHE_STRATEGY";

impl CacheStrategyArg {
    /// Resolves the user-facing flag against `ATO_CACHE_STRATEGY` and the
    /// hard default. Returns the concrete materializer cache strategy.
    pub(crate) fn resolve(self) -> crate::application::dependency_materializer::CacheStrategy {
        use crate::application::dependency_materializer::CacheStrategy;
        match self {
            CacheStrategyArg::None => CacheStrategy::None,
            CacheStrategyArg::Derivation => CacheStrategy::DerivationCache,
            CacheStrategyArg::Auto => match std::env::var(ENV_CACHE_STRATEGY) {
                Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                    "derivation" | "derivation_cache" => CacheStrategy::DerivationCache,
                    "none" | "off" | "disabled" | "" => CacheStrategy::None,
                    other => {
                        tracing::warn!(
                            value = %other,
                            "ATO_CACHE_STRATEGY set to an unrecognized value, falling back to none"
                        );
                        CacheStrategy::None
                    }
                },
                Err(_) => CacheStrategy::None,
            },
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

#[cfg(test)]
mod tests {
    use super::CacheStrategyArg;
    use crate::application::dependency_materializer::CacheStrategy;
    use serial_test::serial;

    fn clear_env() {
        std::env::remove_var("ATO_CACHE_STRATEGY");
    }

    #[test]
    #[serial]
    fn explicit_none_resolves_to_none() {
        clear_env();
        assert_eq!(CacheStrategyArg::None.resolve(), CacheStrategy::None);
    }

    #[test]
    #[serial]
    fn explicit_derivation_resolves_to_derivation_cache() {
        clear_env();
        assert_eq!(
            CacheStrategyArg::Derivation.resolve(),
            CacheStrategy::DerivationCache
        );
    }

    #[test]
    #[serial]
    fn auto_without_env_defaults_to_none() {
        clear_env();
        assert_eq!(CacheStrategyArg::Auto.resolve(), CacheStrategy::None);
    }

    #[test]
    #[serial]
    fn auto_honors_env_var_for_derivation() {
        clear_env();
        std::env::set_var("ATO_CACHE_STRATEGY", "derivation");
        let resolved = CacheStrategyArg::Auto.resolve();
        clear_env();
        assert_eq!(resolved, CacheStrategy::DerivationCache);
    }

    #[test]
    #[serial]
    fn auto_honors_env_var_for_none() {
        clear_env();
        std::env::set_var("ATO_CACHE_STRATEGY", "none");
        let resolved = CacheStrategyArg::Auto.resolve();
        clear_env();
        assert_eq!(resolved, CacheStrategy::None);
    }

    #[test]
    #[serial]
    fn unrecognized_env_value_falls_back_to_none() {
        clear_env();
        std::env::set_var("ATO_CACHE_STRATEGY", "yolo");
        let resolved = CacheStrategyArg::Auto.resolve();
        clear_env();
        assert_eq!(resolved, CacheStrategy::None);
    }

    #[test]
    #[serial]
    fn explicit_flag_overrides_env_var() {
        clear_env();
        std::env::set_var("ATO_CACHE_STRATEGY", "derivation");
        let resolved = CacheStrategyArg::None.resolve();
        clear_env();
        assert_eq!(resolved, CacheStrategy::None);
    }
}
