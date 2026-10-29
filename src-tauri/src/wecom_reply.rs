//! Per-message reply finalization; no transport or history side effects.
//!
//! The caller supplies an opaque, request-scoped stream ID (including msgid),
//! registers it before queueing, then prepares exactly one final payload. A
//! cancellation/error is a final payload too: first finalization wins. Retry the
//! returned `Prepared`, not `prepare`, so retries neither retime nor rewrite it.
//!
//! Retention is bounded, not permanent deduplication: at most 4096 states and
//! 2048 finished tombstones, in completion order. Text snapshots have a separate
//! 4 MiB UTF-8 byte budget (stricter than a 4M-character budget). Evicting text
//! does not reopen a finished stream. History should consume `final_text` soon;
//! an oversized payload is returned intact but is not retained as a snapshot.
//! Active states never undergo LRU eviction. After two hours they become closed
//! tombstones on the next mutation. IDs must not be reused: after a tombstone is
//! evicted an unknown ID may finalize anew, with an explicitly zero duration.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_STATES: usize = 4096;
const MAX_FINISHED: usize = 2048;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const ACTIVE_TTL: Duration = Duration::from_secs(2 * 60 * 60);

/// An immutable first-finalization payload. Keep this value for transport retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub text: String,
    pub elapsed_ms: u64,
}

#[derive(Debug)]
enum State {
    Running { started: Instant },
    Finished { text: Option<String> },
}

/// Deterministic state machine: callers explicitly supply a monotonic clock.
#[derive(Debug, Default)]
pub struct ReplyRegistry {
    states: HashMap<String, State>,
    finished: VecDeque<String>,
    text_bytes: usize,
}

impl ReplyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// True for a newly registered or already-running stream; false for closed
    /// streams or capacity exhaustion. Duplicate registration never resets time.
    pub fn begin_at(&mut self, stream: &str, now: Instant) -> bool {
        let existed = self.states.contains_key(stream);
        self.expire_at(now);
        if let Some(state) = self.states.get(stream) {
            return matches!(state, State::Running { .. });
        }
        // Expiration may immediately evict this ID when many states expire.
        // Do not accidentally restart it in the same operation.
        if existed || !self.make_room() {
            return false;
        }
        self.states
            .insert(stream.to_owned(), State::Running { started: now });
        true
    }

    /// Capture the elapsed time and final body atomically in one state transition.
    /// Missing registration means 0 ms, never a guessed start time.
    pub fn prepare_at(&mut self, stream: &str, body: &str, now: Instant) -> Option<Prepared> {
        let closed = match self.states.get(stream) {
            Some(State::Finished { .. }) => true,
            Some(State::Running { started }) => {
                now.saturating_duration_since(*started) >= ACTIVE_TTL
            }
            None => false,
        };
        self.expire_at(now);
        if closed {
            return None;
        }
        let elapsed_ms = match self.states.get(stream) {
            Some(State::Running { started }) => now
                .saturating_duration_since(*started)
                .as_millis()
                .min(u64::MAX as u128) as u64,
            Some(State::Finished { .. }) => return None,
            None => {
                if !self.make_room() {
                    return None;
                }
                0
            }
        };
        let text = format!("{body}\n\n⏱ 本次耗时：{}", elapsed_label(elapsed_ms));
        // Large bodies are still emitted in full. Keep a tombstone without a
        // snapshot instead of exceeding the budget or caching a truncated lie.
        let snapshot = (text.len() <= MAX_TEXT_BYTES).then(|| text.clone());
        self.close(stream.to_owned(), snapshot);
        Some(Prepared { text, elapsed_ms })
    }

    pub fn finish_at(&mut self, stream: &str, body: &str, now: Instant) -> Option<String> {
        self.prepare_at(stream, body, now).map(|reply| reply.text)
    }

    /// The exact finalized payload, including timing, while its snapshot remains.
    /// This is read-only: reads never extend retention or let model output replace it.
    pub fn final_text(&self, stream: &str) -> Option<String> {
        match self.states.get(stream) {
            Some(State::Finished { text }) => text.clone(),
            _ => None,
        }
    }

    pub fn is_finished(&self, stream: &str) -> bool {
        matches!(self.states.get(stream), Some(State::Finished { .. }))
    }

    fn close(&mut self, stream: String, text: Option<String>) {
        self.text_bytes += text.as_ref().map_or(0, String::len);
        self.states.insert(stream.clone(), State::Finished { text });
        self.finished.push_back(stream);
        while self.finished.len() > MAX_FINISHED {
            self.evict_finished();
        }
        // Text eviction is deliberately separate from tombstone eviction.
        for key in &self.finished {
            if self.text_bytes <= MAX_TEXT_BYTES {
                break;
            }
            if let Some(State::Finished { text }) = self.states.get_mut(key) {
                if let Some(old) = text.take() {
                    self.text_bytes -= old.len();
                }
            }
        }
    }

    fn evict_finished(&mut self) -> bool {
        let Some(old) = self.finished.pop_front() else {
            return false;
        };
        if let Some(State::Finished { text }) = self.states.remove(&old) {
            self.text_bytes -= text.as_ref().map_or(0, String::len);
        }
        true
    }

    fn make_room(&mut self) -> bool {
        while self.states.len() >= MAX_STATES {
            if !self.evict_finished() {
                return false;
            }
        }
        true
    }

    fn expire_at(&mut self, now: Instant) {
        let mut expired: Vec<_> = self
            .states
            .iter()
            .filter_map(|(stream, state)| match state {
                State::Running { started }
                    if now.saturating_duration_since(*started) >= ACTIVE_TTL =>
                {
                    Some((*started, stream.clone()))
                }
                _ => None,
            })
            .collect();
        // HashMap iteration order must not determine which tombstones survive.
        expired.sort_unstable();
        for (_, stream) in expired {
            self.close(stream, None);
        }
    }
}

/// Whole seconds, rounded down. Keep zero components for unambiguous boundaries.
pub fn elapsed_label(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{seconds}秒")
    } else if seconds < 3600 {
        format!("{}分{}秒", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}时{}分{}秒",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}

#[derive(Default)]
struct SharedRegistry(Mutex<ReplyRegistry>);

impl SharedRegistry {
    fn register(&self, stream: &str) -> bool {
        self.0
            .lock()
            .map(|mut registry| registry.begin_at(stream, Instant::now()))
            .unwrap_or(false)
    }

    fn prepare(&self, stream: &str, body: &str) -> Option<Prepared> {
        // Never recover a poisoned map: an interrupted transition may have
        // incomplete accounting. Fail closed rather than send a second final.
        self.0.lock().ok()?.prepare_at(stream, body, Instant::now())
    }

    fn final_text(&self, stream: &str) -> Option<String> {
        self.0.lock().ok()?.final_text(stream)
    }

    fn is_finished(&self, stream: &str) -> bool {
        self.0
            .lock()
            .map(|registry| registry.is_finished(stream))
            .unwrap_or(true)
    }
}

fn global() -> &'static SharedRegistry {
    static REGISTRY: OnceLock<SharedRegistry> = OnceLock::new();
    REGISTRY.get_or_init(SharedRegistry::default)
}

/// Register before queueing a message; use `register` when admission matters.
pub fn begin(stream: &str) {
    let _ = register(stream);
}

/// Admission-aware alias of `begin`: false means closed, full, or poisoned.
pub fn register(stream: &str) -> bool {
    global().register(stream)
}

pub fn prepare(stream: &str, body: &str) -> Option<Prepared> {
    global().prepare(stream, body)
}

pub fn finish(stream: &str, body: &str) -> Option<String> {
    prepare(stream, body).map(|reply| reply.text)
}

pub fn final_text(stream: &str) -> Option<String> {
    global().final_text(stream)
}

/// Poison is treated as closed, also suppressing any late streaming updates.
pub fn is_finished(stream: &str) -> bool {
    global().is_finished(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn elapsed_format_boundaries_and_maximum() {
        for (ms, expected) in [
            (0, "0秒"),
            (999, "0秒"),
            (1000, "1秒"),
            (59_999, "59秒"),
            (60_000, "1分0秒"),
            (61_999, "1分1秒"),
            (3_599_999, "59分59秒"),
            (3_600_000, "1时0分0秒"),
            (3_661_999, "1时1分1秒"),
            (86_400_000, "24时0分0秒"),
            (u64::MAX, "5124095576030时25分51秒"),
        ] {
            assert_eq!(elapsed_label(ms), expected);
        }
    }

    #[test]
    fn duplicate_begin_keeps_queue_time_and_finish_is_immutable() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        assert!(registry.begin_at("req:msg", start));
        assert!(registry.begin_at("req:msg", start + Duration::from_secs(20)));
        let prepared = registry
            .prepare_at("req:msg", "答复", start + Duration::from_millis(61_234))
            .unwrap();
        assert_eq!(prepared.elapsed_ms, 61_234);
        assert_eq!(prepared.text, "答复\n\n⏱ 本次耗时：1分1秒");
        assert_eq!(registry.final_text("req:msg"), Some(prepared.text.clone()));
        assert!(registry.is_finished("req:msg"));
        assert!(!registry.begin_at("req:msg", start + Duration::from_secs(70)));
        assert_eq!(
            registry.finish_at("req:msg", "迟到", start + Duration::from_secs(80)),
            None
        );
        assert_eq!(registry.final_text("req:msg"), Some(prepared.text));
    }

    #[test]
    fn cancel_first_blocks_stale_model_finish_but_not_next_request() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        registry.begin_at("req1:msg1", start);
        let cancelled = registry.finish_at("req1:msg1", "已停止", start + Duration::from_secs(5));
        registry.begin_at("req2:msg2", start + Duration::from_secs(6));
        assert_eq!(
            registry.finish_at("req1:msg1", "迟到模型", start + Duration::from_secs(7)),
            None
        );
        assert_eq!(registry.final_text("req1:msg1"), cancelled);
        assert_eq!(
            registry.finish_at("req2:msg2", "新回复", start + Duration::from_secs(8)),
            Some("新回复\n\n⏱ 本次耗时：2秒".into())
        );
    }

    #[test]
    fn unknown_and_backwards_clock_have_explicit_zero_duration() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        assert!(!registry.is_finished("missing"));
        assert_eq!(registry.final_text("missing"), None);
        let unknown = registry.prepare_at("missing", "", start).unwrap();
        assert_eq!(unknown.elapsed_ms, 0);
        assert_eq!(unknown.text, "\n\n⏱ 本次耗时：0秒");
        assert_eq!(registry.finish_at("missing", "again", start), None);
        registry.begin_at("future", start + Duration::from_secs(1));
        assert_eq!(
            registry
                .prepare_at("future", "body", start)
                .unwrap()
                .elapsed_ms,
            0
        );
    }

    #[test]
    fn active_survives_finished_churn() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        registry.begin_at("queued", start);
        for i in 0..MAX_FINISHED + 20 {
            assert!(registry
                .finish_at(&format!("done:{i}"), "ok", start)
                .is_some());
        }
        assert_eq!(registry.finished.len(), MAX_FINISHED);
        assert!(matches!(
            registry.states.get("queued"),
            Some(State::Running { .. })
        ));
        assert_eq!(
            registry
                .prepare_at("queued", "ok", start + Duration::from_secs(610))
                .unwrap()
                .elapsed_ms,
            610_000
        );
    }

    #[test]
    fn full_active_map_keeps_all_running_states_and_fails_closed() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        for i in 0..MAX_STATES {
            assert!(registry.begin_at(&format!("active:{i}"), start));
        }
        assert!(!registry.begin_at("overflow", start));
        assert_eq!(registry.finish_at("overflow", "ok", start), None);
        assert_eq!(registry.states.len(), MAX_STATES);
        assert!(registry.begin_at("active:0", start + Duration::from_secs(1)));
        assert_eq!(
            registry
                .prepare_at("active:0", "ok", start + Duration::from_secs(2))
                .unwrap()
                .elapsed_ms,
            2000
        );
        assert!(registry.begin_at("overflow", start + Duration::from_secs(3)));
        assert_eq!(registry.states.len(), MAX_STATES);
        assert!(matches!(
            registry.states.get("active:1"),
            Some(State::Running { .. })
        ));
    }

    #[test]
    fn ttl_is_two_hours_and_expiry_closes_without_fake_snapshot() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        registry.begin_at("stale", start);
        registry.begin_at("still-running", start + Duration::from_secs(1));
        assert!(registry.begin_at("stale", start + ACTIVE_TTL - Duration::from_nanos(1)));
        assert_eq!(
            registry.finish_at("stale", "late", start + ACTIVE_TTL),
            None
        );
        assert!(registry.is_finished("stale"));
        assert_eq!(registry.final_text("stale"), None);
        assert!(!registry.begin_at("stale", start + ACTIVE_TTL));
        assert!(registry
            .finish_at("still-running", "ok", start + ACTIVE_TTL)
            .is_some());
    }

    #[test]
    fn bulk_expiry_does_not_restart_target_evicted_in_same_call() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        for i in 0..MAX_STATES {
            registry.begin_at(&format!("{i:04}"), start);
        }
        assert_eq!(
            registry.finish_at("0000", "stale", start + ACTIVE_TTL),
            None
        );
        assert_eq!(registry.finished.len(), MAX_FINISHED);
        assert!(registry.states.len() <= MAX_STATES);
    }

    #[test]
    fn finished_row_retention_is_completion_order_not_read_lru() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        registry.finish_at("first", "first", start);
        for i in 1..MAX_FINISHED {
            registry.finish_at(&i.to_string(), "ok", start);
        }
        assert!(registry.final_text("first").is_some());
        registry.finish_at("latest", "ok", start);
        assert!(!registry.is_finished("first"));
        assert_eq!(registry.final_text("first"), None);
        assert!(registry.is_finished("latest"));
        assert_eq!(registry.finished.len(), MAX_FINISHED);
    }

    #[test]
    fn text_budget_evicts_snapshots_not_tombstones() {
        let mut registry = ReplyRegistry::new();
        let start = Instant::now();
        let body = "字".repeat(MAX_TEXT_BYTES / 6);
        registry.finish_at("old", &body, start);
        let newer = registry.finish_at("new", &body, start).unwrap();
        assert!(registry.text_bytes <= MAX_TEXT_BYTES);
        assert!(registry.is_finished("old"));
        assert_eq!(registry.final_text("old"), None);
        assert_eq!(registry.finish_at("old", "late", start), None);
        assert_eq!(registry.final_text("new"), Some(newer));
        let sum: usize = registry
            .states
            .values()
            .map(|state| match state {
                State::Finished { text } => text.as_ref().map_or(0, String::len),
                _ => 0,
            })
            .sum();
        assert_eq!(registry.text_bytes, sum);
    }

    #[test]
    fn oversized_final_is_emitted_intact_but_not_cached() {
        let mut registry = ReplyRegistry::new();
        let body = "x".repeat(MAX_TEXT_BYTES + 1);
        let start = Instant::now();
        let result = registry.finish_at("large", &body, start).unwrap();
        assert!(result.starts_with(&body));
        assert!(result.ends_with("\n\n⏱ 本次耗时：0秒"));
        assert_eq!(registry.text_bytes, 0);
        assert_eq!(registry.final_text("large"), None);
        assert!(registry.is_finished("large"));
        assert_eq!(registry.finish_at("large", "late", start), None);
    }

    #[test]
    fn concurrent_finalization_has_exactly_one_winner() {
        let shared = Arc::new(SharedRegistry::default());
        assert!(shared.register("race"));
        let barrier = Arc::new(Barrier::new(24));
        let threads: Vec<_> = (0..24)
            .map(|i| {
                let shared = Arc::clone(&shared);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    shared.prepare("race", &format!("winner:{i}"))
                })
            })
            .collect();
        let winners: Vec<_> = threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect();
        assert_eq!(winners.len(), 1);
        assert_eq!(shared.final_text("race"), Some(winners[0].text.clone()));
        assert!(shared.is_finished("race"));
    }

    #[test]
    fn poisoned_mutex_fails_closed_without_recovering_state() {
        let shared = Arc::new(SharedRegistry::default());
        shared.register("poisoned");
        let other = Arc::clone(&shared);
        assert!(thread::spawn(move || {
            let _guard = other.0.lock().unwrap();
            panic!("simulate interrupted transition");
        })
        .join()
        .is_err());
        assert!(!shared.register("new"));
        assert_eq!(shared.prepare("poisoned", "late"), None);
        assert_eq!(shared.prepare("unknown", "late"), None);
        assert_eq!(shared.final_text("poisoned"), None);
        assert!(shared.is_finished("poisoned"));
        assert!(shared.is_finished("unknown"));
        assert!(shared.0.is_poisoned());
    }

    #[test]
    fn public_wrappers_preserve_first_final_and_scope() {
        begin("wrapper:req1:msg1");
        let result = finish("wrapper:req1:msg1", "first").unwrap();
        assert!(result.starts_with("first\n\n⏱ 本次耗时："));
        assert!(is_finished("wrapper:req1:msg1"));
        assert_eq!(final_text("wrapper:req1:msg1"), Some(result));
        assert_eq!(prepare("wrapper:req1:msg1", "late"), None);
        assert!(register("wrapper:req2:msg1"));
        assert!(finish("wrapper:req2:msg1", "second").is_some());
    }
}
