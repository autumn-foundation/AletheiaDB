//! Deterministic task scheduler for concurrency simulation.
//!
//! `SimulatedScheduler` runs a list of tasks in a seeded-random order,
//! making the execution sequence fully reproducible from the seed alone.

use rand::SeedableRng;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;

/// Runs tasks in a deterministic, seeded-random order.
///
/// Every call to [`run_tasks`](Self::run_tasks) consumes the provided closures
/// and returns their results in the order they were executed (which may differ
/// from the order they were provided).
///
/// # Example
/// ```ignore
/// let mut sched = SimulatedScheduler::new(42);
/// let results = sched.run_tasks(vec![
///     Box::new(|| "a"),
///     Box::new(|| "b"),
///     Box::new(|| "c"),
/// ]);
/// // results contains ["a", "b", "c"] in a seeded-deterministic order
/// ```
#[derive(Debug)]
pub struct SimulatedScheduler {
    rng: SmallRng,
}

impl SimulatedScheduler {
    /// Create a new scheduler with the given seed.
    pub fn new(seed: u64) -> Self {
        Self {
            rng: SmallRng::seed_from_u64(seed),
        }
    }

    /// Execute all `tasks` in a seeded-random order and return their results.
    ///
    /// The returned `Vec` has the same length as `tasks`; each entry is the
    /// return value of the corresponding task closure after it ran.
    pub fn run_tasks<T>(&mut self, tasks: Vec<Box<dyn FnOnce() -> T>>) -> Vec<T> {
        if tasks.is_empty() {
            return Vec::new();
        }

        // Assign each task a random priority, then sort by that priority.
        let mut indexed: Vec<(usize, Box<dyn FnOnce() -> T>)> =
            tasks.into_iter().enumerate().collect();

        // Shuffle deterministically with the seeded RNG.
        indexed.shuffle(&mut self.rng);

        // Run tasks in shuffled order and collect results.
        indexed.into_iter().map(|(_, task)| task()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tasks_execute() {
        let mut s = SimulatedScheduler::new(0);
        let mut results = s.run_tasks(vec![
            Box::new(|| 1_u32),
            Box::new(|| 2_u32),
            Box::new(|| 3_u32),
        ]);
        results.sort_unstable();
        assert_eq!(results, vec![1, 2, 3]);
    }

    #[test]
    fn empty_task_list() {
        let mut s = SimulatedScheduler::new(0);
        let r: Vec<i32> = s.run_tasks(vec![]);
        assert!(r.is_empty());
    }

    #[test]
    fn same_seed_same_order() {
        let make_tasks = || -> Vec<Box<dyn FnOnce() -> usize>> {
            vec![Box::new(|| 10), Box::new(|| 20), Box::new(|| 30)]
        };
        let mut a = SimulatedScheduler::new(7);
        let mut b = SimulatedScheduler::new(7);
        assert_eq!(a.run_tasks(make_tasks()), b.run_tasks(make_tasks()));
    }
}
