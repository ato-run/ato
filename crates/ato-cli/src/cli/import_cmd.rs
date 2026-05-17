use std::path::PathBuf;

use clap::Args;

#[derive(Args, Debug, Clone)]
pub(crate) struct ImportArgs {
    /// GitHub repository: github.com/owner/repo, https://github.com/owner/repo, or owner/repo.
    pub(crate) repo: String,

    /// Existing capsule.toml recipe to evaluate for this import session.
    #[arg(long = "recipe", value_name = "PATH")]
    pub(crate) recipe: Option<PathBuf>,

    /// Run the resolved source in a shadow workspace with the selected recipe.
    #[arg(long = "run", default_value_t = false)]
    pub(crate) run: bool,

    /// Emit machine-readable JSON output.
    #[arg(long = "emit-json", default_value_t = false)]
    pub(crate) emit_json: bool,
}
