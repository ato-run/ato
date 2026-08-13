use std::path::PathBuf;

use clap::{Parser, Subcommand};

use super::app::AppCommands;
use super::binding::BindingCommands;
use super::config::{ConfigCommands, EngineCommands};
use super::console::ConsoleCommands;
use super::decap::DecapCommands;
use super::import_cmd::ImportArgs;
use super::inspect::InspectCommands;
use super::ipc::IpcCommands;
use super::key::KeyCommands;
use super::package::PackageCommands;
use super::profile::ProfileCommands;
use super::project::{ProjectCommands, ScaffoldCommands};
use super::receipts::ReceiptsCommands;
use super::registry::RegistryCommands;
use super::shared::{
    CacheStrategyArg, CompatibilityFallbackBackend, EnforcementMode, ProviderToolchain,
    RunAgentMode, cli_styles,
};
use super::source::SourceCommands;
use super::state::StateCommands;
use super::workspace::WorkspaceCommands;

#[derive(Parser)]
#[command(name = "ato")]
// Pin the usage-line spelling: clap otherwise renders argv[0], which is
// `ato.exe` on Windows and would diverge from every doc and error hint.
#[command(bin_name = "ato")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(styles = cli_styles())]
#[command(help_template = "\
Usage: {usage}

Primary Commands:
  run                 Run a local project, source repo, or published recipe
  decap               Open and manage portable Capsule Sessions
  workspace share     Share a local workspace
  workspace setup     Set up a shared workspace locally

Management:
  ps       List running capsules
  stop     Stop a running capsule
  logs     Show logs of a running capsule

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

// The CLI command surface is intentionally wide; some subcommands carry large
// inline arg structs. Boxing variants would fight the clap derive for no user
// benefit, so accept the size spread.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub(crate) enum Commands {
    #[command(
        next_help_heading = "Primary Commands",
        about = "Open and manage portable Capsule Sessions"
    )]
    Decap {
        #[command(subcommand)]
        command: DecapCommands,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Run a local project, source repo, or published recipe",
        trailing_var_arg = true
    )]
    Run {
        /// Local path (./, ../, ~/, /...), share URL (https://ato.run/s/...), store scoped ID (publisher/slug), or GitHub repo (github.com/owner/repo). Default: current directory
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Target label to execute (e.g. static, cli, widget)
        #[arg(short = 't', long = "target")]
        target: Option<String>,

        /// Workspace entry to execute when the target exposes multiple runnable entries
        #[arg(long = "entry")]
        entry: Option<String>,

        /// Load environment variables for this run from a dotenv-style file
        #[arg(long = "env-file", value_name = "PATH")]
        env_file: Option<PathBuf>,

        /// Prompt for missing required environment variables before running
        #[arg(long = "prompt-env", default_value_t = false)]
        prompt_env: bool,

        /// Watch files and restart/re-run when supported (experimental)
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

        /// Auto-bind every unbound persistent `[state.<name>]` (attach="explicit")
        /// under this root, for non-interactive server/runner contexts. Each
        /// state resolves to a stable, path-safe dir `<DIR>/<target>/<state_key>`
        /// (reused across runs). `<DIR>` MUST already be scoped by the
        /// server-confirmed owner/account AND a stable, immutable capsule identity
        /// (e.g. `<base>/<owner_id>/<capsule_revision>`); this flag only appends
        /// target + state_key and never derives owner or capsule identity.
        /// `--state` bindings always win.
        #[arg(long = "managed-state-root", value_name = "DIR")]
        managed_state_root: Option<PathBuf>,

        /// Inject external data binding using KEY=VALUE for targets that declare [external_injection]
        #[arg(long = "inject", value_name = "KEY=VALUE")]
        inject: Vec<String>,

        /// Network enforcement mode
        #[arg(long, value_enum, default_value_t = EnforcementMode::Strict)]
        enforcement: EnforcementMode,

        /// Explicitly allow Tier2 (python/native) execution via native OS sandbox.
        /// Required for capsules with runtime = "source/native" or "source/python".
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

        /// Select the provider-backed materialization toolchain
        #[arg(long = "via", value_enum, default_value_t = ProviderToolchain::Auto)]
        via: ProviderToolchain,

        /// Use an existing capsule.toml from a local file path or community URL
        /// instead of auto-generating one. Accepts a local path or an https:// URL.
        /// Skips community candidate discovery when provided.
        #[arg(short = 'T', long = "use-existing-toml", value_name = "PATH_OR_URL")]
        use_existing_toml: Option<String>,

        /// Pin a GitHub run to an explicit commit SHA and skip mutable-ref resolution
        #[arg(long = "commit", value_name = "SHA")]
        commit: Option<String>,

        /// Dependency cache strategy: auto (default, honors ATO_CACHE_STRATEGY), none, derivation
        #[arg(long = "cache", value_enum, default_value_t = CacheStrategyArg::Auto)]
        cache: CacheStrategyArg,

        /// Skip prompt and auto-install when app-id is not installed.
        /// Required in CI and any non-TTY context (use `-y` when piping
        /// output or running without an interactive terminal).
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Show phase and execution context details on stderr
        #[arg(short = 'v', long = "verbose", default_value_t = false)]
        verbose: bool,

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

        /// Force the build phase to run even if a previous materialization is reusable.
        /// See: docs/rfcs/draft/BUILD_MATERIALIZATION.md
        #[arg(long = "rebuild", default_value_t = false, conflicts_with = "no_build")]
        rebuild: bool,

        /// Forbid the build phase from running. Fails if no usable materialization exists.
        /// See: docs/rfcs/draft/BUILD_MATERIALIZATION.md
        #[arg(long = "no-build", default_value_t = false, conflicts_with = "rebuild")]
        no_build: bool,

        /// Print the aggregate requirements (secrets, permissions, ports) for this capsule
        /// without launching it. Exits 0. Only supports local paths and already-cached
        /// `github.com/<owner>/<repo>` refs; never installs or fetches.
        #[arg(long = "plan-only", default_value_t = false)]
        plan_only: bool,

        /// Fail-closed realization profile (#500): block the launch before
        /// execution if any required launch input cannot be verified, instead
        /// of launching with a conservative warning. Opt-in; the default
        /// profile is unchanged.
        #[arg(long = "strict-realization", default_value_t = false)]
        strict_realization: bool,

        /// Import from docker-compose.yml and run as an Ato OCI service graph
        /// through PodmanProvider (experimental, requires Podman)
        #[arg(long = "oci-compose", default_value_t = false, hide = true)]
        oci_compose: bool,

        /// Import docker run commands from install.sh and run as an Ato OCI
        /// service graph through PodmanProvider (experimental, requires Podman)
        #[arg(long = "oci-install-sh", default_value_t = false, hide = true)]
        oci_install_sh: bool,

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
        hide = true,
        about = "Resolve a capsule handle or terse ref into a launch preview"
    )]
    Resolve {
        /// Canonical capsule handle, GitHub shorthand, registry scoped ID, or local path
        handle: String,

        /// Target label to resolve
        #[arg(short = 't', long = "target")]
        target: Option<String>,

        /// Registry URL override for registry-backed handles
        #[arg(long)]
        registry: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(about = "Explain dependency hash inputs for a capsule")]
    ExplainHash {
        /// Capsule id or run target string to explain
        #[arg(long = "capsule")]
        capsule: String,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Import a GitHub repository as a source recipe session"
    )]
    Import(ImportArgs),

    #[command(about = "Inspect or prune the local A1 dependency cache")]
    Cache {
        #[command(subcommand)]
        command: super::cache::CacheCommands,
    },

    #[command(about = "Manage A2 attestation keys, trust roots, and verification")]
    Attest {
        #[command(subcommand)]
        command: super::attest::AttestCommands,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Share or set up a workspace"
    )]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommands,
    },

    #[command(hide = true, about = "Register a durable local app from the store")]
    Install {
        /// Capsule scoped ID (publisher/slug)
        #[arg(required_unless_present_any = ["from_gh_repo", "from_local"])]
        slug: Option<String>,

        /// Build and install directly from a public GitHub repository
        #[arg(
            long = "from-gh-repo",
            value_name = "REPOSITORY",
            conflicts_with = "slug"
        )]
        from_gh_repo: Option<String>,

        /// Build and install directly from a local capsule directory (hermetic;
        /// no network/registry/GitHub). Expects `<DIR>/capsule.toml`. Intended
        /// for deterministic Desktop/AODD relaunch smoke tests.
        #[arg(
            long = "from-local",
            value_name = "DIR",
            conflicts_with_all = ["slug", "from_gh_repo"]
        )]
        from_local: Option<PathBuf>,

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
        hide = true,
        about = "Launch an installed app by its install profile key or capsule:// URL"
    )]
    Launch {
        /// Install profile key (`ipk_<32hex>`) from `ato install`, or a
        /// `capsule://<location>?<query>` URL to relaunch an installed app with
        /// launch-condition query inputs.
        install_profile_key: String,

        /// Skip interactive prompts and assume yes
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Print verbose output including resolved lifecycle IDs
        #[arg(short = 'v', long = "verbose", default_value_t = false)]
        verbose: bool,

        /// Emit machine-readable JSON with resolved lifecycle IDs before launch
        #[arg(long, default_value_t = false)]
        json: bool,

        /// Override the nacelle runtime binary path
        #[arg(long, hide = true)]
        nacelle: Option<PathBuf>,

        /// Internal (ato-desktop): start the installed app as a detached session
        /// that writes a discoverable session record, instead of running
        /// foreground. Not for interactive use. See #565.
        #[arg(long = "detached-session", hide = true, default_value_t = false)]
        detached_session: bool,
    },

    #[command(about = "List install revisions for an installed app profile")]
    Revisions {
        /// Install profile key (`ipk_<32hex>`) from `ato install` or `ato launch` output
        install_profile_key: String,

        /// Emit machine-readable JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(about = "Rollback an installed app profile to a previous revision")]
    Rollback {
        /// Install profile key (`ipk_<32hex>`) from `ato install` or `ato launch` output
        install_profile_key: String,

        /// Specific revision ID to rollback to. If omitted, rolls back to the previous revision.
        revision_id: Option<String>,
    },

    #[command(
        name = "update",
        about = "Update an installed app to its latest release"
    )]
    AppUpdate {
        /// Install profile key (`ipk_<32hex>`) from `ato install` or `ato launch` output
        install_profile_key: String,

        /// Skip interactive prompts and assume yes
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Emit machine-readable JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(about = "Collect unused install revisions")]
    Gc {
        /// Report what would be deleted without removing anything
        #[arg(long, default_value_t = false)]
        dry_run: bool,

        /// Keep at least this many recent revisions per profile (default: 2)
        #[arg(long, default_value_t = 2)]
        keep_last: usize,

        /// Keep revisions finalized within this many days (default: 14)
        #[arg(long, default_value_t = 14)]
        retention_days: u64,

        /// Emit machine-readable JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(
        hide = true,
        about = "Fetch declared development dependencies for a local project"
    )]
    Setup {
        /// Local workspace path to prepare
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Registry URL override for Ato capsule dependencies
        #[arg(long)]
        registry: Option<String>,

        /// Skip prompts when Ato dependency install requires confirmation
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,

        /// Print the detected setup plan without executing it
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },

    #[command(
        hide = true,
        about = "Materialize a durable capsule.lock baseline for a local workspace"
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
        hide = true,
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
        hide = true,
        about = "Validate capsule build/run inputs without executing"
    )]
    Validate {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Generate .ato/derived/capsule.lock.json from capsule.toml"
    )]
    Lock {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long, default_value_t = false)]
        timings: bool,
        #[arg(long)]
        json: bool,
    },

    #[command(
        hide = true,
        name = "self-update",
        about = "Update ato CLI to the latest version"
    )]
    SelfUpdate,

    #[command(
        hide = true,
        about = "Uninstall ato (install.sh deployments only — Homebrew users should run `brew uninstall ato-cli`)"
    )]
    Uninstall {
        /// Remove regeneratable local Ato data under ~/.ato
        #[arg(long = "purge", default_value_t = false)]
        purge: bool,

        /// Also remove ~/.ato/config.toml (requires --purge)
        #[arg(long = "include-config", requires = "purge", default_value_t = false)]
        include_config: bool,

        /// Also remove ~/.ato/keys/ (requires --purge)
        #[arg(long = "include-keys", requires = "purge", default_value_t = false)]
        include_keys: bool,

        /// Show what would be removed without deleting
        #[arg(long = "dry-run", default_value_t = false)]
        dry_run: bool,

        /// Skip the confirmation prompt
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
    },

    #[command(
        hide = true,
        about = "Inspect lock-first metadata, preview write-back, diagnostics, remediation, and runtime requirements"
    )]
    Inspect {
        #[command(subcommand)]
        command: InspectCommands,
    },

    #[command(about = "Replay a stored execution receipt on this host")]
    Replay {
        /// Execution ID, for example blake3:...
        id: String,
        /// Fail closed unless the receipt is classified as pure
        #[arg(long, conflicts_with = "best_effort")]
        strict: bool,
        /// Re-run from the local source reference and report that the envelope is best-effort
        #[arg(long = "best-effort", conflicts_with = "strict")]
        best_effort: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Diagnose whether a v2 execution receipt has the portability inputs needed for cross-host reconstruction",
        long_about = "Phase 1 skeleton: inspects the receipt's source provenance, dependency identity, runtime resolved_ref, and policy hashes to determine whether enough information is present to reconstruct the launch envelope on a different host. Does NOT fetch source / deps / runtimes today — that is Phase 2 follow-up work tracked under `ato reconstruct --execute` (rejected for now)."
    )]
    Reconstruct {
        /// Execution ID, for example blake3:...
        id: String,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// (Reserved) actually attempt cross-host reconstruction. Not yet implemented; the
        /// command will return a `not-implemented` error when this flag is passed.
        #[arg(long, hide = true)]
        execute: bool,
    },

    #[command(about = "Inspect and compare stored execution receipts")]
    Receipts {
        #[command(subcommand)]
        command: ReceiptsCommands,
    },

    #[command(hide = true, about = "Search the store for packages")]
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
        #[arg(value_name = "ID")]
        target: Option<String>,
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

    #[command(hide = true, about = "Inspect or adapt app-scoped bootstrap state")]
    App {
        #[command(subcommand)]
        command: AppCommands,
    },

    #[command(
        hide = true,
        about = "Plumbing surface for trusted shells (e.g. ato-desktop)"
    )]
    Internal {
        #[command(subcommand)]
        command: crate::cli::InternalCommands,
    },

    #[command(hide = true, about = "Inspect or register persistent state bindings")]
    State {
        #[command(subcommand)]
        command: StateCommands,
    },

    #[command(hide = true, about = "Inspect or register host-side service bindings")]
    Binding {
        #[command(subcommand)]
        command: BindingCommands,
    },

    #[command(hide = true, about = "Manage secrets (age-encrypted files)")]
    Secrets {
        #[command(subcommand)]
        command: crate::cli::SecretsCommands,
    },

    #[command(
        hide = true,
        about = "Manage the age identity session (unlock once, reuse across commands)"
    )]
    Session {
        #[command(subcommand)]
        command: crate::cli::IdentitySessionCommands,
    },

    #[command(about = "Connected Runner agent: enroll, serve, provision GPU host, or run doctor")]
    Runner {
        #[command(subcommand)]
        command: crate::cli::RunnerCommands,
    },

    #[command(about = "Diagnose this host's readiness for a runtime/feature")]
    Doctor {
        #[command(subcommand)]
        target: DoctorTarget,
    },

    #[command(hide = true, about = "Login to Ato registry")]
    Login {
        #[arg(long)]
        token: Option<String>,
        #[arg(long, default_value_t = false)]
        headless: bool,
        /// Non-interactive login with NDJSON progress on stdout (used when
        /// ato-desktop spawns this as a child process with no TTY). Opens
        /// the OS default browser exactly like plain `ato login`.
        #[arg(long = "desktop", hide = true, default_value_t = false)]
        desktop: bool,
    },

    #[command(hide = true, about = "Logout")]
    Logout,

    #[command(hide = true, about = "Emit a desktop auth handoff for ato-desktop")]
    DesktopAuthHandoff,

    #[command(
        hide = true,
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
        hide = true,
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
        /// Publish a capsule.toml as a community capsule record
        #[arg(
            long,
            value_name = "PATH",
            conflicts_with_all = [
                "registry",
                "artifact",
                "scoped_id",
                "ci",
                "prepare",
                "build",
                "deploy",
                "finalize_local",
                "legacy_full_publish",
                "force_large_payload",
                "paid_large_payload",
                "allow_existing",
                "allow_external_finalize",
                "fix",
                "no_tui"
            ]
        )]
        toml: Option<PathBuf>,
        /// Source locator for the capsule.toml (e.g. github.com/owner/repo)
        #[arg(long, value_name = "SOURCE", requires = "toml")]
        source: Option<String>,
        /// Skip interactive confirmation when using --toml
        #[arg(short = 'y', long = "yes", requires = "toml", default_value_t = false)]
        yes: bool,
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

    #[command(about = "Interact with the Ato community capsule.toml registry")]
    Community {
        #[command(subcommand)]
        command: super::community::CommunityCommands,
    },

    #[command(about = "Open the Ato Web Console connected to the local Runtime")]
    Console {
        #[command(subcommand)]
        command: ConsoleCommands,
    },
}

/// Targets for `ato doctor <target>` — host-readiness diagnostics per
/// runtime/feature. (For GPU *host provisioning* readiness, see
/// `ato runner doctor`.)
#[derive(Subcommand)]
pub(crate) enum DoctorTarget {
    /// Check this host's readiness to run local-LLM (native-inference) capsules:
    /// platform/engine support, model-cache writability, and acceleration
    /// (Metal on macOS, Vulkan on Linux NVIDIA).
    #[command(name = "native-inference")]
    NativeInference {
        /// Emit machine-readable JSON on stdout instead of a human table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Report the on-disk footprint of the local `~/.ato` caches that grow
    /// silently — desktop/session/engine logs, the SQLite state DBs, and the
    /// CAS content store — sorted largest-first, and warn before they bloat.
    /// Read-only: it only measures, never deletes.
    #[command(name = "disk")]
    Disk {
        /// Emit machine-readable JSON on stdout instead of a human table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Report this host's readiness to act as a local Desktop Runner: the
    /// isolation substrate (macOS Apple Containerization / `container`), what it
    /// can run today (cold OCI), and what is not yet supported (Ready-State
    /// restore, CRIU, bindings). Read-only: it only probes, never starts the
    /// `container` service or launches a workload.
    #[command(name = "desktop-runner")]
    DesktopRunner {
        /// Emit machine-readable JSON on stdout instead of a human summary.
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Check this Ubuntu host's readiness as an all-in-one Ato snapshot builder +
    /// capsule runner: KVM, Firecracker, guest kernel, Docker, groups, tun/tap,
    /// artifact root, env file, runner token and the systemd services — plus the
    /// derived Ready-State verdict (can it build_ready_state / restore_snapshot
    /// today?). Diagnostics only: it installs and reconfigures nothing (the sole
    /// write is a transient probe file in the artifact root to test writability,
    /// removed immediately). The fixable set is applied by `ato runner setup --fix`.
    #[command(name = "runner")]
    Runner {
        /// Emit machine-readable JSON on stdout instead of a human table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
}
