use crate::model::{PackingRequest, SolverMetrics, effort_exhausted};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use web_time::Instant;

/// Monotonic time source used by [`Deadline`].
///
/// Production callers normally use [`Deadline::new`]. Tests and deterministic
/// simulations can provide a clock whose value advances on each observation.
pub trait Clock: Send + Sync {
    fn now_ns(&self) -> u64;
}

#[derive(Debug)]
struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }
}

/// Monotonic search budget with an injectable clock.
///
/// It also carries one start's effort-stop latch: a guard that finds the effort budget spent
/// while work remains refuses that work and records it, so the effort flag says the budget
/// stopped the search rather than that a counter equals its limit afterwards. Clones
/// share the latch; [`Deadline::for_start`] gives a start its own.
#[derive(Clone)]
pub struct Deadline {
    clock: Arc<dyn Clock>,
    started_ns: u64,
    limit_ns: u64,
    effort_stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for Deadline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Deadline")
            .field("started_ns", &self.started_ns)
            .field("limit_ns", &self.limit_ns)
            .finish_non_exhaustive()
    }
}

impl Deadline {
    pub fn new(limit_ms: u64) -> Self {
        Self::with_clock(limit_ms, Arc::new(SystemClock::new()))
    }

    pub fn with_clock(limit_ms: u64, clock: Arc<dyn Clock>) -> Self {
        let started_ns = clock.now_ns();
        Self {
            clock,
            started_ns,
            limit_ns: limit_ms.saturating_mul(1_000_000),
            effort_stop: Arc::default(),
        }
    }

    /// The same clock and limit with a latch of its own, for one search start: parallel
    /// starts share the clock but never each other's effort stop.
    pub(crate) fn for_start(&self) -> Self {
        Self {
            effort_stop: Arc::default(),
            ..self.clone()
        }
    }

    /// Whether the effort budget stops the search here, recording that it did. Only a guard
    /// about to do more counted work asks: asking is refusing that work. O(1); the latch is
    /// written only when the guard fires, so the per-orientation check costs what it did.
    pub(crate) fn effort_stops(&self, request: &PackingRequest, metrics: &SolverMetrics) -> bool {
        if !effort_exhausted(request, metrics) {
            return false;
        }
        self.effort_stop.store(true, Ordering::Relaxed);
        true
    }

    /// Whether a guard found the effort budget spent and stopped this start there.
    pub(crate) fn effort_stopped(&self) -> bool {
        self.effort_stop.load(Ordering::Relaxed)
    }

    pub fn expired(&self) -> bool {
        self.elapsed_ns() >= self.limit_ns
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.elapsed_ns() / 1_000_000
    }

    pub fn remaining_ns(&self) -> u64 {
        self.limit_ns.saturating_sub(self.elapsed_ns())
    }

    pub(crate) fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    pub(crate) fn elapsed_ms_since(&self, started_ns: u64) -> u64 {
        self.now_ns().saturating_sub(started_ns) / 1_000_000
    }

    fn elapsed_ns(&self) -> u64 {
        self.now_ns().saturating_sub(self.started_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug)]
    struct StepClock(AtomicU64);

    impl Clock for StepClock {
        fn now_ns(&self) -> u64 {
            self.0.fetch_add(1_000_000, Ordering::SeqCst)
        }
    }

    #[test]
    fn an_injected_clock_expires_after_an_exact_number_of_observations() {
        let deadline = Deadline::with_clock(3, Arc::new(StepClock(AtomicU64::new(0))));
        assert!(!deadline.expired());
        assert!(!deadline.expired());
        assert!(deadline.expired());
    }
}
