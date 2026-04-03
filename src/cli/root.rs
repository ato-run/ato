use std::path::PathBuf;

use clap::{Parser, Subcommand};

use super::app::AppCommands;
use super::binding::BindingCommands;
use super::config::{ConfigCommands, EngineCommands};
use super::inspect::InspectCommands;
use super::ipc::IpcCommands;
use super::key::KeyCommands;
use super::package::PackageCommands;
use super::profile::ProfileCommands;
use super::project::{ProjectCommands, ScaffoldCommands};
use super::registry::RegistryCommands;
use super::shared::{cli_styles, CompatibilityFallbackBackend, EnforcementMode, RunAgentMode};
use super::source::SourceCommands;
use super::state::StateCommands;

#[derive(Parser)]
#[command(name = "ato")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(styles = cli_styles())]
#[command(help_template = "\
{about-with-newline}
Usage: {usage}

Primary Commands:
  run      Execute a .capsule archive or local project in a sandbox
  build    Pack a project into an immutable .capsule archive
  publish  Publish capsule artifacts to a registry
  install  Install a verified package from the registry
  init     Materialize a durable ato.lock.json baseline for the current project
  search   Search the registry for agent skills and packages

Management:
  ps       List running capsules
  stop     Stop a running capsule
  logs     Show logs of a running capsule
    app      Inspect or adapt app-scoped bootstrap state
  state    Inspect or register persistent state bindings
  binding  Inspect or register host-side service bindings

Auth:
  login    Login to Ato registry
  logout   Logout
  whoami   Show current authentication status

Troubleshooting:
  inspect  Inspect lock-first metadata, preview write-back, diagnostics, and runtime requirements

Options:
{options}

Use 'ato help <command>' for more information.
")]
pub(crate) struct Cli {
    /// Path to nacelle engine binary (overrides NACELLE_PATH)
    #[arg(long)]
    pub(crate) nacelle: Option<PathBuf>,

    /// Emit machine-readable JSON output
    #[arg(long)]
    pub(crate) json: bool,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    #[command(
        next_help_heading = "Primary Commands",
        about = "Run a capsule app or local project",
        trailing_var_arg = true
    )]
    Run {
        /// Local path (./, ../, ~/, /...), provider target (pypi:<package> or pypi:<package>[extra]), store scoped ID (publisher/slug), or GitHub repo (github.com/owner/repo). Default: current directory
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Target label to execute (e.g. static, cli, widget)
        #[arg(short = 't', long = "target")]
        target: Option<String>,

        /// Run in development mode (foreground) with hot-reloading on file changes
        #[arg(long)]
        watch: bool,

        /// Run in background mode (detached)
        #[arg(long)]
        background: bool,

        /// Path to nacelle engine binary (overrides NACELLE_PATH)
        #[arg(long)]
        nacelle: Option<PathBuf>,

        /// Registry URL for auto-install when app-id is not installed (default: https://api.ato.run)
        #[arg(long)]
        registry: Option<String>,

        /// Explicitly bind a manifest [state.<name>] entry using STATE=/absolute/path or STATE=state-...
        #[arg(long = "state", value_name = "STATE=/ABS/PATH|STATE=state-...")]
        state: Vec<String>,

        /// Inject external data binding using KEY=VALUE for targets that declare [external_injection]
        #[arg(long = "inject", value_name = "KEY=VALUE")]
        inject: Vec<String>,

        /// Network enforcement mode
        #[arg(long, value_enum, default_value_t = EnforcementMode::Strict)]
        enforcement: EnforcementMode,

        /// Explicitly allow Tier2 (python/native) execution via native OS sandbox
        #[arg(long = "sandbox", default_value_t = false)]
        sandbox_mode: bool,

        /// Legacy alias for `--sandbox`
        #[arg(long = "unsafe", hide = true, default_value_t = false)]
        unsafe_mode_legacy: bool,

        /// Legacy alias for `--sandbox`
        #[arg(long = "unsafe-bypass-sandbox", hide = true, default_value_t = false)]
        unsafe_bypass_sandbox_legacy: bool,

        /// Dangerously bypass all Ato runtime permission/sandbox barriers (host-native execution)
        #[arg(
            short = 'U',
            long = "dangerously-skip-permissions",
            default_value_t = false
        )]
        dangerously_skip_permissions: bool,

        /// Run with an explicit compatibility fallback backend instead of the standard runtime path
        #[arg(long = "compatibility-fallback", value_enum)]
        compatibility_fallback: Option<CompatibilityFallbackBackend>,

        /// Skip prompt and auto-install when app-id is not installed
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Agentic setup recovery mode for local path runs
        #[arg(long, value_enum, default_value_t = RunAgentMode::Auto)]
        agent: RunAgentMode,

        /// Keep failed GitHub checkout artifacts and generated manifests for debugging
        #[arg(long, hide = true, default_value_t = false)]
        keep_failed_artifacts: bool,

        /// Auto-fix generated GitHub draft TOML before build/run
        #[arg(
            long = "auto-fix:toml",
            default_value_t = false,
            conflicts_with_all = ["auto_fix_src", "auto_fix_all"]
        )]
        auto_fix_toml: bool,

        /// Auto-fix fetched GitHub source before build/run
        #[arg(
            long = "auto-fix:src",
            default_value_t = false,
            conflicts_with_all = ["auto_fix_toml", "auto_fix_all"]
        )]
        auto_fix_src: bool,

        /// Enable all GitHub auto-fixes before build/run
        #[arg(
            long = "auto-fix:all",
            default_value_t = false,
            conflicts_with_all = ["auto_fix_toml", "auto_fix_src"]
        )]
        auto_fix_all: bool,

        /// Allow installing/running unverified signatures in non-production environments
        #[arg(long, default_value_t = false)]
        allow_unverified: bool,

        /// Grant read-only access to a host file or directory in sandbox mode
        #[arg(long = "read", value_name = "PATH")]
        read: Vec<String>,

        /// Grant create/update access to a host file or directory in sandbox mode
        #[arg(long = "write", value_name = "PATH")]
        write: Vec<String>,

        /// Grant read-write access to a host file or directory in sandbox mode
        #[arg(long = "read-write", value_name = "PATH")]
        read_write: Vec<String>,

        /// Override the caller working directory used for relative argv and grant resolution
        #[arg(long = "cwd", value_name = "PATH")]
        cwd: Option<PathBuf>,

        /// Arguments passed through to an exported CLI tool after `--`
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Install a package from the store"
    )]
    Install {
        /// Capsule scoped ID (publisher/slug)
        #[arg(required_unless_present = "from_gh_repo")]
        slug: Option<String>,

        /// Build and install directly from a public GitHub repository
        #[arg(
            long = "from-gh-repo",
            value_name = "REPOSITORY",
            conflicts_with = "slug"
        )]
        from_gh_repo: Option<String>,

        /// Registry URL (default: api.ato.run)
        #[arg(long)]
        registry: Option<String>,

        /// Specific version to install
        #[arg(long)]
        version: Option<String>,

        /// Set as default handler for supported content types
        #[arg(long, default_value_t = false)]
        default: bool,

        /// Skip prompts and approve local finalize / projection
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Deprecated legacy flag (always rejected)
        #[arg(long = "skip-verify", hide = true, default_value_t = false)]
        skip_verify_legacy: bool,

        /// Allow installing unverified signatures in non-production environments
        #[arg(long, default_value_t = false)]
        allow_unverified: bool,

        /// Output directory (default: ~/.ato/store/)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Create a launcher projection after install
        #[arg(long, default_value_t = false, conflicts_with = "no_project")]
        project: bool,

        /// Do not prompt for or create a launcher projection
        #[arg(long, default_value_t = false, conflicts_with = "project")]
        no_project: bool,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,

        /// Keep failed GitHub checkout artifacts and generated manifests for debugging
        #[arg(long, hide = true, default_value_t = false)]
        keep_failed_artifacts: bool,

        /// Auto-fix generated GitHub draft TOML before build/install
        #[arg(
            long = "auto-fix:toml",
            default_value_t = false,
            requires = "from_gh_repo",
            conflicts_with_all = ["auto_fix_src", "auto_fix_all"]
        )]
        auto_fix_toml: bool,

        /// Auto-fix fetched GitHub source before build/install
        #[arg(
            long = "auto-fix:src",
            default_value_t = false,
            requires = "from_gh_repo",
            conflicts_with_all = ["auto_fix_toml", "auto_fix_all"]
        )]
        auto_fix_src: bool,

        /// Enable all GitHub auto-fixes before build/install
        #[arg(
            long = "auto-fix:all",
            default_value_t = false,
            requires = "from_gh_repo",
            conflicts_with_all = ["auto_fix_toml", "auto_fix_src"]
        )]
        auto_fix_all: bool,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Materialize a durable ato.lock.json baseline for a local workspace"
    )]
    Init {
        /// Local workspace path to initialize
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Skip prompts when source inference requires explicit confirmation
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Build project into a capsule archive",
        alias = "pack"
    )]
    Build {
        #[arg(default_value = ".")]
        dir: PathBuf,
        /// Initialize capsule.toml interactively
        #[arg(long)]
        init: bool,
        /// Path to signing key
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = EnforcementMode::Strict)]
        enforcement: EnforcementMode,
        #[arg(long)]
        standalone: bool,
        #[arg(long, default_value_t = false)]
        force_large_payload: bool,
        #[arg(long, default_value_t = false)]
        paid_large_payload: bool,
        #[arg(long, default_value_t = false)]
        keep_failed_artifacts: bool,
        #[arg(long, default_value_t = false)]
        timings: bool,
        #[arg(long, default_value_t = false)]
        strict_v3: bool,
    },

    #[command(
        next_help_heading = "Troubleshooting",
        about = "Validate capsule build/run inputs without executing"
    )]
    Validate {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },

    #[command(
        next_help_heading = "Troubleshooting",
        about = "Update ato CLI to the latest version"
    )]
    Update,

    #[command(
        next_help_heading = "Troubleshooting",
        about = "Inspect lock-first metadata, preview write-back, diagnostics, remediation, and runtime requirements"
    )]
    Inspect {
        #[command(subcommand)]
        command: InspectCommands,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Search the store for packages"
    )]
    Search {
        query: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long = "tag", value_delimiter = ',')]
        tags: Vec<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
        /// Registry URL (default: https://api.ato.run)
        #[arg(long)]
        registry: Option<String>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = false)]
        no_tui: bool,
        #[arg(long, default_value_t = false)]
        show_manifest: bool,
    },

    #[command(hide = true)]
    Fetch {
        /// Capsule reference such as publisher/slug or localhost:8080/slug:version
        capsule_ref: String,
        /// Registry URL override
        #[arg(long)]
        registry: Option<String>,
        /// Version override when <CAPSULE_REF> omits :version
        #[arg(long)]
        version: Option<String>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(hide = true)]
    Finalize {
        /// Path to fetched artifact directory
        fetched_artifact_dir: PathBuf,
        #[arg(long, default_value_t = false)]
        allow_external_finalize: bool,
        /// Output directory for the finalized app
        #[arg(long)]
        output_dir: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(hide = true)]
    Project {
        /// Path to the finalized app produced by ato finalize
        derived_app_path: Option<PathBuf>,
        #[arg(long)]
        launcher_dir: Option<PathBuf>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        command: Option<ProjectCommands>,
    },

    #[command(hide = true)]
    Unproject {
        /// Projection ID or projected path
        projection_ref: String,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(next_help_heading = "Management", about = "List running capsules")]
    Ps {
        #[arg(long, default_value_t = false)]
        all: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(
        next_help_heading = "Management",
        about = "Stop a running capsule",
        alias = "close"
    )]
    Stop {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = false)]
        all: bool,
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    #[command(
        next_help_heading = "Management",
        about = "Show logs of a running capsule"
    )]
    Logs {
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value_t = false)]
        follow: bool,
        #[arg(long)]
        tail: Option<usize>,
    },

    #[command(
        next_help_heading = "Management",
        about = "Inspect or adapt app-scoped bootstrap state"
    )]
    App {
        #[command(subcommand)]
        command: AppCommands,
    },

    #[command(
        next_help_heading = "Management",
        about = "Inspect or register persistent state bindings"
    )]
    State {
        #[command(subcommand)]
        command: StateCommands,
    },

    #[command(
        next_help_heading = "Management",
        about = "Inspect or register host-side service bindings"
    )]
    Binding {
        #[command(subcommand)]
        command: BindingCommands,
    },

    #[command(next_help_heading = "Auth", about = "Login to Ato registry")]
    Login {
        #[arg(long)]
        token: Option<String>,
        #[arg(long, default_value_t = false)]
        headless: bool,
    },

    #[command(next_help_heading = "Auth", about = "Logout")]
    Logout,

    #[command(
        next_help_heading = "Auth",
        about = "Show current authentication status",
        alias = "auth"
    )]
    Whoami,

    #[command(hide = true)]
    Key {
        #[command(subcommand)]
        command: KeyCommands,
    },

    #[command(hide = true)]
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Publish capsule artifacts through the unified pipeline (My Dock direct upload by default, official registry is CI-first)"
    )]
    Publish {
        #[arg(long)]
        registry: Option<String>,
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with = "ci",
            help = "Start at Verify using an existing .capsule artifact"
        )]
        artifact: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PUBLISHER/SLUG",
            conflicts_with = "ci",
            requires = "artifact",
            help = "Override publisher/slug for artifact uploads"
        )]
        scoped_id: Option<String>,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with_all = ["ci", "dry_run"],
            help = "Allow idempotent success when the final Publish phase sees the same artifact/version already present"
        )]
        allow_existing: bool,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with_all = ["ci", "dry_run"],
            help = "Select Prepare as the stop point"
        )]
        prepare: bool,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with_all = ["ci", "dry_run"],
            help = "Select Verify as the stop point (source input builds then verifies; artifact input verifies only)"
        )]
        build: bool,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with_all = ["ci", "dry_run"],
            help = "Select Publish as the stop point"
        )]
        deploy: bool,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with_all = ["ci", "dry_run"],
            help = "Temporary official-registry compatibility mode that restores the legacy full pipeline"
        )]
        legacy_full_publish: bool,
        #[arg(long, default_value_t = false)]
        force_large_payload: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Raise the large-payload threshold from 200 MB to 1 GB for paid-plan uploads"
        )]
        paid_large_payload: bool,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with = "artifact",
            help = "Finalize a desktop source build locally, then publish the signed artifact"
        )]
        finalize_local: bool,
        #[arg(
            long,
            default_value_t = false,
            help = "Allow host-local finalize steps that invoke external signing/finalize tools"
        )]
        allow_external_finalize: bool,
        #[arg(
            long,
            default_value_t = false,
            conflicts_with_all = ["ci", "dry_run"],
            help = "Apply the official workflow fix, then rerun Publish diagnostics"
        )]
        fix: bool,
        /// Run the official CI publish mode directly
        #[arg(long, conflicts_with = "dry_run")]
        ci: bool,
        /// Run top-level dry-run mode (registry and permission simulation, no upload)
        #[arg(long, conflicts_with = "ci")]
        dry_run: bool,
        /// Disable interactive handoff UI for official publish guidance
        #[arg(long, conflicts_with_all = ["ci", "dry_run", "json"])]
        no_tui: bool,
        #[arg(long)]
        json: bool,
    },

    #[command(hide = true)]
    GenCi,

    #[command(hide = true)]
    Engine {
        #[command(subcommand)]
        command: EngineCommands,
    },

    #[command(hide = true)]
    Registry {
        #[command(subcommand)]
        command: RegistryCommands,
    },

    #[command(hide = true)]
    Setup {
        /// Engine name to install
        #[arg(long, default_value = "nacelle")]
        engine: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = false)]
        skip_verify: bool,
    },

    #[command(hide = true)]
    New {
        name: String,
        #[arg(long, default_value = "python")]
        template: String,
    },

    #[command(hide = true)]
    Keygen {
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    Scaffold {
        #[command(subcommand)]
        command: ScaffoldCommands,
    },

    #[command(hide = true)]
    Sign {
        target: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },

    #[command(hide = true)]
    Verify {
        target: PathBuf,
        #[arg(long)]
        sig: Option<PathBuf>,
        #[arg(long)]
        signer: Option<String>,
        #[arg(long)]
        json: bool,
    },

    #[command(hide = true)]
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },

    #[command(hide = true)]
    Package {
        #[command(subcommand)]
        command: PackageCommands,
    },

    #[command(hide = true)]
    Source {
        #[command(subcommand)]
        command: SourceCommands,
    },

    #[command(hide = true)]
    Guest {
        #[arg()]
        sync_path: PathBuf,
    },

    #[command(hide = true)]
    Ipc {
        #[command(subcommand)]
        command: IpcCommands,
    },
}
