use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub(crate) enum IdentitySessionCommands {
    /// Unlock the age identity for the duration of the session
    ///
    /// Decrypts the identity key and keeps the unlocked key in a per-process
    /// session file at ~/.ato/run/session-{pid}.key (chmod 600).
    /// Subsequent `ato secrets` calls in child processes pick it up via
    /// ATO_SESSION_KEY_FILE so they never re-prompt for a passphrase.
    #[command(name = "start")]
    Start {
        /// Session TTL (e.g. "1h", "30m", "8h"). Defaults to 8h.
        #[arg(long, value_name = "DURATION", default_value = "8h")]
        ttl: String,
    },

    /// Revoke the current session and delete the session key file
    #[command(name = "end")]
    End,

    /// Print information about the current session
    #[command(name = "status")]
    Status,
}
