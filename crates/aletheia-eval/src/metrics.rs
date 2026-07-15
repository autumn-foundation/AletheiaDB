//! Deterministic retrieval-quality metrics (Issue #3366).
//!
//! Every function here is a pure function of its inputs: given the same
//! retrieved list, gold set, and per-question flags it returns the same
//! `f64`, with no clocks, threads, or hidden state. This is what makes the
//! whole harness reproducible (see `harness::run`) and what the unit tests at
//! the bottom of this file pin with known inputs → known outputs.
//!
//! # Formulas
//!
//! Let `R` be the ordered list of retrieved evidence keys for one question,
//! `R_k` its first `k` elements, and `G` the gold-evidence set.
//!
//! * **precision@k** = `|R_k ∩ G| / k`. Fixed-budget: the denominator is `k`,
//!   not the number actually retrieved, so failing to fill the budget costs
//!   precision. Guarded: `k == 0` returns `0.0`.
//! * **recall@k** = `|R_k ∩ G| / |G|`. An empty gold set is vacuously perfect
//!   and returns `1.0` (there is nothing to miss).
//! * **grounding precision** = `|R ∩ G| / |R|` over the *entire* retrieved
//!   list (no `k` cap): of everything we grounded an answer on, what fraction
//!   was actually relevant. An empty retrieval returns `0.0`.
//! * **temporal accuracy** = fraction of time-anchored questions whose
//!   retrieved answer-bearing fact matches the gold answer valid at the
//!   anchor. Computed as `mean(correct_flags)`; an empty input returns `0.0`.
//! * **citation validity** = fraction of returned citations that resolve to a
//!   real entity/version supporting the answer. `mean(resolved_flags)`; an
//!   empty input returns `1.0` (no citations, nothing invalid).
//!
//! All aggregate (dataset-level) metrics are the arithmetic mean of the
//! per-question values, so they too are deterministic.

use std::collections::BTreeSet;

/// `|R_k ∩ G| / k` — standard fixed-budget precision at `k`.
///
/// `k == 0` is guarded and returns `0.0` (an empty budget retrieves nothing).
/// When `retrieved` is shorter than `k`, the missing slots simply contribute
/// no hits, so the denominator stays `k` (unfilled budget is penalised).
#[must_use]
pub fn precision_at_k(retrieved: &[String], gold: &BTreeSet<String>, k: usize) -> f64 {
    if k == 0 {
        return 0.0;
    }
    let hits = hits_in_top_k(retrieved, gold, k);
    hits as f64 / k as f64
}

/// `|R_k ∩ G| / |G|` — recall at `k`.
///
/// An empty gold set returns `1.0` (vacuously perfect: there is nothing to
/// recall). `k == 0` yields `0.0` for any non-empty gold set.
#[must_use]
pub fn recall_at_k(retrieved: &[String], gold: &BTreeSet<String>, k: usize) -> f64 {
    if gold.is_empty() {
        return 1.0;
    }
    let hits = hits_in_top_k(retrieved, gold, k);
    hits as f64 / gold.len() as f64
}

/// `|R ∩ G| / |R|` — grounding precision over the whole retrieved list.
///
/// Distinct from [`precision_at_k`]: there is no `k` cap and the denominator
/// is the number of items actually retrieved, so this measures the purity of
/// the evidence the answer was grounded on. An empty retrieval returns `0.0`.
#[must_use]
pub fn grounding_precision(retrieved: &[String], gold: &BTreeSet<String>) -> f64 {
    if retrieved.is_empty() {
        return 0.0;
    }
    // Note: unlike `precision_at_k`'s `hits_in_top_k`, the numerator here does
    // not de-duplicate — a repeated relevant key would be counted once per
    // occurrence. This is harmless because the harness builds `retrieved` from a
    // node-id-deduplicated candidate list (see `harness::run`), so the keys are
    // already distinct; the denominator counts the same list, keeping the ratio
    // well-defined.
    let relevant = retrieved.iter().filter(|item| gold.contains(*item)).count();
    relevant as f64 / retrieved.len() as f64
}

/// Fraction of time-anchored questions answered temporally-correctly.
///
/// Each element of `correct_flags` is one time-anchored question: `true` iff
/// the retrieved answer-bearing fact (reconstructed at the query's time
/// anchor) matched the gold answer valid at that anchor. An empty input
/// returns `0.0` (no temporal questions → no temporal signal).
#[must_use]
pub fn temporal_accuracy(correct_flags: &[bool]) -> f64 {
    mean_of_bools(correct_flags, 0.0)
}

/// Fraction of returned citations that resolve to a real supporting version.
///
/// Each element of `resolved_flags` is one citation: `true` iff it resolves
/// to a real entity/version in the database that supports the answer. A
/// citation to a non-existent version is `false`. An empty input returns
/// `1.0` (no citations were made, so none are invalid).
#[must_use]
pub fn citation_validity(resolved_flags: &[bool]) -> f64 {
    mean_of_bools(resolved_flags, 1.0)
}

/// Arithmetic mean of a slice of `f64`, or `0.0` for an empty slice.
///
/// Used to fold per-question metric values into the dataset-level aggregate.
#[must_use]
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Count how many of the first `k` retrieved items are in the gold set.
///
/// Duplicate retrieved keys each count once against `k` (the budget) but a key
/// already seen is not double-credited, so a retriever cannot inflate its
/// score by repeating the same relevant item.
fn hits_in_top_k(retrieved: &[String], gold: &BTreeSet<String>, k: usize) -> usize {
    let mut seen = BTreeSet::new();
    let mut hits = 0;
    for item in retrieved.iter().take(k) {
        if gold.contains(item) && seen.insert(item.clone()) {
            hits += 1;
        }
    }
    hits
}

fn mean_of_bools(flags: &[bool], empty_value: f64) -> f64 {
    if flags.is_empty() {
        return empty_value;
    }
    let correct = flags.iter().filter(|f| **f).count();
    correct as f64 / flags.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gold(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn retrieved(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn precision_at_k_basic() {
        // top-3 = [a, x, b]; gold = {a, b, c}; 2 relevant in top-3 → 2/3.
        let r = retrieved(&["a", "x", "b", "y"]);
        let g = gold(&["a", "b", "c"]);
        assert!((precision_at_k(&r, &g, 3) - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn precision_at_k_larger_than_result_set() {
        // Only 2 retrieved, both relevant, but k=5 → fixed budget denominator 5.
        let r = retrieved(&["a", "b"]);
        let g = gold(&["a", "b", "c"]);
        assert!((precision_at_k(&r, &g, 5) - 2.0 / 5.0).abs() < 1e-12);
    }

    #[test]
    fn precision_at_k_zero_guarded() {
        let r = retrieved(&["a"]);
        let g = gold(&["a"]);
        assert_eq!(precision_at_k(&r, &g, 0), 0.0);
    }

    #[test]
    fn precision_at_k_empty_gold_is_zero() {
        let r = retrieved(&["a", "b"]);
        let g = gold(&[]);
        assert_eq!(precision_at_k(&r, &g, 2), 0.0);
    }

    #[test]
    fn recall_at_k_basic() {
        // top-2 = [a, x]; gold {a, b, c}; 1 hit / 3 gold.
        let r = retrieved(&["a", "x", "b"]);
        let g = gold(&["a", "b", "c"]);
        assert!((recall_at_k(&r, &g, 2) - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn recall_at_k_full() {
        let r = retrieved(&["a", "b", "c"]);
        let g = gold(&["a", "b", "c"]);
        assert!((recall_at_k(&r, &g, 3) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn recall_at_k_empty_gold_is_vacuously_one() {
        let r = retrieved(&["a"]);
        let g = gold(&[]);
        assert_eq!(recall_at_k(&r, &g, 3), 1.0);
    }

    #[test]
    fn recall_at_k_larger_than_result_set() {
        // k exceeds retrieved length: still only the 2 present hits count.
        let r = retrieved(&["a", "b"]);
        let g = gold(&["a", "b", "c", "d"]);
        assert!((recall_at_k(&r, &g, 10) - 2.0 / 4.0).abs() < 1e-12);
    }

    #[test]
    fn grounding_precision_basic() {
        // 2 of 4 retrieved are relevant.
        let r = retrieved(&["a", "x", "b", "y"]);
        let g = gold(&["a", "b", "c"]);
        assert!((grounding_precision(&r, &g) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn grounding_precision_empty_retrieval_is_zero() {
        let r = retrieved(&[]);
        let g = gold(&["a"]);
        assert_eq!(grounding_precision(&r, &g), 0.0);
    }

    #[test]
    fn duplicate_hits_counted_once() {
        // Repeating a relevant key must not inflate the hit count.
        let r = retrieved(&["a", "a", "a"]);
        let g = gold(&["a", "b"]);
        // 1 unique hit within budget 3 → 1/3, not 3/3.
        assert!((precision_at_k(&r, &g, 3) - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn temporal_accuracy_all_correct() {
        assert_eq!(temporal_accuracy(&[true, true, true]), 1.0);
    }

    #[test]
    fn temporal_accuracy_mixed() {
        // The core signal: a question whose fact changed. Full config
        // (correct AS OF) → true, baseline (no anchoring) → false.
        assert_eq!(temporal_accuracy(&[true]), 1.0);
        assert_eq!(temporal_accuracy(&[false]), 0.0);
        assert_eq!(temporal_accuracy(&[true, false, true, false]), 0.5);
    }

    #[test]
    fn temporal_accuracy_empty_is_zero() {
        assert_eq!(temporal_accuracy(&[]), 0.0);
    }

    #[test]
    fn citation_validity_all_valid() {
        assert_eq!(citation_validity(&[true, true]), 1.0);
    }

    #[test]
    fn citation_to_nonexistent_version_is_invalid() {
        // One citation resolves, one points at a non-existent version.
        assert_eq!(citation_validity(&[true, false]), 0.5);
    }

    #[test]
    fn citation_validity_empty_is_one() {
        assert_eq!(citation_validity(&[]), 1.0);
    }

    #[test]
    fn mean_basic() {
        assert!((mean(&[1.0, 0.0, 0.5]) - 0.5).abs() < 1e-12);
        assert_eq!(mean(&[]), 0.0);
    }
}
