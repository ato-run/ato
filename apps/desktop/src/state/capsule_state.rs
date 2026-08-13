//! Session-scoped capsule key-value state store.
//!
//! `CapsuleStateStore` backs the `capsule.state.get` / `capsule.state.set`
//! IPC commands.  State is ephemeral: each entry lives only as long as its
//! session.  When `clear_session` is called (triggered by session teardown)
//! all key-value pairs for that session are dropped.
//!
//! # Key scheme
//!
//! ```text
//! session_id
//!   └─ capsule_instance_key   (e.g. execution_id from IpcPrincipal::Capsule)
//!        └─ state_key → serde_json::Value
//! ```
//!
//! Two capsule instances with different `capsule_instance_key`s (even if they
//! share the same capsule handle) cannot read each other's state.  Two
//! sessions are always isolated regardless of `capsule_instance_key`.

use std::collections::HashMap;

use serde_json::Value;
/// Ephemeral, session-scoped key-value store for capsule state.
///
/// Register as a GPUI global at startup with `cx.set_global(CapsuleStateStore::default())`.
/// Call [`clear_session`] from the session teardown path so memory is reclaimed.
///
/// The `gpui::Global` impl lives in `window/mod.rs` to keep this module free of
/// UI-framework imports.
///
/// [`clear_session`]: CapsuleStateStore::clear_session
#[derive(Debug, Default)]
pub struct CapsuleStateStore {
    /// session_id → capsule_instance_key → (state_key → value)
    sessions: HashMap<String, HashMap<String, HashMap<String, Value>>>,
}

impl CapsuleStateStore {
    /// Read a single value.
    ///
    /// Returns `None` if the session, capsule instance, or key does not exist.
    pub fn get(&self, session_id: &str, capsule_instance_key: &str, key: &str) -> Option<&Value> {
        self.sessions
            .get(session_id)?
            .get(capsule_instance_key)?
            .get(key)
    }

    /// Write a single value, creating intermediate maps as needed.
    pub fn set(
        &mut self,
        session_id: impl Into<String>,
        capsule_instance_key: impl Into<String>,
        key: impl Into<String>,
        value: Value,
    ) {
        self.sessions
            .entry(session_id.into())
            .or_default()
            .entry(capsule_instance_key.into())
            .or_default()
            .insert(key.into(), value);
    }

    /// Drop all state for a session.  Call this during session teardown.
    ///
    /// O(1) — a single HashMap removal regardless of how many keys were stored.
    pub fn clear_session(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
    }

    /// Number of sessions with live state (useful for tests).
    #[cfg(test)]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Number of key-value pairs for a specific (session, capsule instance) pair (useful for tests).
    #[cfg(test)]
    pub fn entry_count(&self, session_id: &str, capsule_instance_key: &str) -> usize {
        self.sessions
            .get(session_id)
            .and_then(|s| s.get(capsule_instance_key))
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_returns_none_for_missing_session() {
        let store = CapsuleStateStore::default();
        assert!(store.get("s1", "cap1", "key").is_none());
    }

    #[test]
    fn set_and_get_roundtrip() {
        let mut store = CapsuleStateStore::default();
        store.set("s1", "cap1", "foo", json!(42));
        assert_eq!(store.get("s1", "cap1", "foo"), Some(&json!(42)));
    }

    #[test]
    fn different_sessions_are_isolated() {
        let mut store = CapsuleStateStore::default();
        store.set("s1", "cap1", "key", json!("session1"));
        store.set("s2", "cap1", "key", json!("session2"));
        assert_eq!(store.get("s1", "cap1", "key"), Some(&json!("session1")));
        assert_eq!(store.get("s2", "cap1", "key"), Some(&json!("session2")));
    }

    #[test]
    fn different_capsule_instances_are_isolated() {
        let mut store = CapsuleStateStore::default();
        store.set("s1", "exec-a", "key", json!("a"));
        store.set("s1", "exec-b", "key", json!("b"));
        assert_eq!(store.get("s1", "exec-a", "key"), Some(&json!("a")));
        assert_eq!(store.get("s1", "exec-b", "key"), Some(&json!("b")));
        // exec-a cannot see exec-b's value
        assert_eq!(store.get("s1", "exec-a", "other"), None);
    }

    #[test]
    fn clear_session_removes_all_state() {
        let mut store = CapsuleStateStore::default();
        store.set("s1", "cap1", "k1", json!(1));
        store.set("s1", "cap1", "k2", json!(2));
        store.set("s2", "cap1", "k1", json!(3));
        store.clear_session("s1");
        assert_eq!(store.session_count(), 1); // s2 still alive
        assert!(store.get("s1", "cap1", "k1").is_none());
        assert_eq!(store.get("s2", "cap1", "k1"), Some(&json!(3)));
    }

    #[test]
    fn overwrite_existing_key() {
        let mut store = CapsuleStateStore::default();
        store.set("s1", "cap1", "k", json!("first"));
        store.set("s1", "cap1", "k", json!("second"));
        assert_eq!(store.get("s1", "cap1", "k"), Some(&json!("second")));
    }

    #[test]
    fn entry_count_reflects_keys() {
        let mut store = CapsuleStateStore::default();
        store.set("s1", "cap1", "a", json!(1));
        store.set("s1", "cap1", "b", json!(2));
        assert_eq!(store.entry_count("s1", "cap1"), 2);
    }
}
