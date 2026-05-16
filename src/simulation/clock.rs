//! Simulated clock for deterministic temporal testing.
//!
//! `SimulatedClock` lets tests control what `time::now()` returns on the current
//! thread, enabling precise clock-jump and time-travel scenarios without touching
//! wall-clock time.

use std::cell::Cell;

use crate::core::hlc::HybridTimestamp;
use crate::core::temporal::Timestamp;

thread_local! {
    /// Overrides `time::now()` for the current thread when set.
    /// `None` means "use the real wall clock".
    static SIMULATED_NOW_MICROS: Cell<Option<i64>> = const { Cell::new(None) };
}

/// Read the current thread-local simulated time, if any.
///
/// Called by `time::now()` when the `simulation` feature is active.
#[inline]
pub(crate) fn thread_local_now() -> Option<i64> {
    SIMULATED_NOW_MICROS.with(|c| c.get())
}

/// Set the thread-local simulated time (raw microseconds since epoch).
fn set_thread_local(micros: Option<i64>) {
    SIMULATED_NOW_MICROS.with(|c| c.set(micros));
}

/// A controllable clock for simulation tests.
///
/// Create a `SimulatedClock`, call [`inject`](Self::inject) to intercept
/// `time::now()` on the current thread, then advance, jump, or freeze time
/// as the scenario requires.
///
/// # Example
/// ```ignore
/// use aletheiadb::simulation::SimulatedClock;
/// use aletheiadb::core::temporal::time;
///
/// let mut clock = SimulatedClock::new(1_000_000); // t = 1s
/// let _guard = clock.inject();
/// assert_eq!(time::now().wallclock(), 1_000_000);
/// clock.advance_by(500);
/// // time::now() now returns 1_000_500 µs
/// ```
#[derive(Debug)]
pub struct SimulatedClock {
    current_micros: i64,
}

impl SimulatedClock {
    /// Create a new clock at the given initial time (microseconds since epoch).
    pub fn new(initial_micros: i64) -> Self {
        Self {
            current_micros: initial_micros,
        }
    }

    /// Current simulated time in microseconds since Unix epoch.
    pub fn now_micros(&self) -> i64 {
        self.current_micros
    }

    /// Current simulated time as a `Timestamp` (HybridTimestamp with logical=0).
    pub fn now_as_timestamp(&self) -> Timestamp {
        HybridTimestamp::new_unchecked(self.current_micros, 0)
    }

    /// Advance the clock by `delta_micros` (must be ≥ 0).
    ///
    /// # Panics
    /// Panics if `delta_micros` is negative (use [`jump_to`](Self::jump_to) for backwards moves).
    pub fn advance_by(&mut self, delta_micros: i64) {
        assert!(
            delta_micros >= 0,
            "Use jump_to() to move the clock backwards"
        );
        self.current_micros = self.current_micros.saturating_add(delta_micros);
        // If an injection guard is active on this thread, keep it in sync.
        if SIMULATED_NOW_MICROS.with(|c| c.get()).is_some() {
            set_thread_local(Some(self.current_micros));
        }
    }

    /// Jump the clock to an absolute microsecond value (may go backwards).
    pub fn jump_to(&mut self, absolute_micros: i64) {
        self.current_micros = absolute_micros;
        if SIMULATED_NOW_MICROS.with(|c| c.get()).is_some() {
            set_thread_local(Some(self.current_micros));
        }
    }

    /// Apply a relative jump (positive = forward, negative = backward).
    pub fn jump_by(&mut self, delta_micros: i64) {
        self.current_micros = self.current_micros.saturating_add(delta_micros);
        if SIMULATED_NOW_MICROS.with(|c| c.get()).is_some() {
            set_thread_local(Some(self.current_micros));
        }
    }

    /// Intercept `time::now()` on the current thread.
    ///
    /// Returns a guard that restores the **previous** thread-local value when dropped,
    /// making nested injections safe. If no outer injection is active, dropping the
    /// guard reverts to the real wall clock.
    pub fn inject(&self) -> ClockInjectionGuard {
        let previous = thread_local_now();
        set_thread_local(Some(self.current_micros));
        ClockInjectionGuard { previous }
    }
}

/// RAII guard that restores the previous simulated-clock value when dropped.
///
/// Dropping this guard restores whatever `time::now()` returned before the
/// corresponding [`SimulatedClock::inject`] call, enabling safe nesting of
/// multiple injections on the same thread.
#[must_use = "dropping this guard immediately would restore the clock override at once"]
#[derive(Debug)]
pub struct ClockInjectionGuard {
    /// The thread-local value that was active before this guard was created.
    previous: Option<i64>,
}

impl Drop for ClockInjectionGuard {
    fn drop(&mut self) {
        set_thread_local(self.previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::time;

    #[test]
    fn clock_new_starts_at_given_time() {
        let c = SimulatedClock::new(999);
        assert_eq!(c.now_micros(), 999);
    }

    #[test]
    fn clock_advance_by_accumulates() {
        let mut c = SimulatedClock::new(0);
        c.advance_by(100);
        c.advance_by(50);
        assert_eq!(c.now_micros(), 150);
    }

    #[test]
    fn clock_jump_to_allows_backwards() {
        let mut c = SimulatedClock::new(1000);
        c.jump_to(1);
        assert_eq!(c.now_micros(), 1);
    }

    #[test]
    fn inject_overrides_time_now() {
        let c = SimulatedClock::new(7_777_777);
        let _g = c.inject();
        assert_eq!(time::now().wallclock(), 7_777_777);
    }

    #[test]
    fn guard_drop_restores_real_clock() {
        {
            let c = SimulatedClock::new(1);
            let _g = c.inject();
        }
        // After drop, thread-local must be cleared (no outer injection was active).
        assert!(thread_local_now().is_none());
    }

    #[test]
    fn nested_injections_restore_outer_on_inner_drop() {
        let outer = SimulatedClock::new(1_000);
        let inner = SimulatedClock::new(2_000);

        let _outer_guard = outer.inject();
        assert_eq!(time::now().wallclock(), 1_000);

        {
            let _inner_guard = inner.inject();
            assert_eq!(time::now().wallclock(), 2_000);
        }
        // Inner guard dropped — outer value is restored, not None.
        assert_eq!(time::now().wallclock(), 1_000);
    }
}
