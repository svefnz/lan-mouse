//! Clipboard sync state.
//!
//! Two counters decide everything:
//!
//! - `local_seq` increments on every local clipboard change, whether or
//!   not the bytes differ from last time.
//! - `last_pushed[peer]` records the `local_seq` value of the last
//!   successful push to that peer.
//!
//! A transfer happens only when the two differ, so moving the cursor to
//! another machine without copying anything never overwrites that
//! machine's clipboard. Using a counter rather than a content hash is
//! deliberate: re-copying byte-identical content must transfer again,
//! which a hash cannot express.

use std::collections::HashMap;

use input_clipboard::Snapshot;
use lan_mouse_ipc::ClientHandle;
use sha2::{Digest, Sha256};

/// Content fingerprint of a snapshot, used to recognize the echo of a
/// write we just applied.
pub fn hash(snapshot: &Snapshot) -> u64 {
    let mut hasher = Sha256::new();
    for item in &snapshot.items {
        hasher.update((item.name.len() as u32).to_be_bytes());
        hasher.update(item.name.as_bytes());
        hasher.update((item.bytes.len() as u32).to_be_bytes());
        hasher.update(&item.bytes);
    }
    let digest = hasher.finalize();
    u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
}

#[derive(Debug, Default)]
pub struct SyncState {
    local_seq: u64,
    last_pushed: HashMap<ClientHandle, u64>,
    /// fingerprint of the content we last applied from a peer; the
    /// matching local change event is the echo of our own write
    expected_echo: Option<u64>,
    /// origin sequence of the frame we last pushed; a peer that echoes it
    /// back is bouncing our own content, which must not be re-applied
    last_sent: Option<u64>,
}

impl SyncState {
    /// Record a local clipboard change and return its sequence number.
    pub fn on_local_change(&mut self) -> u64 {
        self.local_seq += 1;
        self.local_seq
    }

    /// Whether the current local clipboard should be pushed to `handle`.
    pub fn should_push(&self, handle: ClientHandle) -> bool {
        self.last_pushed.get(&handle).copied().unwrap_or(0) != self.local_seq
    }

    /// Record a successful push to `handle` of the given sequence.
    ///
    /// The sequence is the one that was actually sent, not the current
    /// one: a change made while the transfer was in flight still needs
    /// to be pushed on the next entry.
    pub fn on_pushed(&mut self, handle: ClientHandle, seq: u64) {
        self.last_pushed.insert(handle, seq);
        self.last_sent = Some(seq);
    }

    /// Whether an incoming frame carries our own last push back to us.
    pub fn is_own_echo(&self, origin_seq: u64) -> bool {
        self.last_sent == Some(origin_seq)
    }

    /// Forget a peer, e.g. when it is removed from the configuration.
    pub fn forget(&mut self, handle: ClientHandle) {
        self.last_pushed.remove(&handle);
    }

    /// Remember the content we are about to write, so its change event
    /// is recognized as our own echo instead of a new local copy.
    pub fn expect_echo(&mut self, hash: u64) {
        self.expected_echo = Some(hash);
    }

    /// Consume the pending echo expectation.
    ///
    /// Returns `true` when `hash` matches the content we just wrote. The
    /// expectation is one-shot: a later copy of identical content is a
    /// genuine change and must propagate.
    pub fn take_echo(&mut self, hash: u64) -> bool {
        if self.expected_echo == Some(hash) {
            self.expected_echo = None;
            true
        } else {
            false
        }
    }

    pub fn local_seq(&self) -> u64 {
        self.local_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use input_clipboard::Representation;

    fn snapshot(text: &str) -> Snapshot {
        Snapshot {
            items: vec![Representation::new("text/plain", text.as_bytes().to_vec())],
            dropped: Vec::new(),
        }
    }

    #[test]
    fn copy_then_enter_transfers() {
        let mut state = SyncState::default();
        state.on_local_change();
        assert!(state.should_push(1));
    }

    #[test]
    fn enter_without_change_does_not_transfer() {
        let mut state = SyncState::default();
        state.on_local_change();
        assert!(state.should_push(1));
        state.on_pushed(1, 1);

        // cursor moves back and forth, nothing copied
        assert!(!state.should_push(1));
        assert!(!state.should_push(1));
    }

    #[test]
    fn repeated_crossings_do_not_transfer_again() {
        let mut state = SyncState::default();
        state.on_local_change();
        state.on_pushed(1, 1);
        for _ in 0..10 {
            assert!(!state.should_push(1));
        }
    }

    #[test]
    fn copying_identical_content_transfers_again() {
        let mut state = SyncState::default();
        state.on_local_change();
        state.on_pushed(1, 1);
        assert!(!state.should_push(1));

        // the bytes are the same, but the clipboard changed
        state.on_local_change();
        assert!(state.should_push(1));
    }

    #[test]
    fn peers_are_tracked_independently() {
        let mut state = SyncState::default();
        state.on_local_change();
        state.on_pushed(1, 1);

        assert!(!state.should_push(1));
        assert!(state.should_push(2));

        state.on_pushed(2, 1);
        assert!(!state.should_push(2));
    }

    #[test]
    fn applied_remote_content_is_not_pushed_back() {
        let mut state = SyncState::default();
        let incoming = snapshot("from the peer");
        state.expect_echo(hash(&incoming));

        // the watcher fires because we just wrote the clipboard
        assert!(state.take_echo(hash(&incoming)));
        // the expectation was consumed, so nothing was recorded as local
        assert_eq!(state.local_seq(), 0);
    }

    #[test]
    fn a_later_copy_of_the_same_content_is_not_swallowed() {
        let mut state = SyncState::default();
        let content = snapshot("same bytes");
        state.expect_echo(hash(&content));
        assert!(state.take_echo(hash(&content)));

        // the user copies the same text again
        state.on_local_change();
        assert!(!state.take_echo(hash(&content)));
        assert!(state.should_push(1));
    }

    #[test]
    fn unrelated_local_changes_are_not_treated_as_echoes() {
        let mut state = SyncState::default();
        state.expect_echo(hash(&snapshot("remote")));
        assert!(!state.take_echo(hash(&snapshot("local"))));
    }

    #[test]
    fn a_pushed_frame_echoed_back_is_recognized() {
        let mut state = SyncState::default();
        state.on_local_change();
        state.on_pushed(1, 7);
        assert!(state.is_own_echo(7));
        assert!(!state.is_own_echo(8));
    }

    #[test]
    fn a_change_made_during_a_push_is_pushed_next_time() {
        let mut state = SyncState::default();
        state.on_local_change(); // seq 1, sent
        state.on_pushed(1, 1);
        state.on_local_change(); // seq 2, arrived mid-flight
        assert!(state.should_push(1));
    }

    #[test]
    fn hash_distinguishes_name_and_bytes() {
        let a = Snapshot {
            items: vec![Representation::new("text/plain", b"ab".to_vec())],
            dropped: Vec::new(),
        };
        let b = Snapshot {
            items: vec![Representation::new("text/plaina", b"b".to_vec())],
            dropped: Vec::new(),
        };
        assert_ne!(hash(&a), hash(&b));
    }
}
