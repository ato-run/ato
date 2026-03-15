use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use colored::Colorize;
use serde::Serialize;
use serde_json::json;
use std::cmp::Ordering;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use tracing::debug;

use capsule_core::CapsuleReporter;

fn print_animated_logo() {
    let logo = r#"
    ___    __       
   /   |  / /_____  
  / /| | / __/ __ \ 
 / ___ |/ /_/ /_/ / 
/_/  |_|\__/\____/  
"#;

    for line in logo.lines() {
        println!("{}", line.cyan().bold());
        io::stdout().flush().unwrap();
        thread::sleep(Duration::from_millis(30));
    }
    println!();
}

const DEFAULT_RUN_REGISTRY_URL: &str = "https://api.ato.run";

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EnforcementMode {
    Strict,
    BestEffort,
}

impl EnforcementMode {
    fn as_str(self) -> &'static str {
        match self {
            EnforcementMode::Strict => "strict",
            EnforcementMode::BestEffort => "best_effort",
        }
    }
}

struct SidecarCleanup {
    sidecar: Option<common::sidecar::SidecarHandle>,
    reporter: std::sync::Arc<reporters::CliReporter>,
}

impl SidecarCleanup {
    fn new(
        sidecar: Option<common::sidecar::SidecarHandle>,
        reporter: std::sync::Arc<reporters::CliReporter>,
    ) -> Self {
        Self { sidecar, reporter }
    }

    fn stop_now(&mut self) {
        if let Some(sidecar) = self.sidecar.take() {
            if let Err(err) = sidecar.stop() {
                let _ = futures::executor::block_on(
                    self.reporter
                        .warn(format!("⚠️  Failed to stop sidecar: {}", err)),
                );
            }
        }
    }
}

impl Drop for SidecarCleanup {
    fn drop(&mut self) {
        self.stop_now();
    }
}

mod ato_error_jsonl;
mod auth;
mod binding;
mod commands;
mod common;
mod consent_store;
mod data_injection;
mod diagnostics;
mod engine_manager;
mod env;
mod error_codes;
mod executors;
mod external_capsule;
mod gen_ci;
mod guest_protocol;
mod inference_feedback;
mod ingress_proxy;
mod init;
mod install;
mod ipc;
mod keygen;
mod local_input;
mod native_delivery;
mod new;
mod payload_guard;
mod process_manager;
mod profile;
mod publish_artifact;
mod publish_ci;
mod publish_dry_run;
mod publish_official;
mod publish_preflight;
mod publish_prepare;
mod publish_private;
mod registry;
mod registry_delete;
mod registry_http;
mod registry_serve;
mod registry_store;
mod registry_yank;
mod reporters;
mod runtime_manager;
mod runtime_overrides;
mod runtime_tree;
mod scaffold;
mod search;
mod sign;
mod skill;
mod skill_resolver;
mod source;
mod state;
mod tui;
mod verify;

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
struct Cli {
    /// Path to nacelle engine binary (overrides NACELLE_PATH)
    #[arg(long)]
    nacelle: Option<PathBuf>,

    /// Emit machine-readable JSON output
    #[arg(long)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(
        next_help_heading = "Primary Commands",
        about = "Run a capsule app or local project"
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

        /// Skip prompt and auto-install when app-id is not installed
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

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
    },

    #[command(
        next_help_heading = "Primary Commands",
        about = "Analyze the current project and print an agent-ready capsule.toml prompt"
    )]
    Init,

    #[command(
        next_help_heading = "Primary Commands",
        about = "Build project into a capsule archive"
    )]
    Build {
        /// Directory containing capsule.toml (default: ".")
        #[arg(default_value = ".")]
        dir: PathBuf,

        /// Initialize capsule.toml interactively if not found
        #[arg(long)]
        init: bool,

        /// Path to signing key (optional)
        #[arg(long)]
        key: Option<PathBuf>,

        /// Network enforcement mode
        #[arg(long, value_enum, default_value_t = EnforcementMode::Strict)]
        enforcement: EnforcementMode,

        /// Create self-extracting executable installer (includes nacelle runtime)
        #[arg(long)]
        standalone: bool,

        /// Allow building payloads larger than 200MB
        #[arg(long, default_value_t = false)]
        force_large_payload: bool,

        /// Keep failed build artifacts when smoke test fails
        #[arg(long, default_value_t = false)]
        keep_failed_artifacts: bool,

        /// Print per-phase build timings
        #[arg(long, default_value_t = false)]
        timings: bool,

        /// Disallow fallback when source_digest/CAS(v3 path) is unavailable
        #[arg(long, default_value_t = false)]
        strict_v3: bool,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Validate capsule build/run inputs without executing"
    )]
    Validate {
        /// Directory containing capsule.toml or the manifest file itself (default: ".")
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Emit machine-readable JSON output
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
        /// Search query (e.g., "note", "ai chat")
        query: Option<String>,

        /// Filter by category
        #[arg(long)]
        category: Option<String>,

        /// Filter by tag (repeatable, comma-separated supported)
        #[arg(long = "tag", value_delimiter = ',')]
        tags: Vec<String>,

        /// Maximum number of results (default: 20, max: 50)
        #[arg(long)]
        limit: Option<usize>,

        /// Pagination cursor for next page
        #[arg(long)]
        cursor: Option<String>,

        /// Registry URL (default: https://api.ato.run)
        #[arg(long)]
        registry: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,

        /// Disable interactive TUI even when running in TTY
        #[arg(long, default_value_t = false)]
        no_tui: bool,

        /// Show selected capsule's capsule.toml in the TUI right panel
        #[arg(long, default_value_t = false)]
        show_manifest: bool,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Fetch an artifact into local cache for debugging or manual workflows"
    )]
    Fetch {
        /// Capsule ref (`publisher/slug[@version]` or `localhost:8080/slug:version`)
        capsule_ref: String,

        /// Registry URL override (or embed registry in `capsule_ref`)
        #[arg(long)]
        registry: Option<String>,

        /// Specific version to fetch (or use publisher/slug@version)
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
        /// Path to fetched artifact directory created by `ato fetch`
        fetched_artifact_dir: PathBuf,

        /// Allow external finalize execution (`codesign`) for this PoC
        #[arg(long, default_value_t = false)]
        allow_external_finalize: bool,

        /// Output directory for derived artifacts
        #[arg(long)]
        output_dir: PathBuf,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(
        next_help_heading = "Advanced Commands",
        about = "Add a finalized app to launcher surfaces (experimental)"
    )]
    Project {
        /// Path to a finalized local derived artifact directory created by `ato finalize`
        derived_app_path: Option<PathBuf>,

        /// Override launcher surface directory (default: host-specific launcher dir)
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
        /// Projection ID, projected symlink path, or finalized derived .app path
        projection_ref: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(next_help_heading = "Management", about = "List running capsules")]
    Ps {
        /// Show all capsules including stopped ones
        #[arg(long, default_value_t = false)]
        all: bool,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    #[command(next_help_heading = "Management", about = "Stop a running capsule")]
    Stop {
        /// Capsule ID (from ps output)
        #[arg(long)]
        id: Option<String>,

        /// Capsule name (partial match)
        #[arg(long)]
        name: Option<String>,

        /// Stop all capsules matching the name
        #[arg(long, default_value_t = false)]
        all: bool,

        /// Force kill (SIGKILL) instead of graceful shutdown (SIGTERM)
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    #[command(
        next_help_heading = "Management",
        about = "Show logs of a running capsule"
    )]
    Logs {
        /// Capsule ID (from ps output)
        #[arg(long)]
        id: Option<String>,

        /// Capsule name (partial match)
        #[arg(long)]
        name: Option<String>,

        /// Follow log output in real-time
        #[arg(long, default_value_t = false)]
        follow: bool,

        /// Show last N lines
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
        /// GitHub Personal Access Token (legacy fallback, scope: read:user)
        #[arg(long)]
        token: Option<String>,

        /// Do not open browser automatically; print activation URL for another device/session
        #[arg(long, default_value_t = false)]
        headless: bool,
    },

    #[command(next_help_heading = "Auth", about = "Logout")]
    Logout,

    #[command(
        next_help_heading = "Auth",
        about = "Show current authentication status"
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
        /// Registry URL override (default: My Dock when logged in; official Store remains CI-first)
        #[arg(long)]
        registry: Option<String>,

        /// Use prebuilt .capsule artifact (skip repackaging for private registry publish)
        #[arg(long, value_name = "PATH", conflicts_with_all = ["ci", "dry_run"])]
        artifact: Option<PathBuf>,

        /// Explicit scoped ID for artifact publish (publisher/slug)
        #[arg(
            long,
            value_name = "PUBLISHER/SLUG",
            conflicts_with_all = ["ci", "dry_run"],
            requires = "artifact"
        )]
        scoped_id: Option<String>,

        /// Allow idempotent success when same version already exists with identical sha256
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        allow_existing: bool,

        /// Run prepare phase
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        prepare: bool,

        /// Run build phase
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        build: bool,

        /// Run deploy phase
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        deploy: bool,

        /// Use legacy default phases (prepare/build/deploy) for official registry publish
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        legacy_full_publish: bool,

        /// Allow publishing payloads larger than 200MB
        #[arg(long, default_value_t = false)]
        force_large_payload: bool,

        /// Auto-fix official CI workflow once, then rerun diagnostics exactly once
        #[arg(long, default_value_t = false, conflicts_with_all = ["ci", "dry_run"])]
        fix: bool,

        /// Publish from GitHub Actions with OIDC token (CI-only mode)
        #[arg(long, conflicts_with = "dry_run")]
        ci: bool,

        /// Validate local capsule build inputs without publishing
        #[arg(long, conflicts_with = "ci")]
        dry_run: bool,

        /// Disable interactive TUI and show CI guidance instead
        #[arg(long, conflicts_with_all = ["ci", "dry_run", "json"])]
        no_tui: bool,

        /// Emit machine-readable JSON output
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
        /// Engine name to install (default: nacelle)
        #[arg(long, default_value = "nacelle")]
        engine: String,

        /// Engine version (default: latest)
        #[arg(long)]
        version: Option<String>,

        /// Skip SHA256 verification
        #[arg(long, default_value_t = false)]
        skip_verify: bool,
    },

    #[command(hide = true)]
    Open {
        /// Path to a .capsule archive or directory containing capsule.toml
        #[arg()]
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

        /// Skip prompt and auto-install when app-id is not installed
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,
    },

    #[command(hide = true)]
    New {
        /// Project name
        name: String,

        /// Template type: python, node, hono, rust, go, shell
        #[arg(long, default_value = "python")]
        template: String,
    },

    #[command(hide = true)]
    Keygen {
        /// Output base path (default: ./private.key and ./public.key)
        #[arg(long)]
        out: Option<PathBuf>,

        /// Overwrite existing keys
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Output keys in StoredKey JSON format
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    #[command(hide = true)]
    Pack {
        /// Directory containing capsule.toml (default: ".")
        #[arg(default_value = ".")]
        dir: PathBuf,

        /// Initialize capsule.toml interactively if not found
        #[arg(long)]
        init: bool,

        /// Path to signing key (optional)
        #[arg(long)]
        key: Option<PathBuf>,

        /// Network enforcement mode
        #[arg(long, value_enum, default_value_t = EnforcementMode::Strict)]
        enforcement: EnforcementMode,

        /// Create self-extracting executable installer (includes nacelle runtime)
        #[arg(long)]
        standalone: bool,

        /// Allow building payloads larger than 200MB
        #[arg(long, hide = true, default_value_t = false)]
        force_large_payload: bool,

        /// Keep failed build artifacts when smoke test fails
        #[arg(long, hide = true, default_value_t = false)]
        keep_failed_artifacts: bool,

        /// Print per-phase build timings
        #[arg(long, hide = true, default_value_t = false)]
        timings: bool,

        /// Disallow fallback when source_digest/CAS(v3 path) is unavailable
        #[arg(long, hide = true, default_value_t = false)]
        strict_v3: bool,
    },

    #[command(hide = true)]
    Scaffold {
        #[command(subcommand)]
        command: ScaffoldCommands,
    },

    #[command(hide = true)]
    Sign {
        /// File to sign
        target: PathBuf,

        /// Path to the secret key
        #[arg(long)]
        key: PathBuf,

        /// Output signature path (default: <target>.sig)
        #[arg(long)]
        out: Option<PathBuf>,
    },

    #[command(hide = true)]
    Verify {
        /// File to verify (the artifact, not the .sig file)
        target: PathBuf,

        /// Path to the signature file (default: <target>.sig)
        #[arg(long)]
        sig: Option<PathBuf>,

        /// Expected signer DID or developer key (optional, for additional check)
        #[arg(long)]
        signer: Option<String>,

        /// Emit machine-readable JSON output
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
    Close {
        /// Capsule ID (from ps output)
        #[arg(long)]
        id: Option<String>,

        /// Capsule name (partial match)
        #[arg(long)]
        name: Option<String>,

        /// Stop all capsules matching the name
        #[arg(long, default_value_t = false)]
        all: bool,

        /// Force kill (SIGKILL) instead of graceful shutdown (SIGTERM)
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    #[command(hide = true)]
    Guest {
        /// Path to a .sync archive
        #[arg()]
        sync_path: PathBuf,
    },

    #[command(hide = true)]
    Ipc {
        #[command(subcommand)]
        command: IpcCommands,
    },

    #[command(hide = true)]
    Auth,
}

#[derive(Subcommand)]
enum InspectCommands {
    #[command(about = "Inspect runtime requirements from capsule.toml")]
    Requirements {
        /// Local path or scoped store ID (publisher/slug)
        target: String,

        /// Registry URL override for remote inspection
        #[arg(long)]
        registry: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProjectCommands {
    #[command(
        about = "List experimental projection state and detect broken projections read-only"
    )]
    Ls {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum KeyCommands {
    /// Generate a new signing keypair
    Gen {
        /// Output base path (default: ./private.key and ./public.key)
        #[arg(long)]
        out: Option<PathBuf>,

        /// Overwrite existing keys
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Output keys in StoredKey JSON format
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Sign an existing artifact
    Sign {
        /// File to sign
        target: PathBuf,

        /// Path to the secret key
        #[arg(long)]
        key: PathBuf,

        /// Output signature path (default: <target>.sig)
        #[arg(long)]
        out: Option<PathBuf>,
    },

    /// Verify a signed artifact
    Verify {
        /// File to verify (the artifact, not the .sig file)
        target: PathBuf,

        /// Path to the signature file (default: <target>.sig)
        #[arg(long)]
        sig: Option<PathBuf>,

        /// Expected signer DID or developer key (optional, for additional check)
        #[arg(long)]
        signer: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Engine configuration
    Engine {
        #[command(subcommand)]
        command: ConfigEngineCommands,
    },

    /// Registry configuration
    Registry {
        #[command(subcommand)]
        command: ConfigRegistryCommands,
    },
}

#[derive(Subcommand)]
enum ConfigEngineCommands {
    /// Show engine capabilities (JSON)
    Features,

    /// Register a nacelle engine binary (writes ~/.ato/config.toml)
    Register {
        /// Registration name (e.g. "default" or "my-custom-nacelle")
        #[arg(long)]
        name: String,

        /// Path to nacelle engine binary (if omitted, uses NACELLE_PATH)
        #[arg(long)]
        path: Option<PathBuf>,

        /// Set this registration as the default engine
        #[arg(long, default_value_t = false)]
        default: bool,
    },

    /// Download and install an engine
    Install {
        /// Engine name to install (default: nacelle)
        #[arg(long, default_value = "nacelle")]
        engine: String,

        /// Engine version (default: latest)
        #[arg(long)]
        version: Option<String>,

        /// Skip SHA256 verification
        #[arg(long, default_value_t = false)]
        skip_verify: bool,
    },
}

#[derive(Subcommand)]
enum ConfigRegistryCommands {
    /// Resolve registry for a domain
    Resolve {
        /// Domain to resolve (e.g., example.com)
        domain: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// List configured registries
    List {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Clear registry cache
    ClearCache,
}

#[derive(Subcommand)]
enum IpcCommands {
    /// Show status of running IPC services
    Status {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Start an IPC service from a capsule directory
    Start {
        /// Path to capsule directory or capsule.toml
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Stop a running IPC service
    Stop {
        /// Service name to stop
        #[arg(long)]
        name: String,

        /// Force kill (SIGKILL) instead of graceful shutdown (SIGTERM)
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Validate and send a JSON-RPC invoke request
    Invoke {
        /// Path to capsule directory or capsule.toml
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Override exported service name
        #[arg(long)]
        service: Option<String>,

        /// Method name to invoke
        #[arg(long)]
        method: String,

        /// JSON arguments payload
        #[arg(long)]
        args: String,

        /// JSON-RPC request id
        #[arg(long, default_value = "invoke-1")]
        id: String,

        /// Maximum serialized message size in bytes
        #[arg(long)]
        max_message_size: Option<usize>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ScaffoldCommands {
    /// Generate a Dockerfile + .dockerignore for running a self-extracting bundle
    Docker {
        /// Path to capsule.toml
        #[arg(long, default_value = "capsule.toml")]
        manifest: PathBuf,

        /// Output Dockerfile path (default: <manifest dir>/Dockerfile)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Output directory (default: manifest directory). Ignored if --output is set.
        #[arg(long)]
        output_dir: Option<PathBuf>,

        /// Overwrite existing files
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCommands {
    /// Create a new profile.sync
    Create {
        /// Display name
        #[arg(long)]
        name: String,

        /// Short bio
        #[arg(long)]
        bio: Option<String>,

        /// Path to avatar image (png/jpg)
        #[arg(long)]
        avatar: Option<PathBuf>,

        /// Path to signing key (JSON format)
        #[arg(long)]
        key: PathBuf,

        /// Output path (default: ./profile.sync)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Website URL
        #[arg(long)]
        website: Option<String>,

        /// GitHub username
        #[arg(long)]
        github: Option<String>,

        /// Twitter/X handle
        #[arg(long)]
        twitter: Option<String>,
    },

    /// Show profile info from a profile.sync file
    Show {
        /// Path to profile.sync
        #[arg()]
        path: PathBuf,

        /// Emit JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum EngineCommands {
    /// Show engine capabilities (JSON)
    Features,

    /// Register a nacelle engine binary (writes ~/.ato/config.toml)
    Register {
        /// Registration name (e.g. "default" or "my-custom-nacelle")
        #[arg(long)]
        name: String,

        /// Path to nacelle engine binary (if omitted, uses NACELLE_PATH)
        #[arg(long)]
        path: Option<PathBuf>,

        /// Set this registration as the default engine
        #[arg(long, default_value_t = false)]
        default: bool,
    },
}

#[derive(Subcommand)]
enum RegistryCommands {
    /// Resolve registry for a domain
    Resolve {
        /// Domain to resolve (e.g., example.com)
        domain: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// List configured registries
    List {
        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Clear registry cache
    ClearCache,

    /// Start local HTTP registry server for offline development
    Serve {
        /// Listen port
        #[arg(long, default_value_t = 8787)]
        port: u16,

        /// Data directory for local registry state
        #[arg(long, default_value = "~/.ato/local-registry")]
        data_dir: String,

        /// Listen host (non-loopback requires --auth-token)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Bearer token required for write API (recommended when exposing non-loopback host)
        #[arg(long)]
        auth_token: Option<String>,
    },
}

#[derive(Subcommand)]
enum StateCommands {
    /// List registered persistent states
    #[command(visible_alias = "ls")]
    List {
        /// Filter by owner scope
        #[arg(long)]
        owner_scope: Option<String>,

        /// Filter by manifest state name
        #[arg(long)]
        state_name: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Inspect one persistent state by state-id or ato-state:// URI
    Inspect {
        /// State reference (`state-...` or `ato-state://state-...`)
        state_ref: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Register a persistent state from a manifest contract
    Register {
        /// Path to capsule directory or capsule.toml
        #[arg(long, default_value = ".")]
        manifest: PathBuf,

        /// State name from [state.<name>]
        #[arg(long = "name")]
        state_name: String,

        /// Absolute host directory to bind to this state contract
        #[arg(long = "path", value_name = "/ABS/PATH")]
        path: PathBuf,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum BindingCommands {
    /// List registered host-side service bindings
    #[command(visible_alias = "ls")]
    List {
        /// Filter by owner scope
        #[arg(long)]
        owner_scope: Option<String>,

        /// Filter by service name
        #[arg(long)]
        service_name: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Inspect one host-side service binding by binding-id
    Inspect {
        /// Binding reference (`binding-...`)
        binding_ref: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Resolve a host-side service binding by owner scope, service, and kind
    Resolve {
        /// Binding owner scope
        #[arg(long)]
        owner_scope: String,

        /// Service name from [services.<name>]
        #[arg(long)]
        service_name: String,

        /// Binding kind to resolve
        #[arg(long, default_value = "ingress")]
        binding_kind: String,

        /// Optional caller service for allow_from-restricted bindings
        #[arg(long)]
        caller_service: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Explicitly bootstrap TLS assets and optional trust installation for an ingress binding
    BootstrapTls {
        /// Binding reference (`binding-...`)
        #[arg(long = "binding")]
        binding_ref: String,

        /// Attempt to install the generated certificate into the local user trust store
        #[arg(long, default_value_t = false)]
        install_system_trust: bool,

        /// Skip the interactive consent prompt after reviewing the trust action
        #[arg(short = 'y', long = "yes", default_value_t = false)]
        yes: bool,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Run a host-side ingress reverse proxy for a registered binding
    ServeIngress {
        /// Binding reference (`binding-...`)
        #[arg(long = "binding")]
        binding_ref: String,

        /// Path to capsule directory or capsule.toml used to derive the upstream port
        #[arg(long, default_value = ".")]
        manifest: PathBuf,

        /// Optional upstream URL override
        #[arg(long)]
        upstream_url: Option<String>,
    },

    /// Register a host-side ingress binding from a manifest service
    RegisterIngress {
        /// Path to capsule directory or capsule.toml
        #[arg(long, default_value = ".")]
        manifest: PathBuf,

        /// Service name from [services.<name>]
        #[arg(long)]
        service_name: String,

        /// Host-side ingress URL (http:// or https://)
        #[arg(long)]
        url: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Register a local cross-capsule service binding for a separately launched service
    RegisterService {
        /// Path to capsule directory or capsule.toml
        #[arg(long, default_value = ".")]
        manifest: PathBuf,

        /// Service name from [services.<name>]
        #[arg(long)]
        service_name: String,

        /// Loopback URL for the running local service (http://localhost:PORT or http://127.0.0.1:PORT)
        #[arg(long)]
        url: Option<String>,

        /// Running local process id to derive manifest and target metadata from
        #[arg(long)]
        process_id: Option<String>,

        /// Override the loopback port when registering from a running process
        #[arg(long)]
        port: Option<u16>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },

    /// Auto-register all eligible local service bindings from a running process
    SyncProcess {
        /// Running local process id
        #[arg(long)]
        process_id: String,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SourceCommands {
    /// Show sync run status for a source
    SyncStatus {
        /// Source ID
        #[arg(long = "source-id")]
        source_id: String,

        /// Sync run ID
        #[arg(long = "sync-run-id")]
        sync_run_id: String,

        /// Registry URL
        #[arg(long)]
        registry: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Trigger rebuild/re-sign flow for a source
    Rebuild {
        /// Source ID
        #[arg(long = "source-id")]
        source_id: String,

        /// Optional ref (branch/tag/SHA)
        #[arg(long = "ref", alias = "reference")]
        reference: Option<String>,

        /// Wait and fetch status after trigger
        #[arg(long, default_value_t = false)]
        wait: bool,

        /// Registry URL
        #[arg(long)]
        registry: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum PackageCommands {
    /// Search published packages in the store
    Search {
        /// Search query (e.g., "note", "ai chat")
        query: Option<String>,

        /// Filter by category
        #[arg(long)]
        category: Option<String>,

        /// Filter by tag (repeatable, comma-separated supported)
        #[arg(long = "tag", value_delimiter = ',')]
        tags: Vec<String>,

        /// Maximum number of results (default: 20, max: 50)
        #[arg(long)]
        limit: Option<usize>,

        /// Pagination cursor for next page
        #[arg(long)]
        cursor: Option<String>,

        /// Registry URL (default: https://api.ato.run)
        #[arg(long)]
        registry: Option<String>,

        /// Emit machine-readable JSON output
        #[arg(long)]
        json: bool,

        /// Disable interactive TUI even when running in TTY
        #[arg(long, default_value_t = false)]
        no_tui: bool,

        /// Show selected capsule's capsule.toml in the TUI right panel
        #[arg(long, default_value_t = false)]
        show_manifest: bool,
    },
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json_mode = args.iter().any(|arg| arg == "--json");
    let command_context = diagnostics::detect_command_context(&args);

    if let Err(err) = run() {
        if json_mode && commands::inspect::try_emit_json_error(&err) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        if ato_error_jsonl::try_emit_from_anyhow(&err, json_mode) {
            std::process::exit(error_codes::EXIT_USER_ERROR);
        }

        let diagnostic = diagnostics::from_anyhow(&err, command_context);
        let exit_code = diagnostics::map_exit_code(&diagnostic, &err);

        if json_mode {
            if let Ok(payload) = serde_json::to_string(&diagnostic.to_json_envelope()) {
                println!("{}", payload);
            } else {
                println!(
                    r#"{{"schema_version":"1","type":"error","code":"E999","message":"failed to serialize error payload","causes":[]}}"#
                );
            }
        } else {
            eprintln!("{:?}", miette::Report::new(diagnostic));
        }

        std::process::exit(exit_code);
    }
}

fn run() -> Result<()> {
    let is_no_args = std::env::args_os().count() == 1;

    if is_no_args {
        print_animated_logo();
        let mut cmd = Cli::command();
        cmd.print_help().context("failed to print CLI help")?;
        println!();
        return Ok(());
    }

    let cli = Cli::parse();
    let reporter = std::sync::Arc::new(reporters::CliReporter::new(cli.json));

    match cli.command {
        Commands::Run {
            path,
            skill,
            from_skill,
            target,
            watch,
            background,
            nacelle,
            registry,
            state,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            yes,
            allow_unverified,
        } => execute_run_like_command(
            path,
            target,
            watch,
            background,
            nacelle,
            registry,
            state,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            yes,
            allow_unverified,
            skill,
            from_skill,
            None,
            reporter.clone(),
        ),

        Commands::Engine { command } => {
            execute_engine_command(command, cli.nacelle, reporter.clone())
        }

        Commands::Registry { command } => execute_registry_command(command),

        Commands::Setup {
            engine,
            version,
            skip_verify,
        } => execute_setup_command(engine, version, skip_verify, reporter.clone()),

        Commands::Open {
            path,
            target,
            watch,
            background,
            nacelle,
            registry,
            state,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            yes,
        } => execute_run_like_command(
            path,
            target,
            watch,
            background,
            nacelle,
            registry,
            state,
            inject,
            enforcement,
            sandbox_mode,
            unsafe_mode_legacy,
            unsafe_bypass_sandbox_legacy,
            dangerously_skip_permissions,
            yes,
            false,
            None,
            None,
            Some("⚠️  'ato open' is deprecated. Use 'ato run' instead."),
            reporter.clone(),
        ),

        Commands::Init => init::execute_prompt(init::PromptArgs { path: None }, reporter.clone()),

        Commands::New { name, template } => new::execute(
            new::NewArgs {
                name,
                template: Some(template),
            },
            reporter.clone(),
        ),

        Commands::Build {
            dir,
            init,
            key,
            standalone,
            force_large_payload,
            enforcement,
            keep_failed_artifacts,
            timings,
            strict_v3,
        } => {
            let result = commands::build::execute_pack_command(
                dir,
                init,
                key,
                standalone,
                force_large_payload,
                keep_failed_artifacts,
                strict_v3,
                enforcement.as_str().to_string(),
                reporter.clone(),
                timings,
                cli.json,
                cli.nacelle,
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Ok(())
        }

        Commands::Validate { path, json } => {
            commands::validate::execute(path, cli.json || json)?;
            Ok(())
        }

        Commands::Update => {
            commands::update::update()?;
            Ok(())
        }

        Commands::Inspect { command } => match command {
            InspectCommands::Requirements {
                target,
                registry,
                json,
            } => {
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    commands::inspect::execute_requirements(target, registry, cli.json || json)
                        .await
                        .map(|_| ())
                        .map_err(anyhow::Error::from)
                })
            }
        },

        Commands::Keygen { out, force, json } => {
            keygen::execute(keygen::KeygenArgs { out, force, json }, reporter.clone())
        }

        Commands::Key { command } => match command {
            KeyCommands::Gen { out, force, json } => {
                keygen::execute(keygen::KeygenArgs { out, force, json }, reporter.clone())
            }
            KeyCommands::Sign { target, key, out } => {
                sign::execute(sign::SignArgs { target, key, out }, reporter.clone())
            }
            KeyCommands::Verify {
                target,
                sig,
                signer,
                json,
            } => verify::execute(
                verify::VerifyArgs {
                    target,
                    sig,
                    signer,
                    json,
                },
                reporter.clone(),
            ),
        },

        Commands::Pack {
            dir,
            init,
            key,
            standalone,
            force_large_payload,
            enforcement,
            keep_failed_artifacts,
            timings,
            strict_v3,
        } => {
            eprintln!("⚠️  'ato pack' is deprecated. Use 'ato build' instead.");
            let result = commands::build::execute_pack_command(
                dir,
                init,
                key,
                standalone,
                force_large_payload,
                keep_failed_artifacts,
                strict_v3,
                enforcement.as_str().to_string(),
                reporter.clone(),
                timings,
                cli.json,
                cli.nacelle,
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Ok(())
        }

        Commands::Scaffold {
            command:
                ScaffoldCommands::Docker {
                    manifest,
                    output,
                    output_dir,
                    force,
                },
        } => scaffold::execute_docker(
            scaffold::ScaffoldDockerArgs {
                manifest_path: manifest,
                output_dir,
                output,
                force,
            },
            reporter.clone(),
        ),

        Commands::Sign { target, key, out } => {
            sign::execute(sign::SignArgs { target, key, out }, reporter.clone())
        }

        Commands::Verify {
            target,
            sig,
            signer,
            json,
        } => verify::execute(
            verify::VerifyArgs {
                target,
                sig,
                signer,
                json,
            },
            reporter.clone(),
        ),

        Commands::Profile { command } => match command {
            ProfileCommands::Create {
                name,
                bio,
                avatar,
                key,
                output,
                website,
                github,
                twitter,
            } => profile::execute_create(
                profile::CreateArgs {
                    name,
                    bio,
                    avatar,
                    key,
                    output,
                    website,
                    github,
                    twitter,
                },
                reporter.clone(),
            ),
            ProfileCommands::Show { path, json } => {
                profile::execute_show(profile::ShowArgs { path, json }, reporter.clone())
            }
        },

        Commands::Install {
            slug,
            from_gh_repo,
            registry,
            version,
            default,
            yes,
            skip_verify_legacy,
            allow_unverified,
            output,
            project,
            no_project,
            json,
        } => {
            if skip_verify_legacy {
                anyhow::bail!(
                    "--skip-verify is no longer supported. Signature/hash verification is always required."
                );
            }
            let projection_preference = if project {
                install::ProjectionPreference::Force
            } else if no_project {
                install::ProjectionPreference::Skip
            } else {
                install::ProjectionPreference::Prompt
            };
            let can_prompt = !json
                && can_prompt_interactively(
                    std::io::stdin().is_terminal(),
                    std::io::stderr().is_terminal(),
                );
            let rt = tokio::runtime::Runtime::new()?;

            if let Some(repository) = from_gh_repo {
                if registry.is_some() {
                    anyhow::bail!("--registry cannot be used with --from-gh-repo");
                }
                if version.is_some() {
                    anyhow::bail!("--version cannot be used with --from-gh-repo");
                }
                let result = rt.block_on(install_github_repository(
                    &repository,
                    output,
                    yes,
                    projection_preference,
                    json,
                    can_prompt,
                ))?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("\n✅ Installation complete!");
                    println!("   Capsule: {}", result.slug);
                    println!("   Version: {}", result.version);
                    println!("   Path:    {}", result.path.display());
                    println!("   Hash:    {}", result.content_hash);
                }
                return Ok(());
            }

            rt.block_on(async {

                let slug = slug.ok_or_else(|| {
                    anyhow::anyhow!("capsule slug is required when not using --from-gh-repo")
                })?;
                if install::is_slug_only_ref(&slug) {
                    let suggestions = install::suggest_scoped_capsules(
                        &slug,
                        registry.as_deref(),
                        5,
                    )
                    .await?;
                    if suggestions.is_empty() {
                        anyhow::bail!(
                            "scoped_id_required: '{}' is ambiguous. Use publisher/slug (for example: koh0920/{})",
                            slug,
                            slug
                        );
                    }
                    let mut message = format!(
                        "scoped_id_required: '{}' requires publisher scope.\n\nDid you mean one of these?",
                        slug
                    );
                    for suggestion in suggestions {
                        message.push_str(&format!(
                            "\n  - {}  ({} downloads)",
                            suggestion.scoped_id, suggestion.downloads
                        ));
                    }
                    message.push_str("\n\nRun `ato search ");
                    message.push_str(&slug);
                    message.push_str("` to see more options.");
                    anyhow::bail!(message);
                }

                let result = install::install_app(
                    &slug,
                    registry.as_deref(),
                    version.as_deref(),
                    output,
                    default,
                    yes,
                    projection_preference,
                    allow_unverified,
                    false,
                    json,
                    can_prompt,
                )
                .await?;

                if json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("\n✅ Installation complete!");
                    println!("   Capsule: {}", result.slug);
                    println!("   Version: {}", result.version);
                    println!("   Path:    {}", result.path.display());
                    println!("   Hash:    {}", result.content_hash);
                    if let Some(launchable) = &result.launchable {
                        match launchable {
                            install::LaunchableTarget::CapsuleArchive { path } => {
                                println!("   Launch:  ato run {}", path.display());
                            }
                            install::LaunchableTarget::DerivedApp { path } => {
                                println!("   App:     {}", path.display());
                            }
                        }
                    }
                    if let Some(projection) = &result.projection {
                        if projection.performed {
                            if let Some(projected_path) = &projection.projected_path {
                                println!("   Launcher: {}", projected_path.display());
                            }
                        } else if no_project {
                            println!("   Launcher: skipped");
                        }
                    }
                }
                Ok(())
            })
        }

        Commands::Search {
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        } => execute_search_command(
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        ),

        Commands::Fetch {
            capsule_ref,
            registry,
            version,
            json,
        } => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                if install::is_slug_only_ref(&capsule_ref) {
                    let suggestions =
                        install::suggest_scoped_capsules(&capsule_ref, registry.as_deref(), 5)
                            .await?;
                    if suggestions.is_empty() {
                        anyhow::bail!(
                            "scoped_id_required: '{}' is ambiguous. Use publisher/slug (for example: koh0920/{})",
                            capsule_ref,
                            capsule_ref
                        );
                    }
                    let mut message = format!(
                        "scoped_id_required: '{}' requires publisher scope.\n\nDid you mean one of these?",
                        capsule_ref
                    );
                    for suggestion in suggestions {
                        message.push_str(&format!(
                            "\n  - {}  ({} downloads)",
                            suggestion.scoped_id, suggestion.downloads
                        ));
                    }
                    anyhow::bail!(message);
                }

                let result = native_delivery::execute_fetch(
                    &capsule_ref,
                    registry.as_deref(),
                    version.as_deref(),
                )
                .await?;
                if cli.json || json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("✅ Fetched to: {}", result.cache_dir.display());
                    println!("   Scoped ID: {}", result.scoped_id);
                    println!("   Version:   {}", result.version);
                    println!("   Digest:    {}", result.parent_digest);
                }
                Ok(())
            })
        }

        Commands::Finalize {
            fetched_artifact_dir,
            allow_external_finalize,
            output_dir,
            json,
        } => {
            let result = native_delivery::execute_finalize(
                &fetched_artifact_dir,
                &output_dir,
                allow_external_finalize,
            )?;
            if cli.json || json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("✅ Finalized to: {}", result.output_dir.display());
                println!("   App:      {}", result.derived_app_path.display());
                println!("   Parent:   {}", result.parent_digest);
                println!("   Derived:  {}", result.derived_digest);
            }
            Ok(())
        }

        Commands::Project {
            derived_app_path,
            launcher_dir,
            json,
            command,
        } => match command {
            Some(ProjectCommands::Ls {
                json: subcommand_json,
            }) => {
                let result = native_delivery::execute_project_ls()?;
                if cli.json || json || subcommand_json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else if result.projections.is_empty() {
                    println!("No experimental projections found.");
                } else {
                    for projection in result.projections {
                        let marker = if projection.state == "ok" {
                            "✅"
                        } else {
                            "⚠️"
                        };
                        println!(
                            "{} [{}] {} -> {}",
                            marker,
                            projection.state,
                            projection.projected_path.display(),
                            projection.derived_app_path.display()
                        );
                        println!("   ID:       {}", projection.projection_id);
                        if !projection.problems.is_empty() {
                            println!("   Problems: {}", projection.problems.join(", "));
                        }
                    }
                }
                Ok(())
            }
            None => {
                let derived_app_path = derived_app_path.ok_or_else(|| {
                    anyhow::anyhow!(
                        "ato project requires <DERIVED_APP_PATH> or use `ato project ls` for read-only status"
                    )
                })?;
                let result =
                    native_delivery::execute_project(&derived_app_path, launcher_dir.as_deref())?;
                if cli.json || json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("✅ Projected to: {}", result.projected_path.display());
                    println!("   ID:       {}", result.projection_id);
                    println!("   Target:   {}", result.derived_app_path.display());
                    println!("   State:    {}", result.state);
                    println!("   Metadata: {}", result.metadata_path.display());
                }
                Ok(())
            }
        },

        Commands::Unproject {
            projection_ref,
            json,
        } => {
            let result = native_delivery::execute_unproject(&projection_ref)?;
            if cli.json || json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("✅ Unprojected: {}", result.projected_path.display());
                println!("   ID:      {}", result.projection_id);
                println!("   State:   {}", result.state_before);
                println!(
                    "   Removed: metadata={}, symlink={}",
                    result.removed_metadata, result.removed_projected_path
                );
            }
            Ok(())
        }

        Commands::Config { command } => match command {
            ConfigCommands::Engine { command } => match command {
                ConfigEngineCommands::Features => {
                    execute_engine_command(EngineCommands::Features, cli.nacelle, reporter.clone())
                }
                ConfigEngineCommands::Register {
                    name,
                    path,
                    default,
                } => execute_engine_command(
                    EngineCommands::Register {
                        name,
                        path,
                        default,
                    },
                    cli.nacelle,
                    reporter.clone(),
                ),
                ConfigEngineCommands::Install {
                    engine,
                    version,
                    skip_verify,
                } => execute_setup_command(engine, version, skip_verify, reporter.clone()),
            },
            ConfigCommands::Registry { command } => {
                let mapped = match command {
                    ConfigRegistryCommands::Resolve { domain, json } => {
                        RegistryCommands::Resolve { domain, json }
                    }
                    ConfigRegistryCommands::List { json } => RegistryCommands::List { json },
                    ConfigRegistryCommands::ClearCache => RegistryCommands::ClearCache,
                };
                execute_registry_command(mapped)
            }
        },

        Commands::Publish {
            registry,
            artifact,
            scoped_id,
            allow_existing,
            prepare,
            build,
            deploy,
            legacy_full_publish,
            force_large_payload,
            fix,
            ci,
            dry_run,
            no_tui,
            json,
        } => {
            if ci {
                execute_publish_ci_command(json, force_large_payload, reporter.clone())
            } else if dry_run {
                execute_publish_dry_run_command(json, reporter.clone())
            } else {
                execute_publish_command(
                    PublishCommandArgs {
                        registry,
                        artifact,
                        scoped_id,
                        allow_existing,
                        prepare,
                        build,
                        deploy,
                        legacy_full_publish,
                        force_large_payload,
                        fix,
                        no_tui,
                        json,
                    },
                    reporter.clone(),
                )
            }
        }

        Commands::GenCi => gen_ci::execute(reporter.clone()),

        Commands::Package {
            command:
                PackageCommands::Search {
                    query,
                    category,
                    tags,
                    limit,
                    cursor,
                    registry,
                    json,
                    no_tui,
                    show_manifest,
                },
        } => execute_search_command(
            query,
            category,
            tags,
            limit,
            cursor,
            registry,
            json,
            no_tui,
            show_manifest,
        ),

        Commands::Source { command } => match command {
            SourceCommands::SyncStatus {
                source_id,
                sync_run_id,
                registry,
                json,
            } => execute_source_sync_status_command(source_id, sync_run_id, registry, json),
            SourceCommands::Rebuild {
                source_id,
                reference,
                wait,
                registry,
                json,
            } => execute_source_rebuild_command(source_id, reference, wait, registry, json),
        },

        Commands::Ps { all, json } => {
            commands::ps::execute(commands::ps::PsArgs { all, json }, reporter.clone())
        }

        Commands::Stop {
            id,
            name,
            all,
            force,
        } => commands::close::execute(
            commands::close::CloseArgs {
                id,
                name,
                all,
                force,
            },
            reporter.clone(),
        ),

        Commands::Close {
            id,
            name,
            all,
            force,
        } => commands::close::execute(
            commands::close::CloseArgs {
                id,
                name,
                all,
                force,
            },
            reporter.clone(),
        ),

        Commands::Logs {
            id,
            name,
            follow,
            tail,
        } => commands::logs::execute(
            commands::logs::LogsArgs {
                id,
                name,
                follow,
                tail,
            },
            reporter.clone(),
        ),

        Commands::State { command } => execute_state_command(command),

        Commands::Binding { command } => execute_binding_command(command),

        Commands::Guest { sync_path } => {
            commands::guest::execute(commands::guest::GuestArgs { sync_path })
        }

        Commands::Ipc {
            command: IpcCommands::Status { json },
        } => commands::ipc::run_ipc_status(json),

        Commands::Ipc {
            command: IpcCommands::Start { path, json },
        } => commands::ipc::run_ipc_start(path, json),

        Commands::Ipc {
            command: IpcCommands::Stop { name, force, json },
        } => commands::ipc::run_ipc_stop(name, force, json),

        Commands::Ipc {
            command:
                IpcCommands::Invoke {
                    path,
                    service,
                    method,
                    args,
                    id,
                    max_message_size,
                    json,
                },
        } => commands::ipc::run_ipc_invoke(path, service, method, args, id, max_message_size, json),

        Commands::Login { token, headless } => {
            let rt = tokio::runtime::Runtime::new()?;
            match token {
                Some(token) => rt.block_on(auth::login_with_token(token)),
                None => rt.block_on(auth::login_with_store_device_flow(headless)),
            }
        }

        Commands::Logout => auth::logout(),

        Commands::Whoami => auth::status(),

        Commands::Auth => auth::status(),
    }
}

fn execute_engine_command(
    command: EngineCommands,
    nacelle_override: Option<PathBuf>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    match command {
        EngineCommands::Features => {
            let nacelle =
                capsule_core::engine::discover_nacelle(capsule_core::engine::EngineRequest {
                    explicit_path: nacelle_override,
                    manifest_path: None,
                })?;
            let payload = json!({ "spec_version": "0.1.0" });
            let resp = capsule_core::engine::run_internal(&nacelle, "features", &payload)?;
            let body = serde_json::to_string_pretty(&resp)?;
            futures::executor::block_on(reporter.notify(body))?;
            Ok(())
        }
        EngineCommands::Register {
            name,
            path,
            default,
        } => {
            let resolved_path = if let Some(p) = path {
                p
            } else if let Ok(env_path) = std::env::var("NACELLE_PATH") {
                PathBuf::from(env_path)
            } else {
                anyhow::bail!("Missing --path and NACELLE_PATH is not set");
            };

            let validated =
                capsule_core::engine::discover_nacelle(capsule_core::engine::EngineRequest {
                    explicit_path: Some(resolved_path),
                    manifest_path: None,
                })?;

            let mut cfg = capsule_core::config::load_config()?;
            cfg.engines.insert(
                name.clone(),
                capsule_core::config::EngineRegistration {
                    path: validated.display().to_string(),
                },
            );
            if default {
                cfg.default_engine = Some(name.clone());
            }
            capsule_core::config::save_config(&cfg)?;

            futures::executor::block_on(reporter.notify(format!(
                "✅ Registered engine '{}' -> {}",
                name,
                validated.display()
            )))?;
            if default {
                futures::executor::block_on(
                    reporter.notify("✅ Set as default engine".to_string()),
                )?;
            }
            Ok(())
        }
    }
}

fn execute_registry_command(command: RegistryCommands) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        match command {
            RegistryCommands::Resolve { domain, json } => {
                let resolver = registry::RegistryResolver::default();
                match resolver.resolve(&domain).await {
                    Ok(info) => {
                        if json {
                            println!("{}", serde_json::to_string_pretty(&info)?);
                        } else {
                            println!("📡 Registry for {}:", domain);
                            println!("   URL:    {}", info.url);
                            if let Some(name) = &info.name {
                                println!("   Name:   {}", name);
                            }
                            if let Some(key) = &info.public_key {
                                println!("   Key:    {}", key);
                            }
                            println!("   Source: {:?}", info.source);
                        }
                    }
                    Err(e) => {
                        if json {
                            println!(r#"{{"error": "{}"}}"#, e);
                        } else {
                            eprintln!("❌ Failed to resolve registry: {}", e);
                        }
                    }
                }
                Ok(())
            }
            RegistryCommands::List { json } => {
                let resolver = registry::RegistryResolver::default();
                let info = resolver.resolve_for_app("default").await?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&[&info])?);
                } else {
                    println!("📋 Configured registries:");
                    println!(
                        "   • {} ({})",
                        info.url,
                        format!("{:?}", info.source).to_lowercase()
                    );
                }
                Ok(())
            }
            RegistryCommands::ClearCache => {
                let cache = registry::RegistryCache::new();
                cache.clear()?;
                println!("✅ Registry cache cleared");
                Ok(())
            }
            RegistryCommands::Serve {
                port,
                data_dir,
                host,
                auth_token,
            } => {
                if host != "127.0.0.1"
                    && auth_token
                        .as_deref()
                        .map(str::trim)
                        .unwrap_or("")
                        .is_empty()
                {
                    anyhow::bail!("--auth-token is required when --host is not 127.0.0.1");
                }
                registry_serve::serve(registry_serve::RegistryServerConfig {
                    host,
                    port,
                    data_dir,
                    auth_token,
                })
                .await
            }
        }
    })
}

fn execute_state_command(command: StateCommands) -> Result<()> {
    match command {
        StateCommands::List {
            owner_scope,
            state_name,
            json,
        } => state::list_states(owner_scope.as_deref(), state_name.as_deref(), json),
        StateCommands::Inspect { state_ref, json } => state::inspect_state(&state_ref, json),
        StateCommands::Register {
            manifest,
            state_name,
            path,
            json,
        } => state::register_state_from_manifest(
            &manifest,
            &state_name,
            path.to_string_lossy().as_ref(),
            json,
        ),
    }
}

fn execute_binding_command(command: BindingCommands) -> Result<()> {
    match command {
        BindingCommands::List {
            owner_scope,
            service_name,
            json,
        } => binding::list_bindings(owner_scope.as_deref(), service_name.as_deref(), json),
        BindingCommands::Inspect { binding_ref, json } => {
            binding::inspect_binding(&binding_ref, json)
        }
        BindingCommands::Resolve {
            owner_scope,
            service_name,
            binding_kind,
            caller_service,
            json,
        } => binding::resolve_binding(
            &owner_scope,
            &service_name,
            &binding_kind,
            caller_service.as_deref(),
            json,
        ),
        BindingCommands::BootstrapTls {
            binding_ref,
            install_system_trust,
            yes,
            json,
        } => binding::bootstrap_ingress_tls(&binding_ref, install_system_trust, yes, json),
        BindingCommands::ServeIngress {
            binding_ref,
            manifest,
            upstream_url,
        } => binding::serve_ingress_binding(&binding_ref, &manifest, upstream_url.as_deref()),
        BindingCommands::RegisterIngress {
            manifest,
            service_name,
            url,
            json,
        } => binding::register_ingress_binding_from_manifest(&manifest, &service_name, &url, json),
        BindingCommands::RegisterService {
            manifest,
            service_name,
            url,
            process_id,
            port,
            json,
        } => match (url.as_deref(), process_id.as_deref()) {
            (Some(url), _) => {
                binding::register_service_binding_from_manifest(&manifest, &service_name, url, json)
            }
            (None, Some(process_id)) => binding::register_service_binding_from_process(
                process_id,
                &service_name,
                port,
                json,
            ),
            (None, None) => anyhow::bail!("register-service requires either --url or --process-id"),
        },
        BindingCommands::SyncProcess { process_id, json } => {
            binding::sync_service_bindings_from_process(&process_id, json)
        }
    }
}

fn execute_publish_ci_command(
    json_output: bool,
    force_large_payload: bool,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = publish_ci::execute(
            publish_ci::PublishCiArgs {
                json_output,
                force_large_payload,
            },
            reporter.clone(),
        )
        .await?;

        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("✅ Successfully published to Ato Store!");
            println!();
            println!(
                "📦 Capsule:   {} v{}",
                result.capsule_scoped_id, result.version
            );
            if let Some(sha256) = &result.artifact_sha256 {
                println!("🛡️  Integrity: sha256:{}", sha256);
            } else if let Some(blake3) = &result.artifact_blake3 {
                println!("🛡️  Integrity: {}", blake3);
            }
            println!();
            println!("🌐 Store URL:      {}", result.urls.store);
            if let Some(playground) = &result.urls.playground {
                println!("🎮 Playground URL: {}", playground);
            }
            println!();
            println!("👉 Next step: ato run {}", result.capsule_scoped_id);
            println!();
            println!("   Event ID:   {}", result.publish_event_id);
            println!("   Release ID: {}", result.release_id);
            println!("   Artifact:   {}", result.artifact_id);
            println!("   Status:     {}", result.verification_status);
        }
        futures::executor::block_on(
            reporter
                .notify("CI publish completed using GitHub OIDC workflow identity.".to_string()),
        )?;
        Ok(())
    })
}

fn execute_publish_dry_run_command(
    json_output: bool,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result =
            publish_dry_run::execute(publish_dry_run::PublishDryRunArgs { json_output }).await?;

        if json_output {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("✅ Dry-run successful! Your capsule is ready to be published via CI.");
            println!("   Capsule: {}", result.capsule_name);
            println!("   Version: {}", result.version);
            println!("   Artifact: {}", result.artifact_path.display());
            println!("   Size: {} bytes", result.artifact_size_bytes);
        }
        futures::executor::block_on(
            reporter.notify("Local publish dry-run completed (no upload performed).".to_string()),
        )?;
        Ok(())
    })
}

#[allow(dead_code)]
fn execute_publish_guidance_command(json_output: bool, registry_url: &str) -> Result<()> {
    if json_output {
        let payload = serde_json::json!({
            "ok": false,
            "code": "CI_ONLY_PUBLISH",
            "message": "Official registry publishing is CI-first. Use `ato publish --ci` in GitHub Actions, or `ato publish --dry-run` locally."
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!(
            "❌ Direct local publishing is disabled for official registry ({}).",
            registry_url
        );
        println!();
        println!("Ato uses a strict CI-first publishing model via GitHub Actions (OIDC).");
        println!("This guarantees published capsules match committed source.");
        println!();
        println!("👉 Next steps:");
        println!("  1. Run `ato gen-ci` to generate `.github/workflows/ato-publish.yml`.");
        println!("  2. Commit and tag your release (e.g. `git tag v0.1.0`).");
        println!("  3. Push the tag to GitHub (`git push origin v0.1.0`).");
        println!("  4. GitHub Actions runs `ato publish --ci` automatically.");
        println!();
        println!("💡 Tip: Run `ato publish --dry-run` to validate locally before pushing.");
        println!("💡 Private registry directly publish: `ato publish --registry <url>`");
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PublishCommandArgs {
    registry: Option<String>,
    artifact: Option<PathBuf>,
    scoped_id: Option<String>,
    allow_existing: bool,
    prepare: bool,
    build: bool,
    deploy: bool,
    legacy_full_publish: bool,
    force_large_payload: bool,
    fix: bool,
    no_tui: bool,
    json: bool,
}

#[derive(Debug, Clone, Copy)]
struct PublishPhaseSelection {
    prepare: bool,
    build: bool,
    deploy: bool,
    explicit_filter: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PublishPhaseResult {
    name: &'static str,
    selected: bool,
    ok: bool,
    status: &'static str,
    elapsed_ms: u64,
    actionable_fix: Option<String>,
    message: String,
    result_kind: Option<String>,
    skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishTargetMode {
    PersonalDockDirect,
    OfficialCi,
    CustomDirect,
}

impl PublishTargetMode {
    fn is_official(self) -> bool {
        matches!(self, Self::OfficialCi)
    }

    fn is_personal_dock(self) -> bool {
        matches!(self, Self::PersonalDockDirect)
    }

    fn route_label(self) -> &'static str {
        match self {
            Self::PersonalDockDirect => "personal_dock_direct",
            Self::OfficialCi => "official",
            Self::CustomDirect => "private",
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedPublishTarget {
    registry_url: String,
    mode: PublishTargetMode,
    publisher_handle: Option<String>,
}

#[derive(Debug, Clone)]
struct OfficialDeployOutcome {
    route: publish_official::PublishRoutePlan,
    fix_result: publish_official::WorkflowFixResult,
    diagnosis: publish_official::OfficialPublishDiagnosis,
}

fn execute_publish_command(
    args: PublishCommandArgs,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let resolved_target = resolve_publish_target(args.registry.clone())?;
    let is_official = resolved_target.mode.is_official();
    let selection = select_publish_phases(
        args.prepare,
        args.build,
        args.deploy,
        is_official,
        args.legacy_full_publish,
    );
    if resolved_target.mode.is_personal_dock() && selection.deploy {
        let _ = crate::auth::require_session_token()?;
    }
    validate_publish_phase_options(&args, selection, is_official)?;
    maybe_warn_legacy_full_publish(&args, selection, is_official);
    let _ = args.no_tui;

    let mut phases = vec![
        new_phase_result("prepare", selection.prepare),
        new_phase_result("build", selection.build),
        new_phase_result("deploy", selection.deploy),
    ];

    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let mut built_artifact_path: Option<PathBuf> = None;
    let mut private_result: Option<publish_private::PublishPrivateResult> = None;
    let mut official_result: Option<OfficialDeployOutcome> = None;

    let private_preview = if selection.deploy && !is_official {
        Some(publish_private::summarize(
            &publish_private::PublishPrivateArgs {
                registry_url: resolved_target.registry_url.clone(),
                publisher_hint: resolved_target.publisher_handle.clone(),
                artifact_path: args.artifact.clone(),
                force_large_payload: args.force_large_payload,
                scoped_id: args.scoped_id.clone(),
                allow_existing: args.allow_existing,
            },
        )?)
    } else {
        None
    };

    if selection.prepare {
        print_phase_line(args.json, "prepare", "RUN", "prepare command detection");
        let started = std::time::Instant::now();
        let prepare_spec = publish_prepare::detect_prepare_command(&cwd)?;
        match prepare_spec {
            Some(spec) => {
                let message = format!("running {}", spec.source.as_label());
                publish_prepare::run_prepare_command(&spec, &cwd, args.json)
                    .context("Failed to run publish prepare command")?;
                phase_mark_ok(
                    &mut phases[0],
                    started.elapsed().as_millis() as u64,
                    message.clone(),
                    None,
                );
                print_phase_line(args.json, "prepare", "OK", &message);
            }
            None => {
                let skipped_reason = "no prepare command configured".to_string();
                if selection.explicit_filter {
                    let fix = "capsule.toml に [build.lifecycle].prepare を設定するか package.json scripts[\"capsule:prepare\"] を追加して再実行してください。".to_string();
                    phase_mark_failed(
                        &mut phases[0],
                        started.elapsed().as_millis() as u64,
                        "--prepare was selected but no prepare command was found".to_string(),
                        Some(fix.clone()),
                    );
                    print_phase_line(args.json, "prepare", "FAIL", "prepare command not found");
                    if !args.json {
                        println!("👉 次に打つコマンド: {}", fix);
                    }
                    anyhow::bail!(
                        "--prepare was selected but no prepare command was found. Set `build.lifecycle.prepare` in capsule.toml or add package.json scripts[\"capsule:prepare\"]."
                    );
                }
                phase_mark_skipped(
                    &mut phases[0],
                    started.elapsed().as_millis() as u64,
                    skipped_reason.clone(),
                    skipped_reason.clone(),
                );
                print_phase_line(args.json, "prepare", "SKIP", &skipped_reason);
            }
        }
    } else {
        phase_mark_skipped(
            &mut phases[0],
            0,
            "prepare phase not selected".to_string(),
            "not selected".to_string(),
        );
        print_phase_line(args.json, "prepare", "SKIP", "not selected");
    }

    if selection.build {
        print_phase_line(args.json, "build", "RUN", "artifact build");
        let started = std::time::Instant::now();
        if args.artifact.is_some() {
            let skipped_reason = "--artifact provided".to_string();
            phase_mark_skipped(
                &mut phases[1],
                started.elapsed().as_millis() as u64,
                "build is skipped when --artifact is provided".to_string(),
                skipped_reason.clone(),
            );
            print_phase_line(args.json, "build", "SKIP", &skipped_reason);
        } else {
            let artifact_path = build_capsule_artifact_for_publish(&cwd)?;
            let elapsed = started.elapsed().as_millis() as u64;
            let message = format!("artifact built: {}", artifact_path.display());
            phase_mark_ok(&mut phases[1], elapsed, message.clone(), None);
            built_artifact_path = Some(artifact_path);
            print_phase_line(args.json, "build", "OK", &message);
        }
    } else {
        phase_mark_skipped(
            &mut phases[1],
            0,
            "build phase not selected".to_string(),
            "not selected".to_string(),
        );
        print_phase_line(args.json, "build", "SKIP", "not selected");
    }

    if selection.deploy {
        print_phase_line(args.json, "deploy", "RUN", "deploy execution");
        let started = std::time::Instant::now();
        if is_official {
            let outcome = run_official_deploy(resolved_target.registry_url.clone(), args.fix)?;

            if !args.json {
                println!(
                    "🔎 official publish route registry={} route={:?}",
                    outcome.route.registry_url, outcome.route.route
                );
                for stage in &outcome.diagnosis.stages {
                    let icon = if stage.ok { "✅" } else { "❌" };
                    println!("{} {:<14} {}", icon, stage.key, stage.message);
                }
                if outcome.fix_result.attempted {
                    if outcome.fix_result.applied {
                        let label = if outcome.fix_result.created {
                            "created"
                        } else {
                            "updated"
                        };
                        println!("🛠️  workflow {} via --fix", label);
                    } else {
                        println!("🛠️  --fix requested, but workflow was already up-to-date");
                    }
                }
            }

            if !outcome.diagnosis.can_handoff {
                let actions = publish_official::collect_issue_actions(&outcome.diagnosis.issues);
                let fix_line = actions.first().cloned().unwrap_or_else(|| {
                    "ato publish --deploy --registry https://api.ato.run".to_string()
                });
                phase_mark_failed(
                    &mut phases[2],
                    started.elapsed().as_millis() as u64,
                    "official publish diagnostics failed".to_string(),
                    Some(fix_line.clone()),
                );
                print_phase_line(args.json, "deploy", "FAIL", "official diagnostics failed");
                if !args.json {
                    println!("👉 次に打つコマンド: {}", fix_line);
                    if !actions.is_empty() {
                        println!();
                        println!("詳細:");
                        for issue in &outcome.diagnosis.issues {
                            println!(" - [{}] {}", issue.stage, issue.message);
                        }
                    }
                    anyhow::bail!("official publish diagnostics failed");
                }
                official_result = Some(outcome);
            } else {
                let success_message = "official CI handoff is ready".to_string();
                phase_mark_ok(
                    &mut phases[2],
                    started.elapsed().as_millis() as u64,
                    success_message.clone(),
                    Some("handoff".to_string()),
                );
                print_phase_line(args.json, "deploy", "OK", &success_message);
                official_result = Some(outcome);
            }
        } else {
            let source_is_artifact = args.artifact.is_some();
            let deploy_artifact = if let Some(path) = args.artifact.clone() {
                path
            } else if let Some(path) = built_artifact_path.clone() {
                path
            } else {
                let fix_line =
                    "ato publish --deploy --artifact <file.capsule> --registry <url> もしくは ato publish --build --deploy --registry <url>"
                        .to_string();
                phase_mark_failed(
                    &mut phases[2],
                    started.elapsed().as_millis() as u64,
                    "deploy phase requires artifact input".to_string(),
                    Some(fix_line.clone()),
                );
                print_phase_line(args.json, "deploy", "FAIL", "artifact input is missing");
                if !args.json {
                    println!("👉 次に打つコマンド: {}", fix_line);
                }
                anyhow::bail!(
                    "--deploy requires artifact input for private registry. Use --artifact or include --build."
                );
            };

            let preview = private_preview
                .as_ref()
                .context("missing private publish preview")?;
            if !args.json {
                println!(
                    "{}",
                    publish_private_start_summary_line(
                        resolved_target.mode,
                        &resolved_target.registry_url,
                        preview.source,
                        &preview.scoped_id,
                        &preview.version,
                        preview.allow_existing,
                    )
                );
            }

            let status = publish_private_status_message(resolved_target.mode, source_is_artifact);
            futures::executor::block_on(reporter.progress_start(status.to_string(), None))?;
            let scoped_override = if source_is_artifact {
                args.scoped_id.clone()
            } else {
                Some(preview.scoped_id.clone())
            };
            let upload_result = publish_private::execute(publish_private::PublishPrivateArgs {
                registry_url: resolved_target.registry_url.clone(),
                publisher_hint: resolved_target.publisher_handle.clone(),
                artifact_path: Some(deploy_artifact),
                force_large_payload: args.force_large_payload,
                scoped_id: scoped_override,
                allow_existing: args.allow_existing,
            });
            futures::executor::block_on(reporter.progress_finish(None))?;
            let result = upload_result?;

            let success_message = format!("uploaded {}", result.file_name);
            phase_mark_ok(
                &mut phases[2],
                started.elapsed().as_millis() as u64,
                success_message.clone(),
                Some("upload".to_string()),
            );
            print_phase_line(args.json, "deploy", "OK", &success_message);
            private_result = Some(result);
        }
    } else {
        phase_mark_skipped(
            &mut phases[2],
            0,
            "deploy phase not selected".to_string(),
            "not selected".to_string(),
        );
        print_phase_line(args.json, "deploy", "SKIP", "not selected");
    }

    if args.json {
        emit_publish_json_output(
            &resolved_target,
            &phases,
            private_result.as_ref(),
            official_result.as_ref(),
        )?;
    } else if let Some(result) = private_result.as_ref() {
        if resolved_target.mode.is_personal_dock() {
            println!("✅ Successfully published to Personal Dock!");
        } else {
            println!("✅ Successfully published to private registry!");
        }
        println!();
        println!("📦 Capsule:   {} v{}", result.scoped_id, result.version);
        println!("🛡️  Integrity: {}, {}", result.sha256, result.blake3);
        println!();
        println!("🌐 Registry: {}", result.registry_url);
        println!("🌐 Artifact URL: {}", result.artifact_url);
        println!();
        if result.already_existed {
            println!("ℹ️  Existing release reused (same sha256, no new upload).");
            println!();
        }
        if resolved_target.mode.is_personal_dock() {
            println!("👉 Next step: ato install {}", result.scoped_id);
        } else {
            println!(
                "👉 Next step: ato install {} --registry {}",
                result.scoped_id, result.registry_url
            );
        }
    } else if let Some(outcome) = official_result {
        println!();
        println!("✅ CI handoff ready. 次の順で実行してください:");
        for command in &outcome.diagnosis.next_commands {
            println!("  {}", command);
        }
        if let Some(repo) = &outcome.diagnosis.repository {
            println!(
                "  https://github.com/{}/actions/workflows/ato-publish.yml",
                repo
            );
        }
    } else {
        println!("✅ Selected publish phases completed.");
    }

    if !args.json {
        if selection.deploy && phases[2].ok {
            let notice = if is_official {
                "Official publish handoff prepared (CI-first: local upload is not executed)."
            } else if resolved_target.mode.is_personal_dock() {
                "Personal Dock publish completed."
            } else {
                "Private registry publish completed."
            };
            futures::executor::block_on(reporter.notify(notice.to_string()))?;
        } else {
            futures::executor::block_on(
                reporter.notify("Selected publish phases completed.".to_string()),
            )?;
        }
    }

    Ok(())
}

fn run_official_deploy(registry_url: String, fix: bool) -> Result<OfficialDeployOutcome> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let route = publish_official::build_route_plan(&registry_url);

    let mut fix_result = publish_official::WorkflowFixResult::default();
    let mut diagnosis = publish_official::diagnose_official(&cwd, &registry_url);
    if fix && diagnosis.needs_workflow_fix {
        fix_result = publish_official::apply_workflow_fix_once(&cwd)?;
        diagnosis = publish_official::diagnose_official(&cwd, &registry_url);
    }

    Ok(OfficialDeployOutcome {
        route,
        fix_result,
        diagnosis,
    })
}

fn publish_private_status_message(
    target_mode: PublishTargetMode,
    has_artifact: bool,
) -> &'static str {
    if target_mode.is_personal_dock() {
        if has_artifact {
            "📤 Publishing provided artifact to Personal Dock..."
        } else {
            "📦 Building capsule artifact for Personal Dock publish..."
        }
    } else if has_artifact {
        "📤 Publishing provided artifact to private registry..."
    } else {
        "📦 Building capsule artifact for private registry publish..."
    }
}

fn publish_private_start_summary_line(
    target_mode: PublishTargetMode,
    registry_url: &str,
    source: &str,
    scoped_id: &str,
    version: &str,
    allow_existing: bool,
) -> String {
    format!(
        "🔎 {} publish target registry={} source={} scoped_id={} version={} allow_existing={}",
        if target_mode.is_personal_dock() {
            "dock"
        } else {
            "private"
        },
        registry_url,
        source,
        scoped_id,
        version,
        allow_existing
    )
}

fn select_publish_phases(
    prepare: bool,
    build: bool,
    deploy: bool,
    is_official: bool,
    legacy_full_publish: bool,
) -> PublishPhaseSelection {
    let explicit_filter = prepare || build || deploy;
    if explicit_filter {
        PublishPhaseSelection {
            prepare,
            build,
            deploy,
            explicit_filter,
        }
    } else if is_official && !legacy_full_publish {
        PublishPhaseSelection {
            prepare: false,
            build: false,
            deploy: true,
            explicit_filter: false,
        }
    } else {
        PublishPhaseSelection {
            prepare: true,
            build: true,
            deploy: true,
            explicit_filter: false,
        }
    }
}

fn maybe_warn_legacy_full_publish(
    args: &PublishCommandArgs,
    selection: PublishPhaseSelection,
    is_official: bool,
) {
    if args.legacy_full_publish && is_official && !selection.explicit_filter {
        eprintln!(
            "⚠️  --legacy-full-publish is deprecated and will be removed in a future release. Use explicit --prepare/--build/--deploy flags instead."
        );
    }
}

fn validate_publish_phase_options(
    args: &PublishCommandArgs,
    selection: PublishPhaseSelection,
    is_official: bool,
) -> Result<()> {
    if args.fix && !(is_official && selection.deploy) {
        anyhow::bail!("--fix is only available when deploying to official registry");
    }

    if args.legacy_full_publish && !is_official {
        anyhow::bail!("--legacy-full-publish is only available for official registry publish");
    }

    if args.legacy_full_publish && selection.explicit_filter {
        anyhow::bail!("--legacy-full-publish cannot be combined with --prepare/--build/--deploy");
    }

    if args.allow_existing && (is_official || !selection.deploy) {
        anyhow::bail!("--allow-existing is only available for private registry deploy phase");
    }

    if !is_official && selection.deploy && !selection.build && args.artifact.is_none() {
        anyhow::bail!(
            "--deploy requires --artifact for private registry publish (or include --build)"
        );
    }

    Ok(())
}

fn build_capsule_artifact_for_publish(cwd: &std::path::Path) -> Result<PathBuf> {
    let manifest_path = cwd.join("capsule.toml");
    let manifest_raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&manifest_raw)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml: {}", err))?;
    crate::publish_ci::build_capsule_artifact(&manifest_path, &manifest.name, &manifest.version)
        .with_context(|| "Failed to build artifact for publish")
}

fn emit_publish_json_output(
    resolved_target: &ResolvedPublishTarget,
    phases: &[PublishPhaseResult],
    private_result: Option<&publish_private::PublishPrivateResult>,
    official_result: Option<&OfficialDeployOutcome>,
) -> Result<()> {
    if let Some(outcome) = official_result {
        let payload = serde_json::json!({
            "ok": outcome.diagnosis.can_handoff,
            "code": if outcome.diagnosis.can_handoff { "CI_HANDOFF_READY" } else { "CI_ONLY_PUBLISH" },
            "message": if outcome.diagnosis.can_handoff {
                "Official registry publishing is CI-first. Handoff is ready."
            } else {
                "Official registry publishing is CI-first. Run the suggested local fixes, then push tag to trigger CI."
            },
            "route": outcome.route.route,
            "registry": outcome.route.registry_url,
            "fix": outcome.fix_result,
            "diagnosis": outcome.diagnosis,
            "phases": phases,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if let Some(result) = private_result {
        let mut payload = serde_json::to_value(result)?;
        if let serde_json::Value::Object(map) = &mut payload {
            map.insert("phases".to_string(), serde_json::to_value(phases)?);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let payload = serde_json::json!({
        "ok": true,
        "code": "PUBLISH_PHASES_COMPLETED",
        "message": "Selected publish phases completed.",
        "registry": resolved_target.registry_url,
        "route": resolved_target.mode.route_label(),
        "phases": phases,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn new_phase_result(name: &'static str, selected: bool) -> PublishPhaseResult {
    PublishPhaseResult {
        name,
        selected,
        ok: !selected,
        status: "skipped",
        elapsed_ms: 0,
        actionable_fix: None,
        message: if selected {
            "pending".to_string()
        } else {
            "not selected".to_string()
        },
        result_kind: None,
        skipped_reason: if selected {
            None
        } else {
            Some("not selected".to_string())
        },
    }
}

fn phase_mark_ok(
    phase: &mut PublishPhaseResult,
    elapsed_ms: u64,
    message: String,
    result_kind: Option<String>,
) {
    phase.ok = true;
    phase.status = "ok";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = None;
    phase.message = message;
    phase.result_kind = result_kind;
    phase.skipped_reason = None;
}

fn phase_mark_skipped(
    phase: &mut PublishPhaseResult,
    elapsed_ms: u64,
    message: String,
    skipped_reason: String,
) {
    phase.ok = true;
    phase.status = "skipped";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = None;
    phase.message = message;
    phase.result_kind = None;
    phase.skipped_reason = Some(skipped_reason);
}

fn phase_mark_failed(
    phase: &mut PublishPhaseResult,
    elapsed_ms: u64,
    message: String,
    actionable_fix: Option<String>,
) {
    phase.ok = false;
    phase.status = "failed";
    phase.elapsed_ms = elapsed_ms;
    phase.actionable_fix = actionable_fix;
    phase.message = message;
    phase.result_kind = None;
    phase.skipped_reason = None;
}

fn print_phase_line(json_output: bool, phase: &str, state: &str, detail: &str) {
    if json_output {
        return;
    }
    println!("PHASE {:<7} {:<4} {}", phase, state, detail);
}

fn resolve_publish_target(cli_registry: Option<String>) -> Result<ResolvedPublishTarget> {
    let manifest_registry = discover_manifest_publish_registry()?;
    let publisher_handle = crate::auth::current_publisher_handle()?;

    resolve_publish_target_from_sources(
        cli_registry.as_deref(),
        manifest_registry.as_deref(),
        publisher_handle.as_deref(),
    )
}

fn discover_manifest_publish_registry() -> Result<Option<String>> {
    let cwd = std::env::current_dir().context("Failed to resolve current directory")?;
    let manifest_path = cwd.join("capsule.toml");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let parsed: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    Ok(parsed
        .get("store")
        .and_then(|v| v.get("registry"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned))
}

fn resolve_publish_target_from_sources(
    cli_registry: Option<&str>,
    manifest_registry: Option<&str>,
    publisher_handle: Option<&str>,
) -> Result<ResolvedPublishTarget> {
    if let Some(url) = cli_registry {
        return resolve_explicit_publish_target(url);
    }

    if let Some(url) = manifest_registry {
        return resolve_explicit_publish_target(url);
    }

    if let Some(handle) = publisher_handle {
        return Ok(ResolvedPublishTarget {
            registry_url: crate::auth::default_store_registry_url(),
            mode: PublishTargetMode::PersonalDockDirect,
            publisher_handle: Some(handle.to_string()),
        });
    }

    anyhow::bail!(
        "No default publish target found. Run `ato login` to publish to your Personal Dock, or pass `--registry https://api.ato.run` / `--ci` for the official Store."
    );
}

fn resolve_explicit_publish_target(raw: &str) -> Result<ResolvedPublishTarget> {
    let normalized = normalize_registry_url(raw)?;
    if is_legacy_dock_publish_registry(&normalized) {
        anyhow::bail!(
            "Registry URL `{}` is no longer supported. Personal Dock publish now uses `https://api.ato.run`; `/d/<handle>` is a UI page, not a registry.",
            normalized
        );
    }

    Ok(ResolvedPublishTarget {
        registry_url: normalized.clone(),
        mode: if is_official_publish_registry(&normalized) {
            PublishTargetMode::OfficialCi
        } else {
            PublishTargetMode::CustomDirect
        },
        publisher_handle: None,
    })
}

fn normalize_registry_url(raw: &str) -> Result<String> {
    crate::registry_http::normalize_registry_url(raw, "registry")
}

fn is_official_publish_registry(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("api.ato.run") || host.eq_ignore_ascii_case("staging.api.ato.run")
}

fn is_legacy_dock_publish_registry(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(mut segments) = parsed.path_segments() else {
        return false;
    };
    while let Some(segment) = segments.next() {
        if segment == "d" {
            return segments
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some();
        }
    }
    false
}

fn execute_setup_command(
    engine: String,
    version: Option<String>,
    skip_verify: bool,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let capsule_reporter: &dyn capsule_core::CapsuleReporter = reporter.as_ref();
    let install = engine_manager::install_engine_release(
        &engine,
        version.as_deref(),
        skip_verify,
        capsule_reporter,
    )?;

    futures::executor::block_on(reporter.notify(format!(
        "✅ Engine {} {} installed at {}",
        engine,
        install.version,
        install.path.display()
    )))?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute_run_like_command(
    path: PathBuf,
    target: Option<String>,
    watch: bool,
    background: bool,
    nacelle: Option<PathBuf>,
    registry: Option<String>,
    state: Vec<String>,
    inject: Vec<String>,
    enforcement: EnforcementMode,
    sandbox_mode: bool,
    unsafe_mode_legacy: bool,
    unsafe_bypass_sandbox_legacy: bool,
    dangerously_skip_permissions: bool,
    yes: bool,
    allow_unverified: bool,
    skill: Option<String>,
    from_skill: Option<PathBuf>,
    deprecation_warning: Option<&str>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    if let Some(warning) = deprecation_warning {
        eprintln!("{warning}");
    }

    let rt = tokio::runtime::Runtime::new()?;

    let resolved_skill_path = match (skill, from_skill) {
        (Some(skill_name), None) => Some(skill_resolver::resolve_skill_path(&skill_name)?),
        (None, Some(path)) => Some(path),
        (None, None) => None,
        (Some(_), Some(_)) => {
            anyhow::bail!("--skill and --from-skill are mutually exclusive");
        }
    };

    if let Some(skill_path) = resolved_skill_path {
        if watch {
            anyhow::bail!("--skill/--from-skill does not support --watch in MVP mode");
        }
        if background {
            anyhow::bail!("--skill/--from-skill does not support --background in MVP mode");
        }

        let generated = skill::materialize_skill_capsule(&skill_path)?;
        debug!(
            manifest_path = %generated.manifest_path().display(),
            "Translated SKILL.md to capsule"
        );

        let sandbox_requested = sandbox_mode || unsafe_mode_legacy || unsafe_bypass_sandbox_legacy;
        let effective_enforcement = enforce_sandbox_mode_flags(
            enforcement,
            sandbox_requested,
            dangerously_skip_permissions,
            reporter.clone(),
        )?;
        return execute_open_command(
            generated.manifest_path().to_path_buf(),
            target,
            watch,
            background,
            nacelle,
            effective_enforcement,
            sandbox_requested,
            dangerously_skip_permissions,
            yes,
            state,
            inject,
            reporter,
        );
    }

    let path = rt.block_on(resolve_run_target_or_install(
        path,
        yes,
        allow_unverified,
        registry.as_deref(),
        reporter.clone(),
    ))?;

    let sandbox_requested = sandbox_mode || unsafe_mode_legacy || unsafe_bypass_sandbox_legacy;
    let effective_enforcement = enforce_sandbox_mode_flags(
        enforcement,
        sandbox_requested,
        dangerously_skip_permissions,
        reporter.clone(),
    )?;
    execute_open_command(
        path,
        target,
        watch,
        background,
        nacelle,
        effective_enforcement,
        sandbox_requested,
        dangerously_skip_permissions,
        yes,
        state,
        inject,
        reporter,
    )
}

async fn resolve_run_target_or_install(
    path: PathBuf,
    yes: bool,
    allow_unverified: bool,
    registry: Option<&str>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<PathBuf> {
    let raw = path.to_string_lossy().to_string();
    let expanded_local = local_input::expand_local_path(&raw);
    if local_input::should_treat_input_as_local(&raw, &expanded_local) {
        return Ok(expanded_local);
    }

    if let Some(repository) = install::parse_github_run_ref(&raw)? {
        let json_mode = matches!(reporter.as_ref(), reporters::CliReporter::Json(_));
        if json_mode && !yes {
            anyhow::bail!(
                "Non-interactive JSON mode requires -y/--yes when auto-installing missing capsules"
            );
        }

        if !yes
            && !can_prompt_interactively(
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
            )
        {
            anyhow::bail!(
                "Interactive install confirmation requires a TTY. Re-run with -y/--yes in CI or non-interactive environments."
            );
        }

        if !yes {
            let approved = prompt_github_run_confirmation(&repository)?;
            if !approved {
                anyhow::bail!("Installation cancelled by user");
            }
        } else {
            debug!(
                repository = %repository,
                "GitHub repository not installed locally; continuing with -y auto-install"
            );
        }

        let install_result = install_github_repository(
            &repository,
            None,
            yes,
            install::ProjectionPreference::Skip,
            json_mode,
            !json_mode
                && can_prompt_interactively(
                    std::io::stdin().is_terminal(),
                    std::io::stderr().is_terminal(),
                ),
        )
        .await?;
        return Ok(install_result.path);
    }

    let scoped_ref = match install::parse_capsule_ref(&raw) {
        Ok(value) => value,
        Err(error) => {
            if install::is_slug_only_ref(&raw) {
                let effective_registry = registry.unwrap_or(DEFAULT_RUN_REGISTRY_URL);
                let suggestions =
                    install::suggest_scoped_capsules(&raw, Some(effective_registry), 5).await?;
                if suggestions.is_empty() {
                    anyhow::bail!(
                        "scoped_id_required: '{}' is not valid for `ato run`. Use publisher/slug (for example: koh0920/{}).",
                        raw,
                        raw.trim()
                    );
                }

                let mut message = format!(
                    "scoped_id_required: '{}' is ambiguous. Specify publisher/slug.\n\nDid you mean one of these?",
                    raw
                );
                for suggestion in suggestions {
                    message.push_str(&format!(
                        "\n  - {}  ({} downloads)",
                        suggestion.scoped_id, suggestion.downloads
                    ));
                }
                message.push_str("\n\nRun `ato search ");
                message.push_str(raw.trim());
                message.push_str("` to see more options.");
                anyhow::bail!(message);
            }
            return Err(error).context(
                "Invalid run target. Use a local path or existing .capsule file, or publisher/slug for store capsules.",
            );
        }
    };

    let installed_capsule = resolve_installed_capsule_archive(&scoped_ref, registry, None).await?;
    let mut registry_detail = None;
    let mut registry_installable_version = None;

    if let Some(explicit_registry) = registry {
        match install::fetch_capsule_detail(&scoped_ref.scoped_id, Some(explicit_registry)).await {
            Ok(detail) => {
                registry_installable_version = detail
                    .latest_version
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);

                if let Some(version) = registry_installable_version.as_deref() {
                    if let Some(installed_capsule) =
                        resolve_installed_capsule_archive(&scoped_ref, registry, Some(version))
                            .await?
                    {
                        debug!(
                            capsule = %installed_capsule.display(),
                            version = version,
                            "Using installed capsule matching registry current version"
                        );
                        return Ok(installed_capsule);
                    }
                }

                registry_detail = Some(detail);
            }
            Err(error) => {
                if let Some(installed_capsule) = installed_capsule {
                    debug!(
                        capsule = %installed_capsule.display(),
                        error = %error,
                        "Falling back to installed capsule after registry detail lookup failed"
                    );
                    return Ok(installed_capsule);
                }
                return Err(error);
            }
        }
    } else if let Some(installed_capsule) = installed_capsule {
        debug!(
            capsule = %installed_capsule.display(),
            "Using installed capsule"
        );
        return Ok(installed_capsule);
    }

    let json_mode = matches!(reporter.as_ref(), reporters::CliReporter::Json(_));
    if json_mode && !yes {
        anyhow::bail!(
            "Non-interactive JSON mode requires -y/--yes when auto-installing missing capsules"
        );
    }

    if !yes
        && !can_prompt_interactively(
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
        )
    {
        anyhow::bail!(
            "Interactive install confirmation requires a TTY. Re-run with -y/--yes in CI or non-interactive environments."
        );
    }

    let effective_registry = registry.unwrap_or(DEFAULT_RUN_REGISTRY_URL);
    let detail = if let Some(detail) = registry_detail {
        detail
    } else {
        install::fetch_capsule_detail(&scoped_ref.scoped_id, Some(effective_registry)).await?
    };
    let installable_version = if let Some(version) = registry_installable_version {
        version
    } else {
        detail
            .latest_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot auto-install '{}': no installable version is published.",
                    detail.scoped_id
                )
            })?
            .to_string()
    };

    if !yes {
        let approved = prompt_install_confirmation(&detail, &installable_version)?;
        if !approved {
            anyhow::bail!("Installation cancelled by user");
        }
    } else {
        debug!(
            scoped_id = %detail.scoped_id,
            "Capsule not installed; continuing with -y auto-install"
        );
    }

    let install_result = install::install_app(
        &scoped_ref.scoped_id,
        Some(effective_registry),
        Some(installable_version.as_str()),
        None,
        false,
        yes,
        install::ProjectionPreference::Skip,
        allow_unverified,
        false,
        json_mode,
        !json_mode
            && can_prompt_interactively(
                std::io::stdin().is_terminal(),
                std::io::stderr().is_terminal(),
            ),
    )
    .await?;
    Ok(install_result.path)
}

async fn resolve_installed_capsule_archive(
    scoped_ref: &install::ScopedCapsuleRef,
    registry: Option<&str>,
    preferred_version: Option<&str>,
) -> Result<Option<PathBuf>> {
    let store_root = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ato")
        .join("store");
    if let Some(path) = resolve_installed_capsule_archive_in_store(
        &store_root.join(&scoped_ref.publisher),
        &scoped_ref.slug,
        preferred_version,
    )? {
        return Ok(Some(path));
    }

    let legacy_slug_dir = store_root.join(&scoped_ref.slug);
    if !legacy_slug_dir.exists() || !legacy_slug_dir.is_dir() {
        return Ok(None);
    }

    let scoped_slug_dir = store_root
        .join(&scoped_ref.publisher)
        .join(&scoped_ref.slug);
    if scoped_slug_dir.exists() {
        return resolve_installed_capsule_archive_in_store(
            &store_root.join(&scoped_ref.publisher),
            &scoped_ref.slug,
            preferred_version,
        );
    }

    let effective_registry = registry.unwrap_or(DEFAULT_RUN_REGISTRY_URL);
    let suggestions =
        install::suggest_scoped_capsules(&scoped_ref.slug, Some(effective_registry), 10).await?;
    let scoped_matches: Vec<_> = suggestions
        .iter()
        .filter(|candidate| {
            candidate
                .scoped_id
                .ends_with(&format!("/{}", scoped_ref.slug))
        })
        .collect();
    let unique_match =
        scoped_matches.len() == 1 && scoped_matches[0].scoped_id == scoped_ref.scoped_id;

    if !unique_match {
        anyhow::bail!(
            "Legacy installation found at {} but publisher could not be determined safely. Please reinstall using: ato install {}",
            legacy_slug_dir.display(),
            scoped_ref.scoped_id
        );
    }

    if let Some(parent) = scoped_slug_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create scoped store directory: {}",
                parent.display()
            )
        })?;
    }
    std::fs::rename(&legacy_slug_dir, &scoped_slug_dir).with_context(|| {
        format!(
            "Failed to migrate legacy store path {} -> {}",
            legacy_slug_dir.display(),
            scoped_slug_dir.display()
        )
    })?;

    resolve_installed_capsule_archive_in_store(
        &store_root.join(&scoped_ref.publisher),
        &scoped_ref.slug,
        preferred_version,
    )
}

async fn install_github_repository(
    repository: &str,
    output_dir: Option<PathBuf>,
    yes: bool,
    projection_preference: install::ProjectionPreference,
    json: bool,
    can_prompt: bool,
) -> Result<install::InstallResult> {
    const MAX_GITHUB_DRAFT_RETRIES: u8 = 3;

    let install_draft = match install::fetch_github_install_draft(repository).await {
        Ok(draft) => Some(draft),
        Err(error) => {
            if !json {
                eprintln!(
                    "⚠️  Failed to fetch ato store install draft: {error}. Falling back to local zero-config inference."
                );
            }
            None
        }
    };
    let checkout = install::download_github_repository_at_ref(
        repository,
        install_draft
            .as_ref()
            .map(|draft| draft.resolved_ref.sha.as_str()),
    )
    .await?;
    let install_draft = install_draft
        .as_ref()
        .map(|draft| draft.normalize_preview_toml_for_checkout(&checkout.checkout_dir))
        .transpose()?;
    let injected_manifest = install_draft
        .as_ref()
        .and_then(|draft| draft.preview_toml.clone());
    let inference_attempt = if let Some(draft) = install_draft.as_ref() {
        inference_feedback::submit_attempt(repository, draft)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    if !json {
        eprintln!(
            "📦 Building {} from GitHub source in {}",
            checkout.repository,
            checkout.checkout_dir.display()
        );
        if let Some(draft) = install_draft.as_ref() {
            eprintln!(
                "   Revision: {} ({})",
                draft.resolved_ref.sha, draft.resolved_ref.ref_name
            );
            if draft.manifest_source == "inferred" {
                eprintln!(
                    "   Using store-generated capsule draft for {}",
                    draft.repo_ref
                );
                if let Some(hint) = draft.capsule_hint.as_ref() {
                    eprintln!("   Confidence: {}", hint.confidence);
                    for warning in &hint.warnings {
                        eprintln!("   Warning: {warning}");
                    }
                }
            }
        }
    }
    let mut latest_install_draft = install_draft.clone();
    let build_result = match build_github_repository_checkout(
        checkout.checkout_dir.clone(),
        json,
        injected_manifest.clone(),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let mut last_error = error;
            let smoke_report = last_error
                .downcast_ref::<commands::build::InferredManifestSmokeFailure>()
                .map(|failure| failure.report.clone());

            if let (Some(attempt), Some(report)) =
                (inference_attempt.as_ref(), smoke_report.as_ref())
            {
                let _ = inference_feedback::submit_smoke_failed(attempt, report).await;
            }

            if let (Some(draft), Some(report)) = (install_draft.as_ref(), smoke_report.as_ref()) {
                if !json {
                    eprintln!("Failed to run with inferred capsule.toml.");
                    eprintln!("Reason: {}", report.message);
                }
                if draft_requires_manual_review(draft) {
                    return Err(build_github_manual_intervention_error(
                        &checkout.checkout_dir,
                        repository,
                        draft,
                        inference_attempt.as_ref(),
                        &report.message,
                    )?);
                }

                let mut recovered_build = None;
                let mut current_draft = draft.clone();
                let mut current_report = report.clone();
                for retry_ordinal in 1..=MAX_GITHUB_DRAFT_RETRIES {
                    let previous_toml = current_draft.preview_toml.clone().unwrap_or_default();
                    if previous_toml.trim().is_empty() {
                        break;
                    }

                    let next_draft = match inference_feedback::request_retry_install_draft(
                        repository,
                        &current_draft,
                        inference_attempt.as_ref(),
                        &current_report,
                        retry_ordinal,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(retry_error) => {
                            if !json {
                                eprintln!(
                                    "⚠️  Failed to request retry draft ({retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES}): {retry_error}"
                                );
                            }
                            break;
                        }
                    };
                    let next_draft =
                        next_draft.normalize_preview_toml_for_checkout(&checkout.checkout_dir)?;
                    let next_toml = next_draft.preview_toml.clone().unwrap_or_default();
                    let draft_changed = next_toml.trim() != previous_toml.trim();
                    latest_install_draft = Some(next_draft.clone());
                    current_draft = next_draft;

                    if !draft_changed {
                        if !json {
                            eprintln!(
                                "ℹ️  Retry draft {retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES} did not change the generated capsule.toml."
                            );
                        }
                        break;
                    }
                    if !current_draft.retryable {
                        if !json {
                            eprintln!(
                                "ℹ️  Retry draft {retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES} requested manual review instead of another automatic retry."
                            );
                        }
                        break;
                    }

                    if !json {
                        eprintln!(
                            "🔁 Retrying build with failure-aware draft ({retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES})..."
                        );
                        if let Some(hint) = current_draft.capsule_hint.as_ref() {
                            eprintln!("   Confidence: {}", hint.confidence);
                            if let Some(launchability) = hint.launchability.as_deref() {
                                eprintln!("   Launchability: {}", launchability);
                            }
                            for warning in &hint.warnings {
                                eprintln!("   Warning: {warning}");
                            }
                        }
                    }

                    match build_github_repository_checkout(
                        checkout.checkout_dir.clone(),
                        json,
                        current_draft.preview_toml.clone(),
                    )
                    .await
                    {
                        Ok(result) => {
                            recovered_build = Some(result);
                            break;
                        }
                        Err(retry_error) => {
                            let retry_smoke_report = retry_error
                                .downcast_ref::<commands::build::InferredManifestSmokeFailure>()
                                .map(|failure| failure.report.clone());
                            last_error = retry_error;
                            let Some(retry_report) = retry_smoke_report else {
                                break;
                            };
                            current_report = retry_report.clone();
                            if let Some(attempt) = inference_attempt.as_ref() {
                                let _ =
                                    inference_feedback::submit_smoke_failed(attempt, &retry_report)
                                        .await;
                            }
                        }
                    }
                }

                if let Some(result) = recovered_build {
                    result
                } else if draft_requires_manual_review(
                    latest_install_draft.as_ref().unwrap_or(draft),
                ) {
                    return Err(build_github_manual_intervention_error(
                        &checkout.checkout_dir,
                        repository,
                        latest_install_draft.as_ref().unwrap_or(draft),
                        inference_attempt.as_ref(),
                        &current_report.message,
                    )?);
                } else if can_prompt {
                    let draft_for_manual_fix = latest_install_draft.as_ref().unwrap_or(draft);
                    if let Some(recovered) = retry_github_build_after_manual_fix(
                        &checkout.checkout_dir,
                        repository,
                        draft_for_manual_fix,
                        inference_attempt.as_ref(),
                        json,
                    )
                    .await?
                    {
                        recovered
                    } else {
                        return Err(last_error);
                    }
                } else {
                    return Err(last_error);
                }
            } else {
                return Err(last_error);
            }
        }
    };
    let artifact = build_result.artifact.ok_or_else(|| {
        anyhow::anyhow!("GitHub repository did not produce an installable .capsule artifact")
    })?;
    install::install_built_github_artifact(
        &artifact,
        &checkout.publisher,
        &checkout.repository,
        install::InstallExecutionOptions {
            output_dir,
            yes,
            projection_preference,
            json_output: json,
            can_prompt_interactively: can_prompt,
        },
    )
    .await
}

async fn run_blocking_github_install_step<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("GitHub repository build task failed")?
}

async fn build_github_repository_checkout(
    checkout_dir: PathBuf,
    json: bool,
    injected_manifest: Option<String>,
) -> Result<commands::build::BuildResult> {
    run_blocking_github_install_step(move || {
        let reporter = std::sync::Arc::new(reporters::CliReporter::new(json));
        commands::build::execute_pack_command_with_injected_manifest(
            checkout_dir,
            false,
            None,
            false,
            false,
            false,
            false,
            EnforcementMode::Strict.as_str().to_string(),
            reporter,
            false,
            json,
            None,
            injected_manifest.as_deref(),
        )
    })
    .await
}

async fn retry_github_build_after_manual_fix(
    checkout_dir: &std::path::Path,
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    inference_attempt: Option<&inference_feedback::InferenceAttemptHandle>,
    json: bool,
) -> Result<Option<commands::build::BuildResult>> {
    let should_edit =
        inference_feedback::prompt_yes_no("Edit generated capsule.toml and retry? [Y/n]: ", true)?;
    if !should_edit {
        return Ok(None);
    }

    let attempt_label = inference_attempt
        .map(|attempt| attempt.attempt_id.as_str())
        .unwrap_or("manual");
    let manifest_path = inference_feedback::build_manual_manifest_path(checkout_dir, attempt_label);
    let inferred_manifest = install_draft
        .preview_toml
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("store draft previewToml missing for manual fix"))?;
    inference_feedback::write_manual_manifest(&manifest_path, inferred_manifest)?;

    eprintln!("Open editor for {}", manifest_path.display());
    if !inference_feedback::has_configured_editor() {
        return Err(anyhow::anyhow!(build_github_manual_intervention_message(
            repository,
            install_draft,
            &manifest_path,
            "VISUAL or EDITOR is not set for manual fix mode",
        )));
    }
    inference_feedback::open_editor(&manifest_path)?;
    let edited_manifest = inference_feedback::read_manual_manifest(&manifest_path)?;
    if edited_manifest.trim().is_empty() {
        anyhow::bail!("edited capsule.toml is empty");
    }

    let retry_result = build_github_repository_checkout(
        checkout_dir.to_path_buf(),
        json,
        Some(edited_manifest.clone()),
    )
    .await?;

    eprintln!(
        "{}",
        inference_feedback::summarize_manifest_diff(inferred_manifest, &edited_manifest)
    );
    if let Some(attempt) = inference_attempt {
        let should_share = inference_feedback::prompt_yes_no(
            "The generated capsule.toml was fixed and smoke test passed. Share this corrected configuration to improve ato for public GitHub repositories? [Y/n]: ",
            true,
        )?;
        if should_share {
            let _ = inference_feedback::submit_verified_fix(attempt, &edited_manifest).await;
        }
    }

    Ok(Some(retry_result))
}

fn draft_requires_manual_review(draft: &install::GitHubInstallDraftResponse) -> bool {
    if draft
        .capsule_hint
        .as_ref()
        .and_then(|hint| hint.launchability.as_deref())
        == Some("manual_review")
    {
        return true;
    }
    if draft.retryable {
        return false;
    }

    let has_required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .map(|values| !values.is_empty())
        .unwrap_or(false);
    let has_manual_review_warning = draft
        .capsule_hint
        .as_ref()
        .map(|hint| {
            hint.warnings.iter().any(|warning| {
                let lowered = warning.to_ascii_lowercase();
                lowered.contains("manual")
                    || lowered.contains("database")
                    || lowered.contains("redis")
                    || lowered.contains(".env")
                    || lowered.contains("credential")
                    || lowered.contains("secret")
                    || lowered.contains("token")
                    || warning.contains("環境変数")
                    || warning.contains("手動")
                    || warning.contains("外部")
            })
        })
        .unwrap_or(false);

    has_required_env || has_manual_review_warning
}

fn build_github_manual_intervention_error(
    checkout_dir: &std::path::Path,
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    inference_attempt: Option<&inference_feedback::InferenceAttemptHandle>,
    failure_reason: &str,
) -> Result<anyhow::Error> {
    let attempt_label = inference_attempt
        .map(|attempt| attempt.attempt_id.as_str())
        .unwrap_or("manual");
    let manifest_path = inference_feedback::build_manual_manifest_path(checkout_dir, attempt_label);
    if let Some(preview_toml) = install_draft.preview_toml.as_deref() {
        inference_feedback::write_manual_manifest(&manifest_path, preview_toml)?;
    }
    Ok(anyhow::anyhow!(build_github_manual_intervention_message(
        repository,
        install_draft,
        &manifest_path,
        failure_reason,
    )))
}

fn build_github_manual_intervention_message(
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    manifest_path: &std::path::Path,
    failure_reason: &str,
) -> String {
    let mut next_steps = Vec::new();
    let required_env = install_draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .unwrap_or_default();
    if !required_env.is_empty() {
        next_steps.push(format!(
            "Set the required environment variables before rerunning: {}.",
            required_env.join(", ")
        ));
    }
    if let Some(hint) = install_draft.capsule_hint.as_ref() {
        for warning in hint.warnings.iter().take(2) {
            next_steps.push(warning.clone());
        }
    }
    next_steps.push(format!(
        "Review {} and adjust the generated command or target settings as needed.",
        manifest_path.display()
    ));
    if !inference_feedback::has_configured_editor() {
        next_steps.push(
            "Set VISUAL or EDITOR if you want ato to open the file automatically.".to_string(),
        );
    }
    next_steps.push(format!(
        "Rerun `ato run {repository}` after the prerequisites are ready."
    ));
    inference_feedback::build_manual_intervention_message(
        manifest_path,
        failure_reason,
        &next_steps,
    )
}

fn required_env_from_preview_toml(manifest_text: &str) -> Vec<String> {
    let Ok(parsed) = toml::from_str::<toml::Value>(manifest_text) else {
        return Vec::new();
    };
    parsed
        .get("env")
        .and_then(|env| env.get("required"))
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_installed_capsule_archive_in_store(
    store_root: &std::path::Path,
    slug: &str,
    preferred_version: Option<&str>,
) -> Result<Option<PathBuf>> {
    let slug_dir = store_root.join(slug);
    if !slug_dir.exists() || !slug_dir.is_dir() {
        return Ok(None);
    }

    if let Some(version) = preferred_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let version_dir = slug_dir.join(version);
        if !version_dir.exists() || !version_dir.is_dir() {
            return Ok(None);
        }
        return select_capsule_file_in_version(&version_dir);
    }

    let mut version_dirs: Vec<(ParsedSemver, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&slug_dir)
        .with_context(|| format!("Failed to read store directory: {}", slug_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(version_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(parsed) = ParsedSemver::parse(version_name) {
            version_dirs.push((parsed, path));
        }
    }

    version_dirs.sort_by(|(a, _), (b, _)| compare_semver(a, b).reverse());

    for (_, version_dir) in version_dirs {
        if let Some(capsule_path) = select_capsule_file_in_version(&version_dir)? {
            return Ok(Some(capsule_path));
        }
    }

    Ok(None)
}

fn select_capsule_file_in_version(version_dir: &std::path::Path) -> Result<Option<PathBuf>> {
    let mut capsules = Vec::new();
    for entry in std::fs::read_dir(version_dir).with_context(|| {
        format!(
            "Failed to read version directory: {}",
            version_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("capsule"))
        {
            capsules.push(path);
        }
    }

    capsules.sort();
    Ok(capsules.into_iter().next())
}

fn prompt_install_confirmation(
    detail: &install::CapsuleDetailSummary,
    resolved_version: &str,
) -> Result<bool> {
    println!();
    println!("[!] Capsule '{}' is not installed.", detail.scoped_id);
    println!();
    let name = if detail.name.trim().is_empty() {
        detail.slug.as_str()
    } else {
        detail.name.trim()
    };
    println!("📦 {} (v{})", name, resolved_version);
    if !detail.description.trim().is_empty() {
        println!("{}", detail.description.trim());
    }

    print_permission_summary(detail.permissions.as_ref());
    println!();

    loop {
        print!("? Do you want to install and run this capsule? (Y/n): ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("Failed to read user input")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                println!("Please answer 'y' or 'n'.");
            }
        }
    }
}

fn prompt_github_run_confirmation(repository: &str) -> Result<bool> {
    println!();
    println!(
        "[!] GitHub repository 'github.com/{}' is not installed.",
        repository
    );
    println!();
    println!("ato will download, build, install, and run this repository.");
    println!();

    loop {
        print!("? Do you want to install and run this repository? (Y/n): ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("Failed to read user input")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => {
                println!("Please answer 'y' or 'n'.");
            }
        }
    }
}

fn print_permission_summary(permissions: Option<&install::CapsulePermissions>) {
    println!("This capsule requests the following permissions:");
    let Some(permissions) = permissions else {
        println!("  - No permissions metadata declared");
        return;
    };

    let mut printed_any = false;

    if let Some(network) = permissions.network.as_ref() {
        let endpoints = network.merged_endpoints();
        if !endpoints.is_empty() {
            printed_any = true;
            println!("  🌐 Network:");
            for endpoint in endpoints {
                println!("    - {}", endpoint);
            }
        }
    }

    if let Some(isolation) = permissions.isolation.as_ref() {
        if !isolation.allow_env.is_empty() {
            printed_any = true;
            println!("  🔑 Isolation env allowlist:");
            for env in &isolation.allow_env {
                println!("    - {}", env);
            }
        }
    }

    if let Some(filesystem) = permissions.filesystem.as_ref() {
        if !filesystem.read_only.is_empty() {
            printed_any = true;
            println!("  📁 Filesystem read-only:");
            for path in &filesystem.read_only {
                println!("    - {}", path);
            }
        }
        if !filesystem.read_write.is_empty() {
            printed_any = true;
            println!("  ✍️  Filesystem read-write:");
            for path in &filesystem.read_write {
                println!("    - {}", path);
            }
        }
    }

    if !printed_any {
        println!("  - No permissions metadata declared");
    }
}

fn can_prompt_interactively(stdin_is_tty: bool, stdout_is_tty: bool) -> bool {
    tui::can_launch_tui(stdin_is_tty, stdout_is_tty)
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ParsedSemver {
    major: u64,
    minor: u64,
    patch: u64,
    pre_release: Option<String>,
}

impl ParsedSemver {
    fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let without_build = trimmed.split('+').next()?;
        let (core, pre_release) = if let Some((core, pre)) = without_build.split_once('-') {
            (core, Some(pre.to_string()))
        } else {
            (without_build, None)
        };

        let mut parts = core.split('.');
        let major = parts.next()?.parse::<u64>().ok()?;
        let minor = parts.next()?.parse::<u64>().ok()?;
        let patch = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None;
        }

        Some(Self {
            major,
            minor,
            patch,
            pre_release,
        })
    }
}

fn compare_semver(a: &ParsedSemver, b: &ParsedSemver) -> Ordering {
    a.major
        .cmp(&b.major)
        .then_with(|| a.minor.cmp(&b.minor))
        .then_with(|| a.patch.cmp(&b.patch))
        .then_with(|| match (&a.pre_release, &b.pre_release) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a_pre), Some(b_pre)) => a_pre.cmp(b_pre),
        })
}

fn enforce_sandbox_mode_flags(
    enforcement: EnforcementMode,
    sandbox_requested: bool,
    dangerously_skip_permissions: bool,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<EnforcementMode> {
    const ENV_ALLOW_UNSAFE: &str = "CAPSULE_ALLOW_UNSAFE";

    if matches!(enforcement, EnforcementMode::BestEffort) {
        anyhow::bail!("--enforcement best-effort is no longer supported; use --enforcement strict");
    }

    if matches!(enforcement, EnforcementMode::Strict) && sandbox_requested {
        futures::executor::block_on(
            reporter.warn(
                "⚠️  Sandbox mode enabled: Tier2 targets will run under strict native sandboxing"
                    .to_string(),
            ),
        )?;
    }

    if dangerously_skip_permissions {
        if std::env::var(ENV_ALLOW_UNSAFE).ok().as_deref() != Some("1") {
            anyhow::bail!(
                "--dangerously-skip-permissions requires {}=1",
                ENV_ALLOW_UNSAFE
            );
        }
        futures::executor::block_on(
            reporter.warn(
                "⚠️  Dangerous mode enabled: bypassing all Ato runtime permission and sandbox barriers"
                    .to_string(),
            ),
        )?;
    }

    Ok(enforcement)
}

#[allow(clippy::too_many_arguments)]
fn execute_open_command(
    path: PathBuf,
    target: Option<String>,
    watch: bool,
    background: bool,
    nacelle: Option<PathBuf>,
    enforcement: EnforcementMode,
    sandbox_mode: bool,
    dangerously_skip_permissions: bool,
    assume_yes: bool,
    state: Vec<String>,
    inject: Vec<String>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let target_path = if path.is_file() || path.extension().is_some_and(|ext| ext == "capsule") {
        path.clone()
    } else {
        path.join("capsule.toml")
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(commands::open::execute(commands::open::OpenArgs {
        target: target_path,
        target_label: target,
        watch,
        background,
        nacelle,
        enforcement: enforcement.as_str().to_string(),
        sandbox_mode,
        dangerously_skip_permissions,
        assume_yes,
        state_bindings: state,
        inject_bindings: inject,
        reporter,
    }))
}

fn execute_source_sync_status_command(
    source_id: String,
    sync_run_id: String,
    registry: Option<String>,
    json: bool,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result =
            source::fetch_sync_run_status(&source_id, &sync_run_id, registry.as_deref(), json)
                .await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Ok(())
    })
}

fn execute_source_rebuild_command(
    source_id: String,
    reference: Option<String>,
    wait: bool,
    registry: Option<String>,
    json: bool,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = source::rebuild_source(
            &source_id,
            reference.as_deref(),
            wait,
            registry.as_deref(),
            json,
        )
        .await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn execute_search_command(
    query: Option<String>,
    category: Option<String>,
    tags: Vec<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    registry: Option<String>,
    json: bool,
    no_tui: bool,
    show_manifest: bool,
) -> Result<()> {
    if should_use_search_tui(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        json,
        no_tui,
    ) {
        let selected = tui::run_search_tui(tui::SearchTuiArgs {
            query: query.clone(),
            category: category.clone(),
            tags: tags.clone(),
            limit,
            cursor: cursor.clone(),
            registry: registry.clone(),
            show_manifest,
        })?;
        if let Some(scoped_id) = selected {
            println!("{}", scoped_id);
        }
        return Ok(());
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let result = search::search_capsules(
            query.as_deref(),
            category.as_deref(),
            Some(tags.as_slice()),
            limit,
            cursor.as_deref(),
            registry.as_deref(),
            json,
        )
        .await?;

        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Ok(())
    })
}

fn should_use_search_tui(
    stdin_is_tty: bool,
    stdout_is_tty: bool,
    json: bool,
    no_tui: bool,
) -> bool {
    tui::can_launch_tui(stdin_is_tty, stdout_is_tty) && !json && !no_tui
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn semver_prefers_highest_stable_release() {
        let stable = ParsedSemver::parse("1.2.0").unwrap();
        let prerelease = ParsedSemver::parse("1.2.0-rc1").unwrap();
        let older = ParsedSemver::parse("1.1.9").unwrap();

        assert_eq!(compare_semver(&stable, &prerelease), Ordering::Greater);
        assert_eq!(compare_semver(&stable, &older), Ordering::Greater);
        assert_eq!(compare_semver(&prerelease, &older), Ordering::Greater);
    }

    #[test]
    fn select_capsule_file_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let version_dir = tmp.path().join("1.0.0");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(version_dir.join("zeta.capsule"), b"z").unwrap();
        std::fs::write(version_dir.join("alpha.capsule"), b"a").unwrap();

        let selected = select_capsule_file_in_version(&version_dir)
            .unwrap()
            .unwrap();
        assert_eq!(
            selected.file_name().and_then(|name| name.to_str()),
            Some("alpha.capsule")
        );
    }

    #[test]
    fn resolve_installed_capsule_uses_highest_version() {
        let tmp = tempfile::tempdir().unwrap();
        let slug = "demo-app";
        let slug_dir = tmp.path().join(slug);
        std::fs::create_dir_all(slug_dir.join("0.9.0")).unwrap();
        std::fs::create_dir_all(slug_dir.join("1.2.0-rc1")).unwrap();
        std::fs::create_dir_all(slug_dir.join("1.2.0")).unwrap();

        std::fs::write(slug_dir.join("0.9.0/old.capsule"), b"old").unwrap();
        std::fs::write(slug_dir.join("1.2.0-rc1/preview.capsule"), b"preview").unwrap();
        std::fs::write(slug_dir.join("1.2.0/new.capsule"), b"new").unwrap();

        let resolved = resolve_installed_capsule_archive_in_store(tmp.path(), slug, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some("new.capsule")
        );
    }

    #[test]
    fn resolve_installed_capsule_can_target_exact_version() {
        let tmp = tempfile::tempdir().unwrap();
        let slug = "demo-app";
        let slug_dir = tmp.path().join(slug);
        std::fs::create_dir_all(slug_dir.join("1.0.0")).unwrap();
        std::fs::create_dir_all(slug_dir.join("2.0.0")).unwrap();

        std::fs::write(slug_dir.join("1.0.0/rolled-back.capsule"), b"old").unwrap();
        std::fs::write(slug_dir.join("2.0.0/current.capsule"), b"new").unwrap();

        let resolved = resolve_installed_capsule_archive_in_store(tmp.path(), slug, Some("1.0.0"))
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.file_name().and_then(|name| name.to_str()),
            Some("rolled-back.capsule")
        );
    }

    #[test]
    fn tty_prompt_gate_requires_both_streams() {
        assert!(can_prompt_interactively(true, true));
        assert!(!can_prompt_interactively(true, false));
        assert!(!can_prompt_interactively(false, true));
        assert!(!can_prompt_interactively(false, false));
    }

    #[test]
    fn resolve_run_target_rejects_noncanonical_github_url_input() {
        let reporter = std::sync::Arc::new(reporters::CliReporter::new(false));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(resolve_run_target_or_install(
                PathBuf::from("https://github.com/Koh0920/demo-repo"),
                true,
                false,
                None,
                reporter,
            ))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("ato run github.com/Koh0920/demo-repo"),
            "error={error:#}"
        );
    }

    #[test]
    fn resolve_run_target_requires_yes_or_tty_for_github_repo_install() {
        let reporter = std::sync::Arc::new(reporters::CliReporter::new(false));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(resolve_run_target_or_install(
                PathBuf::from("github.com/Koh0920/demo-repo"),
                false,
                false,
                None,
                reporter,
            ))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Interactive install confirmation requires a TTY"),
            "error={error:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn github_install_build_step_runs_outside_async_runtime_worker() {
        let value = run_blocking_github_install_step(|| {
            let runtime = tokio::runtime::Runtime::new()?;
            Ok::<u8, anyhow::Error>(runtime.block_on(async { 7 }))
        })
        .await
        .unwrap();

        assert_eq!(value, 7);
    }

    #[test]
    fn search_tui_gate_requires_tty_and_flags_allowing_tui() {
        assert!(should_use_search_tui(true, true, false, false));
        assert!(!should_use_search_tui(false, true, false, false));
        assert!(!should_use_search_tui(true, false, false, false));
        assert!(!should_use_search_tui(true, true, true, false));
        assert!(!should_use_search_tui(true, true, false, true));
    }

    #[test]
    fn run_command_parses_explicit_state_bindings() {
        let cli = Cli::try_parse_from([
            "ato",
            "run",
            ".",
            "--state",
            "data=/var/lib/ato/persistent/demo",
            "--state",
            "cache=/var/lib/ato/persistent/cache",
        ])
        .expect("parse");

        match cli.command {
            Commands::Run { state, .. } => assert_eq!(
                state,
                vec![
                    "data=/var/lib/ato/persistent/demo".to_string(),
                    "cache=/var/lib/ato/persistent/cache".to_string()
                ]
            ),
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn state_command_parses_register_and_inspect_forms() {
        let register = Cli::try_parse_from([
            "ato",
            "state",
            "register",
            "--manifest",
            ".",
            "--name",
            "data",
            "--path",
            "/var/lib/ato/persistent/demo",
        ])
        .expect("parse register");

        match register.command {
            Commands::State {
                command:
                    StateCommands::Register {
                        manifest,
                        state_name,
                        path,
                        json,
                    },
            } => {
                assert_eq!(manifest, PathBuf::from("."));
                assert_eq!(state_name, "data");
                assert_eq!(path, PathBuf::from("/var/lib/ato/persistent/demo"));
                assert!(!json);
            }
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }

        let inspect =
            Cli::try_parse_from(["ato", "state", "inspect", "state-demo"]).expect("parse inspect");
        match inspect.command {
            Commands::State {
                command: StateCommands::Inspect { state_ref, json },
            } => {
                assert_eq!(state_ref, "state-demo");
                assert!(!json);
            }
            other => panic!("unexpected command: {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn parse_sha256_for_artifact_supports_sha256sums_format() {
        let body = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  nacelle-v1.2.3-darwin-arm64
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  nacelle-v1.2.3-linux-x64
";
        let parsed =
            crate::engine_manager::parse_sha256_for_artifact(body, "nacelle-v1.2.3-linux-x64");
        assert_eq!(
            parsed.as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
    }

    #[test]
    fn parse_sha256_for_artifact_supports_bsd_style_format() {
        let body = "SHA256 (nacelle-v1.2.3-darwin-arm64) = CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let parsed =
            crate::engine_manager::parse_sha256_for_artifact(body, "nacelle-v1.2.3-darwin-arm64");
        assert_eq!(
            parsed.as_deref(),
            Some("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc")
        );
    }

    #[test]
    fn extract_first_sha256_hex_reads_single_file_checksum() {
        let body = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd  nacelle-v1.2.3-darwin-arm64";
        let parsed = crate::engine_manager::extract_first_sha256_hex(body);
        assert_eq!(
            parsed.as_deref(),
            Some("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd")
        );
    }

    #[test]
    fn dangerous_skip_permissions_requires_explicit_opt_in_env() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::remove_var("CAPSULE_ALLOW_UNSAFE");

        let reporter = std::sync::Arc::new(reporters::CliReporter::new(true));
        let err = enforce_sandbox_mode_flags(EnforcementMode::Strict, false, true, reporter)
            .expect_err("must fail closed without env opt-in");
        assert!(err
            .to_string()
            .contains("--dangerously-skip-permissions requires CAPSULE_ALLOW_UNSAFE=1"));
    }

    #[test]
    fn dangerous_skip_permissions_allows_with_explicit_opt_in_env() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var("CAPSULE_ALLOW_UNSAFE", "1");

        let reporter = std::sync::Arc::new(reporters::CliReporter::new(true));
        let result = enforce_sandbox_mode_flags(EnforcementMode::Strict, false, true, reporter);
        assert!(result.is_ok());

        std::env::remove_var("CAPSULE_ALLOW_UNSAFE");
    }

    #[test]
    fn publish_private_status_message_for_build_path() {
        assert_eq!(
            publish_private_status_message(PublishTargetMode::CustomDirect, false),
            "📦 Building capsule artifact for private registry publish..."
        );
    }

    #[test]
    fn publish_private_status_message_for_upload_path() {
        assert_eq!(
            publish_private_status_message(PublishTargetMode::CustomDirect, true),
            "📤 Publishing provided artifact to private registry..."
        );
    }

    #[test]
    fn publish_private_status_message_for_personal_dock_build_path() {
        assert_eq!(
            publish_private_status_message(PublishTargetMode::PersonalDockDirect, false),
            "📦 Building capsule artifact for Personal Dock publish..."
        );
    }

    #[test]
    fn publish_private_status_message_for_personal_dock_upload_path() {
        assert_eq!(
            publish_private_status_message(PublishTargetMode::PersonalDockDirect, true),
            "📤 Publishing provided artifact to Personal Dock..."
        );
    }

    #[test]
    fn publish_private_start_summary_line_build_path() {
        let line = publish_private_start_summary_line(
            PublishTargetMode::CustomDirect,
            "http://127.0.0.1:8787",
            "build",
            "local/demo-app",
            "1.2.3",
            false,
        );
        assert!(line.contains("registry=http://127.0.0.1:8787"));
        assert!(line.contains("source=build"));
        assert!(line.contains("scoped_id=local/demo-app"));
        assert!(line.contains("version=1.2.3"));
        assert!(line.contains("allow_existing=false"));
    }

    #[test]
    fn publish_private_start_summary_line_artifact_path() {
        let line = publish_private_start_summary_line(
            PublishTargetMode::CustomDirect,
            "http://127.0.0.1:8787",
            "artifact",
            "team-x/demo-app",
            "1.2.3",
            true,
        );
        assert!(line.contains("source=artifact"));
        assert!(line.contains("allow_existing=true"));
    }

    #[test]
    fn publish_private_start_summary_line_marks_personal_dock_target() {
        let line = publish_private_start_summary_line(
            PublishTargetMode::PersonalDockDirect,
            "https://api.ato.run",
            "artifact",
            "koh0920/demo-app",
            "1.2.3",
            false,
        );
        assert!(line.contains("🔎 dock publish target"));
    }

    fn test_publish_args() -> PublishCommandArgs {
        PublishCommandArgs {
            registry: Some("http://127.0.0.1:8787".to_string()),
            artifact: None,
            scoped_id: None,
            allow_existing: false,
            prepare: false,
            build: false,
            deploy: false,
            legacy_full_publish: false,
            force_large_payload: false,
            fix: false,
            no_tui: false,
            json: true,
        }
    }

    #[test]
    fn publish_phase_selection_defaults_to_all_for_private() {
        let selected = select_publish_phases(false, false, false, false, false);
        assert!(selected.prepare);
        assert!(selected.build);
        assert!(selected.deploy);
        assert!(!selected.explicit_filter);
    }

    #[test]
    fn publish_phase_selection_respects_filter_flags() {
        let selected = select_publish_phases(true, false, true, true, false);
        assert!(selected.prepare);
        assert!(!selected.build);
        assert!(selected.deploy);
        assert!(selected.explicit_filter);
    }

    #[test]
    fn publish_phase_selection_defaults_to_deploy_for_official() {
        let selected = select_publish_phases(false, false, false, true, false);
        assert!(!selected.prepare);
        assert!(!selected.build);
        assert!(selected.deploy);
        assert!(!selected.explicit_filter);
    }

    #[test]
    fn publish_phase_selection_legacy_full_publish_keeps_all_for_official() {
        let selected = select_publish_phases(false, false, false, true, true);
        assert!(selected.prepare);
        assert!(selected.build);
        assert!(selected.deploy);
        assert!(!selected.explicit_filter);
    }

    #[test]
    fn resolve_publish_target_prefers_cli_registry_over_other_sources() {
        let resolved = resolve_publish_target_from_sources(
            Some("https://api.ato.run"),
            Some("http://127.0.0.1:8787"),
            Some("koh0920"),
        )
        .expect("resolve");

        assert_eq!(resolved.registry_url, "https://api.ato.run");
        assert_eq!(resolved.mode, PublishTargetMode::OfficialCi);
    }

    #[test]
    fn resolve_publish_target_uses_manifest_before_logged_in_default() {
        let resolved = resolve_publish_target_from_sources(
            None,
            Some("http://127.0.0.1:8787"),
            Some("koh0920"),
        )
        .expect("resolve");

        assert_eq!(resolved.registry_url, "http://127.0.0.1:8787");
        assert_eq!(resolved.mode, PublishTargetMode::CustomDirect);
    }

    #[test]
    fn resolve_publish_target_uses_logged_in_default_when_no_explicit_target_exists() {
        let resolved =
            resolve_publish_target_from_sources(None, None, Some("koh0920")).expect("resolve");

        assert_eq!(resolved.registry_url, "https://api.ato.run");
        assert_eq!(resolved.mode, PublishTargetMode::PersonalDockDirect);
        assert_eq!(resolved.publisher_handle.as_deref(), Some("koh0920"));
    }

    #[test]
    fn resolve_publish_target_errors_without_login_or_registry_override() {
        let err = resolve_publish_target_from_sources(None, None, None)
            .expect_err("must fail without any publish target");

        assert!(err.to_string().contains("Run `ato login`"));
        assert!(err.to_string().contains("--registry https://api.ato.run"));
    }

    #[test]
    fn resolve_publish_target_rejects_legacy_dock_registry_urls() {
        let err = resolve_publish_target_from_sources(
            Some("https://ato.run/d/koh0920"),
            None,
            Some("koh0920"),
        )
        .expect_err("must reject legacy dock url");
        assert!(err.to_string().contains("https://api.ato.run"));
        assert!(err.to_string().contains("/d/<handle>"));
    }

    #[test]
    fn is_legacy_dock_publish_registry_detects_dock_path_prefix() {
        assert!(is_legacy_dock_publish_registry("https://ato.run/d/koh0920"));
        assert!(is_legacy_dock_publish_registry(
            "https://ato.run/publish/d/koh0920"
        ));
        assert!(!is_legacy_dock_publish_registry("https://api.ato.run"));
    }

    #[test]
    fn publish_validate_rejects_allow_existing_without_deploy() {
        let mut args = test_publish_args();
        args.allow_existing = true;
        let selected = select_publish_phases(false, true, false, false, false);
        let err =
            validate_publish_phase_options(&args, selected, false).expect_err("must fail closed");
        assert!(err.to_string().contains("--allow-existing"));
    }

    #[test]
    fn publish_validate_rejects_fix_for_private_registry() {
        let mut args = test_publish_args();
        args.fix = true;
        let selected = select_publish_phases(false, false, true, false, false);
        let err =
            validate_publish_phase_options(&args, selected, false).expect_err("must fail closed");
        assert!(err.to_string().contains("--fix"));
    }

    #[test]
    fn publish_validate_requires_artifact_or_build_for_private_deploy_only() {
        let args = test_publish_args();
        let selected = select_publish_phases(false, false, true, false, false);
        let err =
            validate_publish_phase_options(&args, selected, false).expect_err("must fail closed");
        assert!(err.to_string().contains("--deploy requires --artifact"));
    }

    #[test]
    fn github_manual_intervention_extracts_required_env() {
        let required = required_env_from_preview_toml(
            r#"
[env]
required = ["DATABASE_URL", "REDIS_URL"]
"#,
        );

        assert_eq!(required, vec!["DATABASE_URL", "REDIS_URL"]);
    }

    #[test]
    fn github_manual_intervention_message_mentions_manifest_and_repo() {
        let draft = install::GitHubInstallDraftResponse {
            repo: install::GitHubInstallDraftRepo {
                owner: "octocat".to_string(),
                repo: "hello-world".to_string(),
                full_name: "octocat/hello-world".to_string(),
                default_branch: "main".to_string(),
            },
            capsule_toml: install::GitHubInstallDraftCapsuleToml { exists: false },
            repo_ref: "octocat/hello-world".to_string(),
            proposed_run_command: None,
            proposed_install_command: "ato run github.com/octocat/hello-world".to_string(),
            resolved_ref: install::GitHubInstallDraftResolvedRef {
                ref_name: "main".to_string(),
                sha: "deadbeef".to_string(),
            },
            manifest_source: "inferred".to_string(),
            preview_toml: Some(
                r#"
[env]
required = ["DATABASE_URL"]
"#
                .to_string(),
            ),
            capsule_hint: Some(install::GitHubInstallDraftHint {
                confidence: "medium".to_string(),
                warnings: vec!["外部DBの準備が必要です。".to_string()],
                launchability: Some("manual_review".to_string()),
            }),
            inference_mode: Some("rules".to_string()),
            retryable: false,
        };

        let message = build_github_manual_intervention_message(
            "github.com/octocat/hello-world",
            &draft,
            std::path::Path::new("/repo/.tmp/ato-inference/attempt/capsule.toml"),
            "Smoke failed",
        );

        assert!(message.contains("manual intervention required"));
        assert!(message.contains("DATABASE_URL"));
        assert!(message.contains("github.com/octocat/hello-world"));
        assert!(message.contains("/repo/.tmp/ato-inference/attempt/capsule.toml"));
    }
}
