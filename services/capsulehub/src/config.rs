//! Operator configuration.
//!
//! Every value that matters in production is explicit: there is no default
//! bind address, no default storage directory, and no default upload token.
//! Tokens are only read from flags or `ATO_CAPSULEHUB_*` environment aliases
//! and are redacted from `Debug` output.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser, ValueEnum};

use crate::auth::Secret;

/// Default `--max-object-bytes`: 1 GiB of ciphertext per object.
pub const DEFAULT_MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
/// Default `--rate-limit-per-minute` per client IP.
pub const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 60;

/// Upload authentication mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PutAuth {
    /// Uploads require `Authorization: Bearer <--put-token>`.
    Token,
    /// Any client may upload. Only appropriate for a trusted network.
    Open,
}

/// Command-line surface of `ato-capsulehub`.
#[derive(Parser)]
#[command(
    name = "ato-capsulehub",
    about = "Self-hosted capsulehub coordinator (blind ciphertext storage)",
    version
)]
pub struct Config {
    /// Explicit listen address, e.g. `127.0.0.1:8787` or `[::]:8787`.
    #[arg(long, env = "ATO_CAPSULEHUB_BIND")]
    pub bind: String,

    /// Storage root. Objects land in `<storage>/objects`, uploads in
    /// `<storage>/tmp`.
    #[arg(long, env = "ATO_CAPSULEHUB_STORAGE")]
    pub storage: PathBuf,

    /// Maximum ciphertext bytes accepted for one object.
    #[arg(
        long,
        env = "ATO_CAPSULEHUB_MAX_OBJECT_BYTES",
        default_value_t = DEFAULT_MAX_OBJECT_BYTES
    )]
    pub max_object_bytes: u64,

    /// Optional total storage budget in bytes. Uploads that would exceed it
    /// are rejected with 507.
    #[arg(long, env = "ATO_CAPSULEHUB_MAX_STORAGE_BYTES")]
    pub max_storage_bytes: Option<u64>,

    /// Upload authentication mode.
    #[arg(
        long,
        env = "ATO_CAPSULEHUB_PUT_AUTH",
        value_enum,
        default_value_t = PutAuth::Token
    )]
    pub put_auth: PutAuth,

    /// Upload bearer token. Required unless `--put-auth open`.
    #[arg(
        long,
        env = "ATO_CAPSULEHUB_PUT_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub put_token: Option<String>,

    /// Operator takedown token for `DELETE`. When unset, `DELETE` always
    /// returns 403.
    #[arg(
        long,
        env = "ATO_CAPSULEHUB_DELETE_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub delete_token: Option<String>,

    /// Per-client-IP request budget per minute. `0` disables rate limiting.
    #[arg(
        long,
        env = "ATO_CAPSULEHUB_RATE_LIMIT_PER_MINUTE",
        default_value_t = DEFAULT_RATE_LIMIT_PER_MINUTE
    )]
    pub rate_limit_per_minute: u32,

    /// Optional object retention in seconds. When set, a background sweep
    /// removes objects (and abandoned uploads) older than this.
    #[arg(long, env = "ATO_CAPSULEHUB_RETENTION_SECONDS")]
    pub retention_seconds: Option<u64>,

    /// Allow unauthenticated `GET`/`HEAD`. There is nothing to authorize in a
    /// blind read, but an operator may still require the upload token.
    #[arg(
        long,
        env = "ATO_CAPSULEHUB_ALLOW_ANONYMOUS_READ",
        default_value_t = true,
        action = ArgAction::Set
    )]
    pub allow_anonymous_read: bool,
}

impl Config {
    /// Validates the operator configuration into a runnable server config.
    pub fn into_server_config(self) -> Result<ServerConfig> {
        let bind: SocketAddr = self
            .bind
            .parse()
            .with_context(|| format!("--bind `{}` is not a socket address", self.bind))?;
        if self.storage.as_os_str().is_empty() {
            bail!("--storage must not be empty");
        }
        if self.max_object_bytes == 0 {
            bail!("--max-object-bytes must be greater than 0");
        }
        if self.max_storage_bytes == Some(0) {
            bail!("--max-storage-bytes must be greater than 0 when set");
        }
        if self.retention_seconds == Some(0) {
            bail!(
                "--retention-seconds must be greater than 0 when set (omit it to disable retention)"
            );
        }
        let put_token = match self.put_auth {
            PutAuth::Token => match self.put_token {
                Some(token) if !token.is_empty() => Some(Secret::new(token)),
                Some(_) => bail!("--put-token must not be empty"),
                None => bail!(
                    "--put-token (or ATO_CAPSULEHUB_PUT_TOKEN) is required unless --put-auth open"
                ),
            },
            PutAuth::Open => {
                if self.put_token.is_some() {
                    tracing::warn!("--put-token is ignored because --put-auth is open");
                }
                None
            }
        };
        if !self.allow_anonymous_read && put_token.is_none() {
            bail!("--allow-anonymous-read=false requires --put-auth token and --put-token");
        }
        if bind.ip().is_unspecified() {
            tracing::warn!(
                address = %bind,
                "ato-capsulehub is binding a wildcard address; verify the network exposure"
            );
        }
        Ok(ServerConfig {
            bind,
            storage: self.storage,
            max_object_bytes: self.max_object_bytes,
            max_storage_bytes: self.max_storage_bytes,
            put_auth: self.put_auth,
            put_token,
            delete_token: self.delete_token.map(Secret::new),
            rate_limit_per_minute: self.rate_limit_per_minute,
            retention: self.retention_seconds.map(Duration::from_secs),
            allow_anonymous_read: self.allow_anonymous_read,
        })
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("bind", &self.bind)
            .field("storage", &self.storage)
            .field("max_object_bytes", &self.max_object_bytes)
            .field("max_storage_bytes", &self.max_storage_bytes)
            .field("put_auth", &self.put_auth)
            .field("put_token", &self.put_token.as_ref().map(|_| "<redacted>"))
            .field(
                "delete_token",
                &self.delete_token.as_ref().map(|_| "<redacted>"),
            )
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .field("retention_seconds", &self.retention_seconds)
            .field("allow_anonymous_read", &self.allow_anonymous_read)
            .finish()
    }
}

/// Validated runtime configuration.
#[derive(Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub storage: PathBuf,
    pub max_object_bytes: u64,
    pub max_storage_bytes: Option<u64>,
    pub put_auth: PutAuth,
    pub put_token: Option<Secret>,
    pub delete_token: Option<Secret>,
    pub rate_limit_per_minute: u32,
    pub retention: Option<Duration>,
    pub allow_anonymous_read: bool,
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("bind", &self.bind)
            .field("storage", &self.storage)
            .field("max_object_bytes", &self.max_object_bytes)
            .field("max_storage_bytes", &self.max_storage_bytes)
            .field("put_auth", &self.put_auth)
            .field("put_token", &self.put_token.as_ref().map(|_| "<redacted>"))
            .field(
                "delete_token",
                &self.delete_token.as_ref().map(|_| "<redacted>"),
            )
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .field("retention", &self.retention)
            .field("allow_anonymous_read", &self.allow_anonymous_read)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn parse(arguments: &[&str]) -> Result<ServerConfig, String> {
        let config = Config::try_parse_from(arguments).map_err(|error| error.to_string())?;
        config
            .into_server_config()
            .map_err(|error| error.to_string())
    }

    fn base_arguments() -> Vec<&'static str> {
        vec![
            "ato-capsulehub",
            "--bind",
            "127.0.0.1:0",
            "--storage",
            "./.tmp/capsulehub-test",
        ]
    }

    #[test]
    fn requires_an_explicit_bind_and_storage() {
        assert!(Config::try_parse_from(["ato-capsulehub"]).is_err());
        assert!(Config::try_parse_from(["ato-capsulehub", "--bind", "127.0.0.1:0"]).is_err());
        assert!(
            Config::try_parse_from(["ato-capsulehub", "--storage", "./.tmp/x"]).is_err(),
            "bind must not default"
        );
    }

    #[test]
    fn token_mode_requires_a_token_and_never_defaults_one() {
        let mut arguments = base_arguments();
        assert!(parse(&arguments).is_err(), "put token is required");
        arguments.extend(["--put-token", "upload-token"]);
        let config = parse(&arguments).unwrap();
        assert_eq!(config.put_auth, PutAuth::Token);
        assert_eq!(config.put_token.as_ref().unwrap().expose(), "upload-token");
        assert!(config.delete_token.is_none());
    }

    #[test]
    fn open_mode_needs_no_token_and_rejects_the_anonymous_read_conflict() {
        let mut arguments = base_arguments();
        arguments.extend(["--put-auth", "open"]);
        let config = parse(&arguments).unwrap();
        assert_eq!(config.put_auth, PutAuth::Open);
        assert!(config.put_token.is_none());

        arguments.extend(["--allow-anonymous-read", "false"]);
        assert!(parse(&arguments).is_err());
    }

    #[test]
    fn anonymous_read_can_be_disabled_with_the_put_token() {
        let mut arguments = base_arguments();
        arguments.extend(["--put-token", "token", "--allow-anonymous-read", "false"]);
        let config = parse(&arguments).unwrap();
        assert!(!config.allow_anonymous_read);
        assert_eq!(config.rate_limit_per_minute, DEFAULT_RATE_LIMIT_PER_MINUTE);
    }

    #[test]
    fn rejects_invalid_limits() {
        let mut arguments = base_arguments();
        arguments.extend(["--put-token", "token", "--max-object-bytes", "0"]);
        assert!(parse(&arguments).is_err());

        let mut arguments = base_arguments();
        arguments.extend(["--put-token", "token", "--max-storage-bytes", "0"]);
        assert!(parse(&arguments).is_err());

        let mut arguments = base_arguments();
        arguments.extend(["--put-token", "token", "--retention-seconds", "0"]);
        assert!(parse(&arguments).is_err());

        let mut arguments = base_arguments();
        arguments[2] = "not-an-address";
        arguments.extend(["--put-token", "token"]);
        assert!(parse(&arguments).is_err());
    }

    #[test]
    fn debug_output_redacts_tokens() {
        let mut arguments = base_arguments();
        arguments.extend([
            "--put-token",
            "upload-secret",
            "--delete-token",
            "delete-secret",
        ]);
        let config = Config::try_parse_from(&arguments).unwrap();
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("upload-secret"));
        assert!(!rendered.contains("delete-secret"));
        assert!(rendered.contains("<redacted>"));

        let server = config.into_server_config().unwrap();
        let rendered = format!("{server:?}");
        assert!(!rendered.contains("upload-secret"));
        assert!(!rendered.contains("delete-secret"));
    }

    #[test]
    fn rate_limit_zero_is_allowed() {
        let mut arguments = base_arguments();
        arguments.extend(["--put-token", "token", "--rate-limit-per-minute", "0"]);
        assert_eq!(parse(&arguments).unwrap().rate_limit_per_minute, 0);
    }
}
