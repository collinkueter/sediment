//! Per-turn cancellation registry — the interrupt/steer/redirect path.
//!
//! `chat_turn` runs a turn behind a streaming Tauri command; to interrupt one
//! in flight the UI calls the separate `cancel_turn` command. The two rendezvous
//! here. A turn registers under the frontend's *client turn id* (the only handle
//! the UI has before the turn completes and returns the real audit id) and gets a
//! [`CancellationToken`]; `cancel_turn` trips it and records the requested
//! [`CancelMode`], so `chat_turn` knows whether to **keep** (Steer) or **revert**
//! (Redirect) the interrupted turn's partial work.
//!
//! The engine itself never sees the mode — it only watches the token and stops.
//! The keep-vs-revert decision is `chat_turn`'s alone.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// What the user asked for when interrupting a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CancelMode {
    /// Stop the turn but **keep** its partial work and the conversation — a
    /// nudge. The interrupted turn is committed like a normal turn.
    Steer,
    /// Stop the turn and **revert** its partial work — change direction. The
    /// interrupted turn leaves no audit entry, as if it never ran.
    Redirect,
}

/// One in-flight turn's cancellation state.
struct Handle {
    token: CancellationToken,
    /// Set by `cancel` to the mode the user chose; read by `chat_turn` after the
    /// engine stops.
    mode: Mutex<Option<CancelMode>>,
}

/// Tauri-managed state: client-turn-id → live turn handle. Entries live only for
/// the duration of a turn (registered at the top of `chat_turn`, removed by its
/// drop-guard on every exit path).
#[derive(Default)]
pub struct CancelRegistry {
    inner: Mutex<HashMap<String, Arc<Handle>>>,
}

impl CancelRegistry {
    /// Register a turn about to run; returns its cancellation token (cloned into
    /// the `TurnRequest`).
    pub fn register(&self, client_turn_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let handle = Arc::new(Handle {
            token: token.clone(),
            mode: Mutex::new(None),
        });
        self.inner
            .lock()
            .expect("cancel registry poisoned")
            .insert(client_turn_id.to_string(), handle);
        token
    }

    /// Trip the turn's token and record the requested mode. A no-op if the id is
    /// unknown — the turn already finished, a benign finish-before-cancel race.
    pub fn cancel(&self, client_turn_id: &str, mode: CancelMode) {
        if let Some(handle) = self
            .inner
            .lock()
            .expect("cancel registry poisoned")
            .get(client_turn_id)
        {
            *handle.mode.lock().expect("cancel mode poisoned") = Some(mode);
            handle.token.cancel();
        }
    }

    /// The mode a cancel recorded for this turn, if it was cancelled.
    pub fn taken_mode(&self, client_turn_id: &str) -> Option<CancelMode> {
        self.inner
            .lock()
            .expect("cancel registry poisoned")
            .get(client_turn_id)
            .and_then(|h| *h.mode.lock().expect("cancel mode poisoned"))
    }

    /// Deregister a finished turn. Idempotent.
    pub fn finish(&self, client_turn_id: &str) {
        self.inner
            .lock()
            .expect("cancel registry poisoned")
            .remove(client_turn_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_records_mode_and_trips_token() {
        let reg = CancelRegistry::default();
        let token = reg.register("t1");
        assert!(!token.is_cancelled());
        assert_eq!(reg.taken_mode("t1"), None);

        reg.cancel("t1", CancelMode::Redirect);
        assert!(token.is_cancelled());
        assert_eq!(reg.taken_mode("t1"), Some(CancelMode::Redirect));
    }

    #[test]
    fn cancel_unknown_id_is_a_noop() {
        let reg = CancelRegistry::default();
        reg.cancel("ghost", CancelMode::Steer); // must not panic
        assert_eq!(reg.taken_mode("ghost"), None);
    }

    #[test]
    fn finish_removes_the_handle() {
        let reg = CancelRegistry::default();
        let token = reg.register("t1");
        reg.finish("t1");
        // After finish, a late cancel can't reach the (gone) turn.
        reg.cancel("t1", CancelMode::Steer);
        assert!(!token.is_cancelled());
        assert_eq!(reg.taken_mode("t1"), None);
        reg.finish("t1"); // idempotent
    }

    #[test]
    fn mode_deserializes_from_lowercase() {
        let s: CancelMode = serde_json::from_str("\"steer\"").unwrap();
        assert_eq!(s, CancelMode::Steer);
        let r: CancelMode = serde_json::from_str("\"redirect\"").unwrap();
        assert_eq!(r, CancelMode::Redirect);
    }
}
