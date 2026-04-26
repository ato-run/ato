use globset::{Glob, GlobSet, GlobSetBuilder};

#[derive(Debug, Clone, Default)]
pub(crate) enum SecretPolicy {
    #[default]
    AllowAll,
    AllowList(GlobSet),
    DenyList(GlobSet),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PolicyResult {
    Allow,
    Deny,
}

impl SecretPolicy {
    pub(crate) fn allow_list(patterns: &[String]) -> anyhow::Result<Self> {
        let gs = build_glob_set(patterns)?;
        Ok(Self::AllowList(gs))
    }

    pub(crate) fn deny_list(patterns: &[String]) -> anyhow::Result<Self> {
        let gs = build_glob_set(patterns)?;
        Ok(Self::DenyList(gs))
    }

    pub(crate) fn check(&self, capsule_id: &str) -> PolicyResult {
        match self {
            Self::AllowAll => PolicyResult::Allow,
            Self::AllowList(gs) => {
                if gs.is_match(capsule_id) {
                    PolicyResult::Allow
                } else {
                    PolicyResult::Deny
                }
            }
            Self::DenyList(gs) => {
                if gs.is_match(capsule_id) {
                    PolicyResult::Deny
                } else {
                    PolicyResult::Allow
                }
            }
        }
    }
}

fn build_glob_set(patterns: &[String]) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        let glob =
            Glob::new(p).map_err(|e| anyhow::anyhow!("Invalid ACL glob pattern '{}': {}", p, e))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build ACL glob set: {}", e))
}
