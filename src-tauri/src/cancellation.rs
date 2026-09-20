//! Shared cancellation tokens for long-running commands.
//!
//! The frontend can cancel a streaming generation by calling `cancel_request`
//! with the request id. The running command periodically checks `is_cancelled`
//! and aborts early.
//!
//! Memory-safety design: two sets protected by one lock.
//! - `active`: request ids that have a live `CancelGuard`. Inserted by the
//!   guard's constructor, removed by its `drop`.
//! - `cancelled`: request ids that the frontend asked to cancel.
//!
//! `cancel_request` only inserts into `cancelled` if the id is in `active`.
//! This prevents leaks from stray cancels for never-started or
//! already-finished requests — the entry would otherwise live forever.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::OnceLock;

struct CancellationState {
    active: HashSet<String>,
    cancelled: HashSet<String>,
}

static STATE: OnceLock<Mutex<CancellationState>> = OnceLock::new();

fn state() -> &'static Mutex<CancellationState> {
    STATE.get_or_init(|| {
        Mutex::new(CancellationState {
            active: HashSet::new(),
            cancelled: HashSet::new(),
        })
    })
}

/// Mark a request as cancelled. Idempotent — safe to call multiple times.
/// No-op if no request with that id is currently active.
pub fn cancel_request(id: &str) {
    if let Ok(mut s) = state().lock() {
        if s.active.contains(id) {
            s.cancelled.insert(id.to_string());
        }
    }
}

/// Check whether a request has been cancelled.
pub fn is_cancelled(id: &str) -> bool {
    state()
        .lock()
        .ok()
        .map(|s| s.cancelled.contains(id))
        .unwrap_or(false)
}

/// Whether a live command currently owns this request id. Callers that expose a
/// cancel button use it to avoid reporting success for a run that never started.
pub fn is_active(id: &str) -> bool {
    state()
        .lock()
        .ok()
        .map(|s| s.active.contains(id))
        .unwrap_or(false)
}

/// Register a request as active and report whether it was pre-cancelled.
fn register_active(id: &str) -> bool {
    if let Ok(mut s) = state().lock() {
        let pre_cancelled = s.cancelled.contains(id);
        s.active.insert(id.to_string());
        pre_cancelled
    } else {
        false
    }
}

/// Clear a request's active + cancelled tokens (call when a request completes).
fn clear_request(id: &str) {
    if let Ok(mut s) = state().lock() {
        s.active.remove(id);
        s.cancelled.remove(id);
    }
}

/// Guard that registers a request as active and clears it when dropped.
pub struct CancelGuard<'a> {
    id: &'a str,
}

impl<'a> CancelGuard<'a> {
    pub fn new(id: &'a str) -> Self {
        register_active(id);
        Self { id }
    }
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        clear_request(self.id);
    }
}
