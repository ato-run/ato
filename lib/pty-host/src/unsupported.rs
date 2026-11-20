use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

/// A PTY session cannot be created on this unsupported host.
pub enum PtySession {}

impl PtySession {
    /// Refuse before launching a process when no host PTY implementation exists.
    pub fn spawn(
        _command: &[String],
        _environment: &BTreeMap<String, String>,
        _cwd: &Path,
        _cols: u16,
        _rows: u16,
    ) -> Result<Self, String> {
        Err("pty_unsupported_platform: this host has no supported PTY implementation".to_owned())
    }

    /// Return the process id of an existing session (none can exist here).
    pub fn pid(&self) -> u32 {
        match *self {}
    }

    /// Write to an existing session (none can exist here).
    pub fn write(&mut self, _bytes: &[u8]) -> Result<(), String> {
        match *self {}
    }

    /// Read an existing session (none can exist here).
    pub fn read_until(
        &mut self,
        _captured: &mut Vec<u8>,
        _bound: usize,
        _deadline: Instant,
        _done: &mut dyn FnMut(&[u8]) -> bool,
    ) -> Result<bool, String> {
        match *self {}
    }

    /// Observe an existing session (none can exist here).
    pub fn exited(&mut self) -> Option<String> {
        match *self {}
    }

    /// Stop an existing session (none can exist here).
    pub fn stop(&mut self) -> Result<(), String> {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_host_refuses_process_creation() {
        let result = PtySession::spawn(
            &["must-not-execute".to_owned()],
            &BTreeMap::new(),
            Path::new("."),
            80,
            24,
        );
        assert!(matches!(result, Err(reason) if reason.starts_with("pty_unsupported_platform:")));
    }
}
