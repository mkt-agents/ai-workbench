//! Shared admission for the WeCom channel (one instance, cloned by all handlers).
//!
//! Handler order: `queue(key)` before awaiting the session lock; once that lock
//! is held, drop the queue permit, then `try_acquire()`. A full global limit
//! returns busy immediately: this module has no global execution queue.
//! The first request can briefly count as waiting before it holds its session
//! lock; active work itself does not consume the per-session waiting allowance.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub const DEFAULT_LIMIT: usize = 2;
pub const MAX_LIMIT: usize = 32;
pub const MAX_WAITING_PER_SESSION: usize = 8;

#[derive(Debug)]
struct Inner {
    // Serializes configuration changes with the entire admission check + CAS.
    // No guard escapes a synchronous method or spans user work / an await.
    admission: Mutex<()>,
    limit: AtomicUsize,
    active: AtomicUsize,
    waiting: Mutex<Waiting>,
}

#[derive(Debug, Default)]
struct Waiting {
    total: usize,
    by_session: HashMap<String, usize>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // No user callbacks run under these locks. Recover so RAII cleanup also
    // works during unwinding if a thread previously poisoned a lock.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn normalized_limit(limit: usize) -> usize {
    if (1..=MAX_LIMIT).contains(&limit) {
        limit
    } else {
        DEFAULT_LIMIT
    }
}

/// Cloneable handle to one global execution budget and per-session wait counts.
#[derive(Clone, Debug)]
pub struct Admission {
    inner: Arc<Inner>,
}

impl Admission {
    pub fn new(limit: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                admission: Mutex::new(()),
                limit: AtomicUsize::new(normalized_limit(limit)),
                active: AtomicUsize::new(0),
                waiting: Mutex::new(Waiting::default()),
            }),
        }
    }

    /// Accepts 1..=32; every out-of-range value resets the limit to 2.
    /// Lowering the limit never revokes existing permits.
    pub fn set_limit(&self, limit: usize) {
        let _guard = lock(&self.inner.admission);
        self.inner
            .limit
            .store(normalized_limit(limit), Ordering::SeqCst);
    }

    pub fn active(&self) -> usize {
        self.inner.active.load(Ordering::SeqCst)
    }

    pub fn limit(&self) -> usize {
        self.inner.limit.load(Ordering::SeqCst)
    }

    /// Atomically claims one execution slot, or immediately returns `None`.
    /// The successful CAS is the admission linearization point. Holding the
    /// configuration lock keeps its checked limit valid through that CAS;
    /// concurrent permit drops can only decrease active and trigger a retry.
    pub fn try_acquire(&self) -> Option<Permit> {
        let _guard = lock(&self.inner.admission);
        let limit = self.limit();
        let mut active = self.active();
        loop {
            if active >= limit {
                return None;
            }
            match self.inner.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    return Some(Permit {
                        inner: Arc::clone(&self.inner),
                    });
                }
                Err(current) => active = current,
            }
        }
    }

    /// Counts a request waiting for its session lock, independently of active.
    /// At most eight live queue permits may share an exact session key.
    pub fn queue(&self, key: &str) -> Option<QueuePermit> {
        let mut waiting = lock(&self.inner.waiting);
        if waiting.by_session.get(key).copied().unwrap_or(0) >= MAX_WAITING_PER_SESSION {
            return None;
        }
        // Prepare owned data before changing counters.
        let key = key.to_owned();
        let permit_key = key.clone();
        *waiting.by_session.entry(key).or_default() += 1;
        waiting.total += 1;
        Some(QueuePermit {
            inner: Arc::clone(&self.inner),
            key: Some(permit_key),
        })
    }

    /// Total queued requests across all sessions (not active executions).
    pub fn waiting(&self) -> usize {
        lock(&self.inner.waiting).total
    }
}

impl Default for Admission {
    fn default() -> Self {
        Self::new(DEFAULT_LIMIT)
    }
}

/// Owned execution slot. Move it with the task; drop on success/error/cancel.
/// Deliberately not `Clone`: exactly one drop returns exactly one slot.
#[derive(Debug)]
#[must_use = "keep the permit alive for the entire execution"]
pub struct Permit {
    inner: Arc<Inner>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.inner.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Owned waiting slot, released explicitly or by drop exactly once.
#[derive(Debug)]
#[must_use = "keep the queue permit alive while awaiting the session lock"]
pub struct QueuePermit {
    inner: Arc<Inner>,
    key: Option<String>,
}

impl QueuePermit {
    /// Idempotent; subsequent release calls and Drop are no-ops.
    pub fn release(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        let mut waiting = lock(&self.inner.waiting);
        let count = waiting
            .by_session
            .get_mut(&key)
            .expect("live queue permit has a session counter");
        *count -= 1;
        if *count == 0 {
            waiting.by_session.remove(&key);
        }
        waiting.total -= 1;
    }
}

impl Drop for QueuePermit {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, poll_fn, Future};
    use std::pin::Pin;
    use std::sync::Barrier;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread;

    #[test]
    fn defaults_and_every_limit_boundary() {
        assert_eq!(Admission::default().limit(), 2);
        for invalid in [0, 33, usize::MAX] {
            let admission = Admission::new(invalid);
            assert_eq!(admission.limit(), 2);
            admission.set_limit(1);
            admission.set_limit(invalid);
            assert_eq!(admission.limit(), 2);
        }
        for valid in 1..=32 {
            let admission = Admission::new(valid);
            assert_eq!(admission.limit(), valid);
            admission.set_limit(33 - valid);
            assert_eq!(admission.limit(), 33 - valid);
        }
    }

    #[test]
    fn forty_threads_share_one_strict_budget() {
        for limit in [1, 2, 32] {
            let admission = Admission::new(limit);
            let start = Arc::new(Barrier::new(41));
            let acquired = Arc::new(Barrier::new(41));
            let release = Arc::new(Barrier::new(41));
            let threads: Vec<_> = (0..40)
                .map(|_| {
                    let admission = admission.clone();
                    let start = Arc::clone(&start);
                    let acquired = Arc::clone(&acquired);
                    let release = Arc::clone(&release);
                    thread::spawn(move || {
                        start.wait();
                        let permit = admission.try_acquire();
                        let success = permit.is_some();
                        acquired.wait();
                        release.wait();
                        drop(permit);
                        success
                    })
                })
                .collect();
            start.wait();
            acquired.wait();
            let held = admission.active();
            release.wait();
            let successes = threads
                .into_iter()
                .map(|worker| usize::from(worker.join().unwrap()))
                .sum::<usize>();
            assert_eq!(held, limit);
            assert_eq!(successes, limit);
            assert_eq!(admission.active(), 0);
        }
    }

    struct NoopWake;
    impl Wake for NoopWake {
        fn wake(self: Arc<Self>) {}
    }

    #[test]
    fn forty_async_tasks_at_barrier_hold_thirty_two_and_cancel_cleanly() {
        // A deterministic std-only executor: all futures reach the same
        // barrier before admission, then stay pending with their permits.
        let admission = Admission::new(32);
        let arrivals = Arc::new(AtomicUsize::new(0));
        let waker = Waker::from(Arc::new(NoopWake));
        let mut cx = Context::from_waker(&waker);
        let mut tasks: Vec<Pin<Box<dyn Future<Output = ()> + Send>>> = (0..40)
            .map(|_| {
                let admission = admission.clone();
                let arrivals = Arc::clone(&arrivals);
                Box::pin(async move {
                    arrivals.fetch_add(1, Ordering::SeqCst);
                    poll_fn(|_| {
                        if arrivals.load(Ordering::SeqCst) == 40 {
                            Poll::Ready(())
                        } else {
                            Poll::Pending
                        }
                    })
                    .await;
                    let _permit = admission.try_acquire();
                    pending::<()>().await;
                }) as Pin<Box<dyn Future<Output = ()> + Send>>
            })
            .collect();
        for _ in 0..2 {
            for task in &mut tasks {
                assert!(task.as_mut().poll(&mut cx).is_pending());
            }
        }
        assert_eq!(arrivals.load(Ordering::SeqCst), 40);
        assert_eq!(admission.active(), 32);
        drop(tasks);
        assert_eq!(admission.active(), 0);
    }

    #[test]
    fn lower_preserves_holders_and_raise_takes_effect_immediately() {
        let admission = Admission::new(3);
        let mut permits: Vec<_> = (0..3).map(|_| admission.try_acquire().unwrap()).collect();
        admission.set_limit(1);
        assert_eq!(admission.active(), 3);
        assert!(admission.try_acquire().is_none());
        permits.pop();
        permits.pop();
        assert_eq!(admission.active(), 1);
        assert!(admission.try_acquire().is_none());
        permits.pop();
        let one = admission.try_acquire().unwrap();
        admission.set_limit(3);
        let two = admission.try_acquire().unwrap();
        let three = admission.try_acquire().unwrap();
        assert!(admission.try_acquire().is_none());
        drop((one, two, three));
        assert_eq!(admission.active(), 0);
    }

    #[test]
    fn concurrent_lower_and_acquire_respect_completed_configuration() {
        for _ in 0..32 {
            let admission = Admission::new(32);
            let baseline = admission.try_acquire().unwrap();
            let start = Arc::new(Barrier::new(3));
            let changing = admission.clone();
            let setter_start = Arc::clone(&start);
            let setter = thread::spawn(move || {
                setter_start.wait();
                changing.set_limit(1);
            });
            let acquiring = admission.clone();
            let contender_start = Arc::clone(&start);
            let contender = thread::spawn(move || {
                contender_start.wait();
                acquiring.try_acquire()
            });
            start.wait();
            setter.join().unwrap();
            let raced = contender.join().unwrap();
            // Either the contender linearized before lowering and keeps its
            // permit, or lowering won and it got None. No later admission.
            assert_eq!(admission.active(), 1 + usize::from(raced.is_some()));
            assert_eq!(admission.limit(), 1);
            assert!(admission.try_acquire().is_none());
            drop((baseline, raced));
            assert_eq!(admission.active(), 0);
        }
    }

    #[test]
    fn errors_and_unwinding_drop_both_kinds_of_permit() {
        fn fail(admission: &Admission) -> Result<(), &'static str> {
            let _queue = admission.queue("error").unwrap();
            let _active = admission.try_acquire().unwrap();
            Err("upstream failed")?;
            Ok(())
        }
        let admission = Admission::default();
        assert!(fail(&admission).is_err());
        assert_eq!((admission.active(), admission.waiting()), (0, 0));
        let result = std::panic::catch_unwind(|| {
            let _queue = admission.queue("panic").unwrap();
            let _active = admission.try_acquire().unwrap();
            panic!("simulated task failure");
        });
        assert!(result.is_err());
        assert_eq!((admission.active(), admission.waiting()), (0, 0));
        assert!(lock(&admission.inner.waiting).by_session.is_empty());
    }

    #[test]
    fn per_key_eight_excludes_active_and_release_is_exactly_once() {
        let admission = Admission::new(1);
        let mut first = admission.queue("a").unwrap();
        first.release(); // Session lock has been acquired.
        let active = admission.try_acquire().unwrap();
        let mut queued: Vec<_> = (0..8).map(|_| admission.queue("a").unwrap()).collect();
        let other = admission.queue("b").unwrap();
        assert!(admission.queue("a").is_none());
        assert_eq!(admission.waiting(), 9);
        queued[0].release();
        queued[0].release();
        assert_eq!(admission.waiting(), 8);
        let replacement = admission.queue("a").unwrap();
        drop(first); // Must not decrement the replacement generation.
        assert_eq!(admission.waiting(), 9);
        assert_eq!(admission.active(), 1);
        assert!(admission.try_acquire().is_none());
        drop((queued, other, replacement));
        assert_eq!(admission.waiting(), 0);
        assert_eq!(admission.active(), 1);
        assert!(lock(&admission.inner.waiting).by_session.is_empty());
        drop(active);
        assert_eq!(admission.active(), 0);
    }

    #[test]
    fn forty_threads_queue_same_key_at_most_eight() {
        let admission = Admission::default();
        let start = Arc::new(Barrier::new(41));
        let acquired = Arc::new(Barrier::new(41));
        let release = Arc::new(Barrier::new(41));
        let threads: Vec<_> = (0..40)
            .map(|_| {
                let admission = admission.clone();
                let start = Arc::clone(&start);
                let acquired = Arc::clone(&acquired);
                let release = Arc::clone(&release);
                thread::spawn(move || {
                    start.wait();
                    let queue = admission.queue("same");
                    let success = queue.is_some();
                    acquired.wait();
                    release.wait();
                    drop(queue);
                    success
                })
            })
            .collect();
        start.wait();
        acquired.wait();
        let waiting = admission.waiting();
        release.wait();
        let successes = threads
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>();
        assert_eq!((waiting, successes), (8, 8));
        assert_eq!(admission.waiting(), 0);
        assert!(lock(&admission.inner.waiting).by_session.is_empty());
    }

    #[test]
    fn zero_waiters_remove_keys_even_across_many_sessions() {
        let admission = Admission::default();
        for index in 0..10_000 {
            let queue = admission.queue(&format!("session-{index}")).unwrap();
            assert_eq!(admission.waiting(), 1);
            drop(queue);
            assert_eq!(admission.waiting(), 0);
            assert!(lock(&admission.inner.waiting).by_session.is_empty());
        }
    }

    #[test]
    fn pending_async_queue_cancellation_cleans_up_and_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Admission>();
        assert_send_sync::<Permit>();
        assert_send_sync::<QueuePermit>();
        let admission = Admission::default();
        let cloned = admission.clone();
        let mut task = Box::pin(async move {
            let _waiting = cloned.queue("cancelled").unwrap();
            pending::<()>().await;
        });
        let waker = Waker::from(Arc::new(NoopWake));
        assert!(task
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        assert_eq!(admission.waiting(), 1);
        drop(task);
        assert_eq!(admission.waiting(), 0);
        assert!(lock(&admission.inner.waiting).by_session.is_empty());
    }
}
