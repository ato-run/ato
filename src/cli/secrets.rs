use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum SecretsCommands {
    /// Store a secret value in the keychain (or secure local file)
    #[command(name = "set")]
    Set {
        /// Secret key name (e.g. OPENAI_API_KEY)
        key: String,

        /// Optional description for this secret
        #[arg(long)]
        description: Option<String>,

        /// Allow specific capsule IDs to access this secret (glob patterns, repeatable)
        #[arg(long, value_name = "PATTERN")]
        allow: Vec<String>,

        /// Deny specific capsule IDs from accessing this secret (glob patterns, repeatable)
        #[arg(long, value_name = "PATTERN")]
        deny: Vec<String>,
    },

    /// Retrieve and display a secret value
    #[command(name = "get")]
    Get {
        /// Secret key name
        key: String,
    },

    /// List all stored secrets (metadata only, no values)
    #[command(name = "list")]
    List {
        /// Output as JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },

    /// Delete a stored secret
    #[command(name = "delete", alias = "rm")]
    Delete {
        /// Secret key name
        key: String,
    },

    /// Import secrets from a .env file into the secure store
    #[command(name = "import")]
    Import {
        /// Path to the .env file to import
        #[arg(long, value_name = "PATH")]
        env_file: PathBuf,
    },

    /// Grant a capsule ID access to a secret
    #[command(name = "allow")]
    Allow {
        /// Secret key name
        key: String,
        /// Capsule ID or glob pattern to allow
        capsule_id: String,
    },

    /// Revoke a capsule ID's access to a secret
    #[command(name = "deny")]
    Deny {
        /// Secret key name
        key: String,
        /// Capsule ID or glob pattern to deny
        capsule_id: String,
    },
}
