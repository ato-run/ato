//! Local half of the certificate-Binding renewal path (design: ADR-044).
//!
//! The tool answers one question for one allowlisted (Instance, domain) pair:
//! is the certificate/private-key pair on this host safe to hand to a new Run,
//! and what exactly is it? It never performs a network call, never starts a
//! Run, and never prints key material outside the one command whose output is
//! the Binding update payload itself.
//!
//! Boundaries this crate keeps:
//!
//! - The allowlist, not the caller, decides which Instance and domain exist.
//! - Every check runs before the first byte of the export directory changes.
//! - The private key is read by 'openssl' from its path; it is never an
//!   argument, an environment value or a stdin payload.
//! - Nothing derived from the key (not even a digest) leaves the host in the
//!   Binding update payload; meta.json keeps the digest only as a local
//!   change-detection fingerprint under a root-only directory.

pub mod allowlist;
pub mod cert;
pub mod export;
pub mod payload;
pub mod time;

use std::fmt;

/// How a failure should be reported, and which exit code it maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The source pair is not safe to use. Nothing was written.
    Validation,
    /// The invocation or the allowlist does not describe a usable request.
    Usage,
    /// The tool is not allowed to read or write the material it was asked to.
    Privilege,
    /// An external tool (openssl) or the host filesystem failed.
    Tool,
    /// The export directory could not be read or written consistently.
    Io,
}

impl ErrorClass {
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Validation => 2,
            Self::Usage => 3,
            Self::Privilege => 4,
            Self::Tool | Self::Io => 5,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::Usage => "usage",
            Self::Privilege => "privilege",
            Self::Tool => "tool",
            Self::Io => "io",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    class: ErrorClass,
    message: String,
}

impl Error {
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Validation, message)
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Usage, message)
    }

    pub fn privilege(message: impl Into<String>) -> Self {
        Self::new(ErrorClass::Privilege, message)
    }

    pub fn tool(error: impl fmt::Display) -> Self {
        Self::new(ErrorClass::Tool, error.to_string())
    }

    pub fn io(error: impl fmt::Display) -> Self {
        Self::new(ErrorClass::Io, error.to_string())
    }

    pub fn class(&self) -> ErrorClass {
        self.class
    }

    pub fn exit_code(&self) -> i32 {
        self.class.exit_code()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.class.as_str(), self.message)
    }
}

impl std::error::Error for Error {}

impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Self::tool(format!("{error:#}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::tool(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// The effective uid of this process, when the platform reports one.
///
/// The tool is a Linux host one-shot; on other platforms the answer is
/// unknown, which the privilege check treats as "not verified" rather than
/// "root".
pub fn effective_uid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("Uid:") else {
            continue;
        };
        // real, effective, saved, filesystem
        let effective = rest.split_whitespace().nth(1)?;
        return effective.parse().ok();
    }
    None
}

/// Refuse to handle key material without root, unless this is an explicit
/// test-mode run.
pub fn require_privilege(allow_unprivileged: bool, effective_uid: Option<u32>) -> Result<()> {
    if allow_unprivileged {
        return Ok(());
    }
    match effective_uid {
        Some(0) => Ok(()),
        Some(uid) => Err(Error::privilege(format!(
            "refusing to handle private key material without root (effective uid {uid}); --allow-unprivileged is for local tests only"
        ))),
        None => Err(Error::privilege(
            "cannot verify root privileges on this platform; --allow-unprivileged is for local tests only",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_separate_rejections_from_operational_failures() {
        assert_eq!(Error::validation("x").exit_code(), 2);
        assert_eq!(Error::usage("x").exit_code(), 3);
        assert_eq!(Error::privilege("x").exit_code(), 4);
        assert_eq!(Error::tool("x").exit_code(), 5);
        assert_eq!(Error::io("x").exit_code(), 5);
        assert!(
            Error::validation("x")
                .to_string()
                .starts_with("validation: ")
        );
    }

    #[test]
    fn root_is_required_unless_the_run_declares_itself_a_test() {
        assert!(require_privilege(false, Some(0)).is_ok());
        assert!(require_privilege(true, Some(1000)).is_ok());
        assert!(require_privilege(true, None).is_ok());
        assert_eq!(
            require_privilege(false, Some(1000)).unwrap_err().class(),
            ErrorClass::Privilege
        );
        assert_eq!(
            require_privilege(false, None).unwrap_err().class(),
            ErrorClass::Privilege
        );
    }
}
