//! Socket Activation Manager
//!
//! Implements Systemd-compatible Socket Activation for zero-downtime port binding.
//! The parent process (nacelle) binds the listening socket and passes it to child
//! processes via file descriptor inheritance, eliminating port clash risks.
//!
//! ## Reference
//! - Systemd Socket Activation: https://www.freedesktop.org/software/systemd/man/sd_listen_fds.html
//! - SD_LISTEN_FDS_START = 3 (first passed FD after stdin/stdout/stderr)

use std::net::{SocketAddr, TcpListener};
use std::os::fd::{AsRawFd, RawFd};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info};

#[cfg(not(unix))]
use tracing::warn;

/// Systemd Socket Activation FD start constant
/// FDs 0, 1, 2 are stdin, stdout, stderr - socket activation starts at 3
pub const SD_LISTEN_FDS_START: RawFd = 3;

/// Environment variable indicating number of file descriptors passed
pub const ENV_LISTEN_FDS: &str = "LISTEN_FDS";

/// Environment variable with PID of the listener process
pub const ENV_LISTEN_PID: &str = "LISTEN_PID";

/// Configuration for socket activation
#[derive(Debug, Clone)]
pub struct SocketConfig {
    /// Port to bind
    pub port: u16,
    /// Bind address (default: 0.0.0.0)
    pub host: String,
    /// Enable socket activation (vs. traditional child binding)
    pub enabled: bool,
}

impl Default for SocketConfig {
    fn default() -> Self {
        Self {
            port: 8000,
            host: "0.0.0.0".to_string(),
            enabled: true,
        }
    }
}

/// Manages a listening socket for socket activation
#[derive(Debug)]
pub struct SocketManager {
    /// The bound TCP listener
    listener: TcpListener,
    /// Configuration
    config: SocketConfig,
}

impl SocketManager {
    /// Create a new SocketManager and bind to the specified address
    ///
    /// This method binds the socket immediately, reserving the port for the parent process.
    /// The socket will be inherited by child processes via `prepare_for_child`.
    pub fn new(config: SocketConfig) -> Result<Self> {
        let addr: SocketAddr = format!("{}:{}", config.host, config.port)
            .parse()
            .context("Invalid socket address")?;

        info!(
            "Socket Activation: Binding to {} (port reservation by parent)",
            addr
        );

        let listener = TcpListener::bind(addr)
            .with_context(|| format!("Failed to bind socket on {}", addr))?;

        // Set SO_REUSEADDR to allow quick rebind after restart
        // Note: This is handled automatically by std::net::TcpListener on most platforms

        info!(
            "Socket Activation: Successfully bound to {}, fd={}",
            addr,
            listener.as_raw_fd()
        );

        Ok(Self { listener, config })
    }

    /// Get the raw file descriptor of the listening socket
    pub fn raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    /// Get the bound port
    pub fn port(&self) -> u16 {
        self.listener
            .local_addr()
            .map(|addr| addr.port())
            .unwrap_or(self.config.port)
    }

    /// Get a reference to the underlying TcpListener
    pub fn listener(&self) -> &TcpListener {
        &self.listener
    }

    /// Prepare a Command for socket activation
    ///
    /// This method configures the child process to inherit the socket FD:
    /// 1. Sets LISTEN_FDS=1 environment variable
    /// 2. Sets LISTEN_PID to the current process PID
    /// 3. Uses pre_exec hook to duplicate the socket FD to SD_LISTEN_FDS_START (3)
    ///
    /// # Safety
    /// This method uses unsafe code in the pre_exec hook to call libc::dup2.
    /// The pre_exec hook runs after fork() but before exec(), so it's safe
    /// as long as we only use async-signal-safe functions.
    #[cfg(unix)]
    pub fn prepare_command(&self, cmd: &mut Command) -> Result<()> {
        use std::os::unix::process::CommandExt;

        let socket_fd = self.raw_fd();
        let target_fd = SD_LISTEN_FDS_START;

        debug!(
            "Socket Activation: Preparing FD inheritance, source_fd={}, target_fd={}",
            socket_fd, target_fd
        );

        // Set environment variables for socket activation
        cmd.env(ENV_LISTEN_FDS, "1");
        cmd.env(ENV_LISTEN_PID, std::process::id().to_string());

        // If the socket FD is already at the target position, we need to handle it differently
        if socket_fd == target_fd {
            debug!("Socket FD is already at target position {}", target_fd);
            // Clear FD_CLOEXEC flag so it survives exec
            unsafe {
                cmd.pre_exec(move || {
                    let flags = libc::fcntl(target_fd, libc::F_GETFD);
                    if flags < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        } else {
            // Duplicate socket FD to position 3 (SD_LISTEN_FDS_START)
            unsafe {
                cmd.pre_exec(move || {
                    // dup2 closes target_fd if open, then duplicates socket_fd to target_fd
                    if libc::dup2(socket_fd, target_fd) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }

                    // Clear FD_CLOEXEC on the new FD so it survives exec
                    let flags = libc::fcntl(target_fd, libc::F_GETFD);
                    if flags < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }

                    info!(
                        "Socket Activation: FD {} duplicated to FD {}",
                        socket_fd, target_fd
                    );

                    Ok(())
                });
            }
        }

        info!(
            "Socket Activation: Passing FD {} to child process as FD {}",
            socket_fd, target_fd
        );

        Ok(())
    }

    /// Prepare command for socket activation (non-Unix stub)
    #[cfg(not(unix))]
    pub fn prepare_command(&self, cmd: &mut Command) -> Result<()> {
        warn!("Socket Activation: Not supported on this platform, child will bind its own socket");
        Ok(())
    }
}

/// Shared socket manager for use across runtimes
pub type SharedSocketManager = Arc<SocketManager>;

/// Create a new shared socket manager
pub fn create_socket_manager(config: SocketConfig) -> Result<SharedSocketManager> {
    Ok(Arc::new(SocketManager::new(config)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_manager_bind() {
        // Use a random high port to avoid conflicts
        let config = SocketConfig {
            port: 0, // Let OS assign port
            host: "127.0.0.1".to_string(),
            enabled: true,
        };

        let manager = SocketManager::new(config);
        assert!(manager.is_ok());

        let manager = manager.unwrap();
        assert!(manager.raw_fd() >= 0);
    }

    #[test]
    fn test_socket_config_default() {
        let config = SocketConfig::default();
        assert_eq!(config.port, 8000);
        assert_eq!(config.host, "0.0.0.0");
        assert!(config.enabled);
    }
}
