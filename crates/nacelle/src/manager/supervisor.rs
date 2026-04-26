//! v0.2.0: Actor-based Process Supervisor
//!
//! Implements the Actor Model (Tokio Task + MPSC Channel) for process management.
//! Key features:
//! - **Deadlock-free**: All state changes via message passing, no Mutex
//! - **Signal Handling**: SIGTERM/SIGINT handled in Actor loop via tokio::select!
//! - **Graceful Shutdown**: Process group termination with timeout and force kill
//! - **Clone-safe Handle**: ProcessSupervisor is a lightweight Clone-able handle
//!
//! ## Architecture
//! ```text
//! ┌─────────────────┐        ┌─────────────────────────────┐
//! │ ProcessSupervisor│       │      SupervisorActor        │
//! │    (Handle)     │───────▶│   (tokio::spawn task)       │
//! │                 │  mpsc  │                             │
//! └─────────────────┘        │  tokio::select! {           │
//!                            │    msg from channel,        │
//!                            │    SIGTERM,                 │
//!                            │    SIGINT                   │
//!                            │  }                          │
//!                            └─────────────────────────────┘
//! ```

use anyhow::Result;
use std::collections::HashMap;
use std::process::Child;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

// Import sandbox module
use crate::system::sandbox::SandboxPolicy;

// Re-export sandbox types for convenience
pub use crate::system::sandbox::{SandboxPolicy as SandboxConfig, SandboxResult};

// ═══════════════════════════════════════════════════════════════════════════
// Message Types for Actor Communication
// ═══════════════════════════════════════════════════════════════════════════

/// Messages that can be sent to the SupervisorActor
#[derive(Debug)]
pub enum SupervisorMessage {
    /// Start a new process with the given command
    Start {
        name: String,
        command: String,
        args: Vec<String>,
        envs: Vec<(String, String)>,
        working_dir: Option<std::path::PathBuf>,
        /// Optional sandbox policy for process isolation
        sandbox_policy: Option<SandboxPolicy>,
        resp: oneshot::Sender<Result<u32, String>>, // Returns PID on success
    },
    /// Stop a specific process by name
    Stop {
        name: String,
        resp: oneshot::Sender<Result<(), String>>,
    },
    /// Register an existing child process (for external spawning)
    Register { id: String, child: Child },
    /// Unregister and kill a child process
    Unregister { id: String },
    /// Gracefully shutdown all processes and terminate actor
    Shutdown { resp: oneshot::Sender<()> },
    /// Get status of all processes
    GetStatus {
        response_tx: oneshot::Sender<HashMap<String, ProcessStatus>>,
    },
}

/// Status information for a managed process
#[derive(Debug, Clone)]
pub struct ProcessStatus {
    pub id: String,
    pub pid: Option<u32>,
    pub state: ProcessState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessState {
    Running,
    Stopped,
    Failed,
}

// ═══════════════════════════════════════════════════════════════════════════
// SupervisorActor - The Core Actor (Internal)
// ═══════════════════════════════════════════════════════════════════════════

/// Internal actor that manages child processes via message passing.
/// This runs as a separate tokio task and handles all process lifecycle events.
struct SupervisorActor {
    /// Managed child processes (name -> Child)
    children: HashMap<String, Child>,
    /// Channel to receive messages from handles
    receiver: mpsc::UnboundedReceiver<SupervisorMessage>,
    /// Shutdown timeout in seconds
    shutdown_timeout_secs: u64,
}

impl SupervisorActor {
    /// Create a new SupervisorActor
    fn new(receiver: mpsc::UnboundedReceiver<SupervisorMessage>) -> Self {
        Self {
            children: HashMap::new(),
            receiver,
            shutdown_timeout_secs: 5,
        }
    }

    /// Main event loop - processes messages and signals until shutdown
    /// Uses tokio::select! to handle both messages and OS signals atomically
    #[cfg(unix)]
    async fn run(mut self) {
        info!("SupervisorActor started (PID: {})", std::process::id());

        // Setup signal handlers
        let mut sig_term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to setup SIGTERM handler: {}", e);
                return;
            }
        };
        let mut sig_int = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to setup SIGINT handler: {}", e);
                return;
            }
        };

        loop {
            tokio::select! {
                // Handle incoming messages
                Some(msg) = self.receiver.recv() => {
                    if self.handle_message(msg).await {
                        break; // Shutdown requested
                    }
                }
                // Handle SIGTERM (graceful shutdown)
                _ = sig_term.recv() => {
                    info!("Received SIGTERM, initiating graceful shutdown...");
                    self.graceful_shutdown().await;
                    break;
                }
                // Handle SIGINT (Ctrl+C)
                _ = sig_int.recv() => {
                    info!("Received SIGINT (Ctrl+C), initiating graceful shutdown...");
                    self.graceful_shutdown().await;
                    break;
                }
            }
        }

        info!("SupervisorActor stopped");
    }

    /// Windows version (no Unix signals)
    #[cfg(not(unix))]
    async fn run(mut self) {
        info!("SupervisorActor started (PID: {})", std::process::id());

        loop {
            tokio::select! {
                Some(msg) = self.receiver.recv() => {
                    if self.handle_message(msg).await {
                        break;
                    }
                }
                // Windows: Use Ctrl+C handler
                _ = tokio::signal::ctrl_c() => {
                    info!("Received Ctrl+C, initiating graceful shutdown...");
                    self.graceful_shutdown().await;
                    break;
                }
            }
        }

        info!("SupervisorActor stopped");
    }

    /// Handle a single message, returns true if shutdown was requested
    async fn handle_message(&mut self, msg: SupervisorMessage) -> bool {
        match msg {
            SupervisorMessage::Start {
                name,
                command,
                args,
                envs,
                working_dir,
                sandbox_policy,
                resp,
            } => {
                let result = self.handle_start(
                    &name,
                    &command,
                    &args,
                    &envs,
                    working_dir.as_deref(),
                    sandbox_policy.as_ref(),
                );
                let _ = resp.send(result);
                false
            }
            SupervisorMessage::Stop { name, resp } => {
                let result = self.handle_stop(&name);
                let _ = resp.send(result);
                false
            }
            SupervisorMessage::Register { id, child } => {
                self.handle_register(id, child);
                false
            }
            SupervisorMessage::Unregister { id } => {
                self.handle_unregister(&id);
                false
            }
            SupervisorMessage::Shutdown { resp } => {
                self.graceful_shutdown().await;
                let _ = resp.send(());
                true // Signal to exit loop
            }
            SupervisorMessage::GetStatus { response_tx } => {
                self.handle_get_status(response_tx);
                false
            }
        }
    }

    /// Start a new process
    fn handle_start(
        &mut self,
        name: &str,
        command: &str,
        args: &[String],
        envs: &[(String, String)],
        working_dir: Option<&std::path::Path>,
        sandbox_policy: Option<&SandboxPolicy>,
    ) -> Result<u32, String> {
        if self.children.contains_key(name) {
            return Err(format!("Process '{}' already exists", name));
        }

        debug!("Starting process '{}': {} {:?}", name, command, args);

        let mut cmd = std::process::Command::new(command);
        cmd.args(args);

        for (key, value) in envs {
            cmd.env(key, value);
        }

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Setup process group and sandbox for proper signal propagation
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            // Create new process group with child as leader
            cmd.process_group(0);

            // Apply sandbox in pre_exec hook if policy is provided
            if let Some(policy) = sandbox_policy {
                let policy = policy.clone();
                unsafe {
                    cmd.pre_exec(move || {
                        // Apply sandbox before exec
                        match crate::system::sandbox::apply_sandbox(&policy) {
                            Ok(result) => {
                                if result.fully_enforced {
                                    // Sandbox applied successfully
                                    Ok(())
                                } else if result.partially_enforced {
                                    // Sandbox partially applied - continue but log
                                    eprintln!(
                                        "Warning: Sandbox partially enforced: {}",
                                        result.message
                                    );
                                    Ok(())
                                } else {
                                    // Sandbox not enforced - continue in dev mode
                                    eprintln!("Warning: Sandbox not enforced: {}", result.message);
                                    Ok(())
                                }
                            }
                            Err(e) => {
                                eprintln!("Sandbox error: {}", e);
                                // Return error to abort exec
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    format!("Failed to apply sandbox: {}", e),
                                ))
                            }
                        }
                    });
                }
            }
        }

        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                info!(
                    "Started process '{}' with PID {}{}",
                    name,
                    pid,
                    if sandbox_policy.is_some() {
                        " (sandboxed)"
                    } else {
                        ""
                    }
                );
                self.children.insert(name.to_string(), child);
                Ok(pid)
            }
            Err(e) => {
                error!("Failed to start process '{}': {}", name, e);
                Err(format!("Failed to start: {}", e))
            }
        }
    }

    /// Stop a specific process
    fn handle_stop(&mut self, name: &str) -> Result<(), String> {
        if let Some(mut child) = self.children.remove(name) {
            let pid = child.id();
            info!("Stopping process '{}' (PID {})", name, pid);

            #[cfg(unix)]
            {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;

                // Send SIGTERM to process group
                let pgid = Pid::from_raw(-(pid as i32));
                if let Err(e) = signal::killpg(pgid, Signal::SIGTERM) {
                    warn!("Failed to send SIGTERM to process group: {}", e);
                    // Fallback: kill individual process
                    let _ = child.kill();
                }
            }

            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }

            // Wait for process to exit
            match child.wait() {
                Ok(status) => {
                    info!("Process '{}' exited with status: {:?}", name, status);
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to wait for process '{}': {}", name, e);
                    Ok(()) // Still return Ok as the process should be gone
                }
            }
        } else {
            Err(format!("Process '{}' not found", name))
        }
    }

    /// Register an externally-spawned child process
    fn handle_register(&mut self, id: String, child: Child) {
        let pid = child.id();
        info!("Registering child process: {} (PID: {})", id, pid);
        self.children.insert(id, child);
    }

    /// Unregister and kill a child process
    fn handle_unregister(&mut self, id: &str) {
        if let Some(mut child) = self.children.remove(id) {
            info!("Unregistering and killing child process: {}", id);

            if let Err(e) = child.kill() {
                warn!("Failed to kill process {}: {}", id, e);
            }

            if let Err(e) = child.wait() {
                warn!("Failed to wait for process {}: {}", id, e);
            }
        } else {
            warn!("Attempted to unregister unknown process: {}", id);
        }
    }

    /// Graceful shutdown: SIGTERM -> wait -> SIGKILL
    async fn graceful_shutdown(&mut self) {
        let count = self.children.len();
        if count == 0 {
            info!("No child processes to shutdown");
            return;
        }

        info!("Shutting down {} child process(es)...", count);

        #[cfg(unix)]
        {
            use nix::sys::signal::{self, Signal};
            use nix::unistd::Pid;

            // Phase 1: Send SIGTERM to all process groups
            for (id, child) in self.children.iter() {
                let pid = child.id();
                info!("Sending SIGTERM to process group '{}' (PID {})", id, pid);

                let pgid = Pid::from_raw(-(pid as i32));
                if let Err(e) = signal::killpg(pgid, Signal::SIGTERM) {
                    warn!("Failed to send SIGTERM to '{}': {}", id, e);
                }
            }

            // Phase 2: Wait for graceful exit (with timeout)
            let timeout = std::time::Duration::from_secs(self.shutdown_timeout_secs);
            let start = std::time::Instant::now();

            while !self.children.is_empty() && start.elapsed() < timeout {
                // Try to reap exited processes
                self.children.retain(|id, child| {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            info!("Process '{}' exited gracefully: {:?}", id, status);
                            false // Remove from map
                        }
                        Ok(None) => true, // Still running
                        Err(e) => {
                            error!("Error checking process '{}': {}", id, e);
                            false
                        }
                    }
                });

                if !self.children.is_empty() {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }

            // Phase 3: Force kill any remaining processes
            if !self.children.is_empty() {
                warn!(
                    "{} process(es) did not exit gracefully, sending SIGKILL...",
                    self.children.len()
                );

                for (id, child) in self.children.iter() {
                    let pid = child.id();
                    let pgid = Pid::from_raw(-(pid as i32));
                    if let Err(e) = signal::killpg(pgid, Signal::SIGKILL) {
                        warn!("Failed to send SIGKILL to '{}': {}", id, e);
                    }
                }

                // Final reap
                for (id, mut child) in self.children.drain() {
                    match child.wait() {
                        Ok(status) => info!("Process '{}' terminated: {:?}", id, status),
                        Err(e) => error!("Failed to reap process '{}': {}", id, e),
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            // Windows: Direct kill (no process groups)
            for (id, mut child) in self.children.drain() {
                info!("Killing child process: {}", id);
                let _ = child.kill();
                let _ = child.wait();
            }
        }

        info!("All child processes terminated");
    }

    /// Get status of all managed processes
    fn handle_get_status(&self, response_tx: oneshot::Sender<HashMap<String, ProcessStatus>>) {
        let mut status_map = HashMap::new();

        for (id, child) in &self.children {
            let status = ProcessStatus {
                id: id.clone(),
                pid: Some(child.id()),
                state: ProcessState::Running,
            };
            status_map.insert(id.clone(), status);
        }

        let _ = response_tx.send(status_map);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ProcessSupervisor - Public Handle (Clone-able)
// ═══════════════════════════════════════════════════════════════════════════

/// Public handle to the ProcessSupervisor actor.
///
/// This is a lightweight, Clone-able handle that can be passed around
/// to interact with the actor via message passing.
#[derive(Clone, Debug)]
pub struct ProcessSupervisor {
    /// Channel to send messages to the actor
    sender: mpsc::UnboundedSender<SupervisorMessage>,
}

impl ProcessSupervisor {
    /// Create a new ProcessSupervisor and spawn the actor task.
    ///
    /// The actor will run in the background, handling messages and OS signals.
    /// Returns a handle that can be cloned and used to interact with the actor.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();

        let actor = SupervisorActor::new(receiver);

        // Spawn the actor on the Tokio runtime
        tokio::spawn(async move {
            actor.run().await;
        });

        Self { sender }
    }

    /// Start a new process managed by the supervisor
    pub async fn start_process(
        &self,
        name: &str,
        command: &str,
        args: Vec<String>,
        envs: Vec<(String, String)>,
        working_dir: Option<std::path::PathBuf>,
    ) -> Result<u32> {
        self.start_process_with_sandbox(name, command, args, envs, working_dir, None)
            .await
    }

    /// Start a new process with sandbox isolation
    pub async fn start_process_with_sandbox(
        &self,
        name: &str,
        command: &str,
        args: Vec<String>,
        envs: Vec<(String, String)>,
        working_dir: Option<std::path::PathBuf>,
        sandbox_policy: Option<SandboxPolicy>,
    ) -> Result<u32> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.sender
            .send(SupervisorMessage::Start {
                name: name.to_string(),
                command: command.to_string(),
                args,
                envs,
                working_dir,
                sandbox_policy,
                resp: resp_tx,
            })
            .map_err(|e| anyhow::anyhow!("Failed to send start message: {}", e))?;

        resp_rx
            .await
            .map_err(|e| anyhow::anyhow!("Failed to receive response: {}", e))?
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Stop a specific process by name
    pub async fn stop_process(&self, name: &str) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.sender
            .send(SupervisorMessage::Stop {
                name: name.to_string(),
                resp: resp_tx,
            })
            .map_err(|e| anyhow::anyhow!("Failed to send stop message: {}", e))?;

        resp_rx
            .await
            .map_err(|e| anyhow::anyhow!("Failed to receive response: {}", e))?
            .map_err(|e| anyhow::anyhow!("{}", e))
    }

    /// Register an externally-spawned child process
    pub fn register(&self, id: String, child: Child) -> Result<()> {
        self.sender
            .send(SupervisorMessage::Register { id, child })
            .map_err(|e| anyhow::anyhow!("Failed to send register message: {}", e))
    }

    /// Unregister and terminate a child process
    pub fn unregister(&self, id: String) -> Result<()> {
        self.sender
            .send(SupervisorMessage::Unregister { id })
            .map_err(|e| anyhow::anyhow!("Failed to send unregister message: {}", e))
    }

    /// Initiate graceful shutdown and wait for completion
    pub async fn shutdown_and_wait(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();

        self.sender
            .send(SupervisorMessage::Shutdown { resp: resp_tx })
            .map_err(|e| anyhow::anyhow!("Failed to send shutdown message: {}", e))?;

        resp_rx
            .await
            .map_err(|e| anyhow::anyhow!("Failed to receive shutdown confirmation: {}", e))
    }

    /// Initiate shutdown without waiting (fire-and-forget)
    pub fn shutdown(&self) -> Result<()> {
        let (resp_tx, _) = oneshot::channel();

        self.sender
            .send(SupervisorMessage::Shutdown { resp: resp_tx })
            .map_err(|e| anyhow::anyhow!("Failed to send shutdown message: {}", e))
    }

    /// Get status of all managed processes
    pub async fn get_status(&self) -> Result<HashMap<String, ProcessStatus>> {
        let (response_tx, response_rx) = oneshot::channel();

        self.sender
            .send(SupervisorMessage::GetStatus { response_tx })
            .map_err(|e| anyhow::anyhow!("Failed to send get_status message: {}", e))?;

        response_rx
            .await
            .map_err(|e| anyhow::anyhow!("Failed to receive status response: {}", e))
    }

    /// Check if the supervisor actor is still alive
    pub fn is_alive(&self) -> bool {
        !self.sender.is_closed()
    }
}

impl Default for ProcessSupervisor {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_supervisor_start_stop() {
        let supervisor = ProcessSupervisor::new();

        // Start a simple sleep process
        let pid = supervisor
            .start_process("test-sleep", "sleep", vec!["10".to_string()], vec![], None)
            .await
            .expect("Failed to start process");

        assert!(pid > 0);

        // Check status
        let status = supervisor.get_status().await.expect("Failed to get status");
        assert!(status.contains_key("test-sleep"));
        assert_eq!(status["test-sleep"].state, ProcessState::Running);

        // Stop the process
        supervisor
            .stop_process("test-sleep")
            .await
            .expect("Failed to stop process");

        // Verify it's gone
        let status = supervisor.get_status().await.expect("Failed to get status");
        assert!(!status.contains_key("test-sleep"));

        // Shutdown
        supervisor
            .shutdown_and_wait()
            .await
            .expect("Failed to shutdown");
    }

    #[tokio::test]
    async fn test_supervisor_shutdown_all() {
        let supervisor = ProcessSupervisor::new();

        // Start multiple processes
        for i in 0..3 {
            supervisor
                .start_process(
                    &format!("test-{}", i),
                    "sleep",
                    vec!["60".to_string()],
                    vec![],
                    None,
                )
                .await
                .expect("Failed to start process");
        }

        // Verify all running
        let status = supervisor.get_status().await.expect("Failed to get status");
        assert_eq!(status.len(), 3);

        // Shutdown all
        supervisor
            .shutdown_and_wait()
            .await
            .expect("Failed to shutdown");

        // Supervisor should be dead
        assert!(!supervisor.is_alive());
    }

    #[tokio::test]
    async fn test_supervisor_with_sandbox_policy() {
        let supervisor = ProcessSupervisor::new();

        // Create a sandbox policy for the current directory
        let sandbox_policy = SandboxPolicy::for_capsule(std::env::current_dir().unwrap());

        // Start a process with sandbox (on macOS, this will use Seatbelt)
        // On Linux, it would use Landlock
        let pid = supervisor
            .start_process_with_sandbox(
                "sandboxed-echo",
                "echo",
                vec!["Hello from sandbox".to_string()],
                vec![],
                None,
                Some(sandbox_policy),
            )
            .await
            .expect("Failed to start sandboxed process");

        assert!(pid > 0);

        // Give the process time to complete
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Shutdown
        supervisor
            .shutdown_and_wait()
            .await
            .expect("Failed to shutdown");
    }
}
