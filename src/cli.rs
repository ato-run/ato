use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

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

fn cli_styles() -> clap::builder::Styles {
    use clap::builder::styling::{AnsiColor, Effects};
    clap::builder::Styles::styled()
        .header(AnsiColor::Cyan.on_default() | Effects::BOLD)
        .usage(AnsiColor::Green.on_default() | Effects::BOLD)
        .literal(AnsiColor::Blue.on_default() | Effects::BOLD)
        .placeholder(AnsiColor::Yellow.on_default())
}

#[derive(Parser)]
#[command(name = "ato")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(styles = cli_styles())]
#[command(help_template = "\
{about-with-newline}
Usage: {usage}

Primary Commands:
  run      Execute a capsule or SKILL.md in a strict Zero-Trust sandbox
  build    Pack a project into an immutable .capsule archive
  publish  Publish capsule artifacts to a registry
  install  Install a verified package from the registry
  search   Search the registry for agent skills and packages
  init     Analyze the current project and print an agent-ready capsule.toml prompt

Management:
  ps       List running capsules
  stop     Stop a running capsule
  logs     Show logs of a running capsule
    state    Inspect or register persistent state bindings
    binding  Inspect or register host-side service bindings

Auth:
  login    Login to Ato registry
  logout   Logout
  whoami   Show current authentication status

Advanced Commands:
  inspect  Inspect capsule metadata and runtime requirements
  fetch    Fetch an artifact into local cache for debugging or manual workflows
  finalize Perform local derivation for a fetched native artifact
  project  Add a finalized app to launcher surfaces
  unproject Remove a launcher projection
  key      Manage signing keys
  config   Manage configuration (registry, engine, source)
  gen-ci   Generate GitHub Actions workflow for OIDC CI publish
  registry Manage registry commands (resolve/list/cache/serve)

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
        alias = "open"
    )]
    Run {
        /// Local path (./, ../, ~/, /...), store scoped ID (publisher/slug), or GitHub repo (github.com/owner/repo). Default: current directory
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Resolve SKILL.md by skill name from standard locations and run it safely
        #[arg(long = "skill", conflicts_with = "from_skill")]
        skill: Option<String>,

        /// Run from SKILL.md by translating frontmatter into a fail-closed capsule execution plan
        #[arg(long = "from-skill", conflicts_with = "skill")]
        from_skill: Option<PathBuf>,

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

        /// Keep failed GitHub checkout artifacts and generated manifests for debugging
        #[arg(long, hide = true, default_value_t = false)]
        keep_failed_artifacts: bool,

        /// Allow installing/running unverified signatures in non-production environments
        #[arg(long, default_value_t = false)]
        allow_unverified: bool,
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
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Analyze the current project and print an agent-ready capsule.toml prompt"
    )]
    Init,

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
        keep_failed_artifacts: bool,
        #[arg(long, default_value_t = false)]
        timings: bool,
        #[arg(long, default_value_t = false)]
        strict_v3: bool,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Validate capsule build/run inputs without executing"
    )]
    Validate {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Update ato CLI to the latest version"
    )]
    Update,

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Inspect capsule metadata and runtime requirements"
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

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Fetch an artifact into local cache for debugging or manual workflows"
    )]
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

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Perform local derivation for a fetched native artifact. Most users should use `ato install`."
    )]
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

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Add a finalized app to launcher surfaces (experimental). Typically used after `ato finalize`."
    )]
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

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Remove an experimental launcher projection without mutating the finalized artifact"
    )]
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

    #[command(next_help_heading = "Advanced Commands", about = "Manage signing keys")]
    Key {
        #[command(subcommand)]
        command: KeyCommands,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Manage configuration (registry, engine)"
    )]
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Publish capsule (default: My Dock direct upload, official registry: CI-first)"
    )]
    Publish {
        #[arg(long)]
        registry: Option<String>,
        #[arg(long, value_name = "PATH", conflicts_with_all = ["ci", "dry_run"])]
        artifact: Option<PathBuf>,
        #[arg(
            long,
            value_name = "PUBLISHER/SLUG",
            conflicts_with_all = ["ci", "dry_run"],
            requires = "artifact"
        )]
        scoped_id: Option<String>,
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        allow_existing: bool,
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        prepare: bool,
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        build: bool,
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        deploy: bool,
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        legacy_full_publish: bool,
        #[arg(long, default_value_t = false)]
        force_large_payload: bool,
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        fix: bool,
        #[arg(long, conflicts_with = "dry_run")]
        ci: bool,
        #[arg(long, conflicts_with = "ci")]
        dry_run: bool,
        #[arg(long, conflicts_with_all = ["ci", "dry_run", "json"])]
        no_tui: bool,
        #[arg(long)]
        json: bool,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Generate fixed GitHub Actions workflow for OIDC CI publish"
    )]
    GenCi,

    #[command(hide = true)]
    Engine {
        #[command(subcommand)]
        command: EngineCommands,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Manage registry commands (resolve/list/cache/serve)"
    )]
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

#[derive(Subcommand)]
pub(crate) enum InspectCommands {
    #[command(about = "Inspect runtime requirements from capsule.toml")]
    Requirements {
        /// Local capsule path or scoped package reference such as publisher/slug
        target: String,
        /// Registry URL override
        #[arg(long)]
        registry: Option<String>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProjectCommands {
    #[command(
        about = "List experimental projection state and detect broken projections read-only"
    )]
    Ls {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum KeyCommands {
    Gen {
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    Sign {
        target: PathBuf,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    Verify {
        target: PathBuf,
        #[arg(long)]
        sig: Option<PathBuf>,
        #[arg(long)]
        signer: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigCommands {
    Engine {
        #[command(subcommand)]
        command: ConfigEngineCommands,
    },
    Registry {
        #[command(subcommand)]
        command: ConfigRegistryCommands,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigEngineCommands {
    Features,
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        default: bool,
    },
    #[command(about = "Download and install an engine")]
    Install {
        /// Engine name to install
        #[arg(long, default_value = "nacelle")]
        engine: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = false)]
        skip_verify: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConfigRegistryCommands {
    Resolve {
        domain: String,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    ClearCache,
}

#[derive(Subcommand)]
pub(crate) enum IpcCommands {
    #[command(about = "Show status of running IPC services")]
    Status {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Start an IPC service")]
    Start {
        /// Capsule path or directory
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Stop a running IPC service")]
    Stop {
        #[arg(long)]
        name: String,
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Validate and send a JSON-RPC invoke request")]
    Invoke {
        /// Capsule path or directory
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        service: Option<String>,
        #[arg(long)]
        method: String,
        #[arg(long)]
        args: String,
        #[arg(long, default_value = "invoke-1")]
        id: String,
        #[arg(long)]
        max_message_size: Option<usize>,
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ScaffoldCommands {
    Docker {
        #[arg(long, default_value = "capsule.toml")]
        manifest: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        output_dir: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProfileCommands {
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        bio: Option<String>,
        #[arg(long)]
        avatar: Option<PathBuf>,
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        website: Option<String>,
        #[arg(long)]
        github: Option<String>,
        #[arg(long)]
        twitter: Option<String>,
    },
    Show {
        #[arg()]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum EngineCommands {
    Features,
    Register {
        #[arg(long)]
        name: String,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        default: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum RegistryCommands {
    Resolve {
        domain: String,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    ClearCache,
    Serve {
        #[arg(long, default_value_t = 8787)]
        port: u16,
        #[arg(long, default_value = "~/.ato/local-registry")]
        data_dir: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long)]
        auth_token: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum StateCommands {
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        owner_scope: Option<String>,
        #[arg(long)]
        state_name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        state_ref: String,
        #[arg(long)]
        json: bool,
    },
    Register {
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long = "name")]
        state_name: String,
        #[arg(long = "path", value_name = "/ABS/PATH")]
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum BindingCommands {
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        owner_scope: Option<String>,
        #[arg(long)]
        service_name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Inspect {
        binding_ref: String,
        #[arg(long)]
        json: bool,
    },
    Resolve {
        #[arg(long)]
        owner_scope: String,
        #[arg(long)]
        service_name: String,
        #[arg(long, default_value = "ingress")]
        binding_kind: String,
        #[arg(long)]
        caller_service: Option<String>,
        #[arg(long)]
        json: bool,
    },
    BootstrapTls {
        #[arg(long = "binding")]
        binding_ref: String,
        #[arg(long, default_value_t = false)]
        install_system_trust: bool,
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    ServeIngress {
        #[arg(long = "binding")]
        binding_ref: String,
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long)]
        upstream_url: Option<String>,
    },
    RegisterIngress {
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long)]
        service_name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        json: bool,
    },
    RegisterService {
        #[arg(long, default_value = ".")]
        manifest: PathBuf,
        #[arg(long)]
        service_name: String,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        process_id: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        json: bool,
    },
    SyncProcess {
        #[arg(long)]
        process_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SourceCommands {
    SyncStatus {
        #[arg(long = "source-id")]
        source_id: String,
        #[arg(long = "sync-run-id")]
        sync_run_id: String,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Rebuild {
        #[arg(long = "source-id")]
        source_id: String,
        #[arg(long = "ref", alias = "reference")]
        reference: Option<String>,
        #[arg(long, default_value_t = false)]
        wait: bool,
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum PackageCommands {
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
        #[arg(long)]
        registry: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = false)]
        no_tui: bool,
        #[arg(long, default_value_t = false)]
        show_manifest: bool,
    },
}
