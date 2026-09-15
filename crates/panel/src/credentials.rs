//! Short-lived, server-side stash for credentials a multi-step form needs.
//!
//! The import wizard collects an SSH password or private key on step one and
//! needs it again on step three. Round-tripping it through hidden form fields
//! puts it in the DOM, in the browser's history and in any proxy log, so instead
//! the value stays in the panel's memory and the browser only ever sees an
//! opaque handle.
//!
//! Entries are bound to the user who created them, expire after
//! [`STASH_TTL`], and are consumed on use.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a wizard may take before its credentials are discarded.
pub const STASH_TTL: Duration = Duration::from_secs(15 * 60);

struct Entry {
    user_id: i64,
    value: String,
    stored_at: Instant,
}

#[derive(Default)]
pub struct CredentialStash {
    entries: Mutex<HashMap<String, Entry>>,
}

impl CredentialStash {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `value` for `user_id` and returns the handle to hand to the form.
    pub fn put(&self, user_id: i64, value: impl Into<String>) -> String {
        let handle = crate::auth::random_token();
        let mut entries = self.entries.lock().expect("stash poisoned");
        Self::sweep(&mut entries);
        entries.insert(
            handle.clone(),
            Entry {
                user_id,
                value: value.into(),
                stored_at: Instant::now(),
            },
        );
        handle
    }

    /// Returns the value without consuming it, for a step that may be retried.
    pub fn peek(&self, user_id: i64, handle: &str) -> Option<String> {
        let mut entries = self.entries.lock().expect("stash poisoned");
        Self::sweep(&mut entries);
        entries
            .get(handle)
            .filter(|entry| entry.user_id == user_id)
            .map(|entry| entry.value.clone())
    }

    /// Returns the value and removes it: the wizard is finished with it.
    pub fn take(&self, user_id: i64, handle: &str) -> Option<String> {
        let mut entries = self.entries.lock().expect("stash poisoned");
        Self::sweep(&mut entries);
        match entries.get(handle) {
            Some(entry) if entry.user_id == user_id => entries.remove(handle).map(|e| e.value),
            _ => None,
        }
    }

    pub fn len(&self) -> usize {
        let mut entries = self.entries.lock().expect("stash poisoned");
        Self::sweep(&mut entries);
        entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn sweep(entries: &mut HashMap<String, Entry>) {
        entries.retain(|_, entry| entry.stored_at.elapsed() < STASH_TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_value_for_its_owner() {
        let stash = CredentialStash::new();
        let handle = stash.put(7, "secret");

        assert_eq!(stash.peek(7, &handle).as_deref(), Some("secret"));
        assert_eq!(stash.take(7, &handle).as_deref(), Some("secret"));
        // Consumed.
        assert_eq!(stash.take(7, &handle), None);
    }

    #[test]
    fn another_user_cannot_read_the_handle() {
        let stash = CredentialStash::new();
        let handle = stash.put(7, "secret");

        assert_eq!(stash.peek(8, &handle), None);
        assert_eq!(stash.take(8, &handle), None);
        // Still there for the owner.
        assert_eq!(stash.peek(7, &handle).as_deref(), Some("secret"));
    }

    #[test]
    fn unknown_handles_are_rejected() {
        let stash = CredentialStash::new();
        assert_eq!(stash.peek(7, "nope"), None);
        assert!(stash.is_empty());
    }

    #[test]
    fn handles_are_unguessable_and_unique() {
        let stash = CredentialStash::new();
        let first = stash.put(1, "a");
        let second = stash.put(1, "b");

        assert_ne!(first, second);
        assert!(first.len() >= 32, "handle should carry real entropy");
        assert_eq!(stash.len(), 2);
    }
}
