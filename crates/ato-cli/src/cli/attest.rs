use std::path::PathBuf;

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum AttestCommands {
    /// Generate a new Ed25519 attestation keypair and write it to `--out`.
    #[command(about = "Generate an A2 attestation keypair")]
    Keygen {
        /// Path the StoredAttestationKey JSON should be written to.
        #[arg(long = "out", value_name = "PATH")]
        out: PathBuf,

        /// Overwrite the file if it already exists.
        #[arg(long, default_value_t = false)]
        force: bool,

        /// Also register the public key as a local trust root.
        #[arg(long, default_value_t = false)]
        trust: bool,

        /// Optional label saved alongside the trust root.
        #[arg(long)]
        label: Option<String>,
    },

    /// Register a public key as a local trust root.
    ///
    /// One of `--pubkey` or `--from-key` must be provided.
    #[command(about = "Register an A2 attestation trust root")]
    Trust {
        /// Base64-encoded raw 32-byte Ed25519 public key.
        #[arg(long = "pubkey", conflicts_with = "from_key")]
        pubkey: Option<String>,

        /// Path to a StoredAttestationKey JSON; the public half is registered.
        #[arg(long = "from-key", value_name = "PATH", conflicts_with = "pubkey")]
        from_key: Option<PathBuf>,

        /// Optional human-readable label.
        #[arg(long)]
        label: Option<String>,
    },

    /// List trust roots registered in `~/.ato/trust/roots/`.
    #[command(about = "List A2 attestation trust roots")]
    TrustList,

    /// Verify the attestations stored for a blob hash.
    #[command(about = "Verify A2 attestations for a blob")]
    Verify {
        /// Subject blob hash (`sha256:<hex>`).
        #[arg(long = "blob", value_name = "HASH")]
        blob: String,

        /// Pretty-print the JSON verdict (default: compact).
        #[arg(long, default_value_t = false)]
        pretty: bool,
    },
}
