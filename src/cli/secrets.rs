use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum SecretsCommands {
    /// Initialize the age identity (run once before using secrets)
    #[command(name = "init")]
    Init {
        /// Store identity as plain text without passphrase protection (chmod 600)
        #[arg(long, default_value_t = false)]
        no_passphrase: bool,

        /// Use an existing SSH ed25519 key as the identity
        #[arg(long, value_name = "PATH")]
        ssh_key: Option<PathBuf>,
    },

    /// Store a secret value in the secure store
    #[command(name = "set")]
    Set {
        /// Secret key name (e.g. OPENAI_API_KEY)
        key: String,

        /// Namespace: "default", "publish", or "capsule:<name>"
        #[arg(long, value_name = "NS", default_value = "default")]
        namespace: String,

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

        /// Namespace to read from
        #[arg(long, value_name = "NS", default_value = "default")]
        namespace: String,
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

        /// Namespace to delete from ("*" to remove from all namespaces)
        #[arg(long, value_name = "NS", default_value = "default")]
        namespace: String,
    },

    /// Import secrets from a .env file into the secure store
    #[command(name = "import")]
    Import {
        /// Path to the .env file to import
        #[arg(long, value_name = "PATH")]
        env_file: PathBuf,

        /// Target namespace
        #[arg(long, value_name = "NS", default_value = "default")]
        namespace: String,
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

    /// Re-encrypt all secrets under a new age identity
    #[command(name = "rotate-identity")]
    RotateIdentity {
        /// Path to the new identity file (generated if omitted)
        #[arg(long, value_name = "PATH")]
        new_identity: Option<PathBuf>,
    },
}
