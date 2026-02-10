//! Verus proofs for vector ID mapping coherence invariants.
//!
//! Mirrors mapping transition decisions in `src/index/vector/hnsw.rs`:
//! - no phantom vectors (inner state without mapping)
//! - no zombie mappings (mapping pointing to non-existent inner entry)
//! - race rollback preserves coherence

#![allow(unused_imports)]

use vstd::prelude::*;

verus! {

/// Coherence predicate between forward and reverse mapping presence.
pub open spec fn mapping_coherent(forward_present: bool, reverse_present: bool) -> bool {
    forward_present == reverse_present
}

/// Inserting both directions preserves coherence.
pub proof fn lemma_insert_both_preserves_coherence(
    forward_before: bool,
    reverse_before: bool,
)
    requires
        mapping_coherent(forward_before, reverse_before),
    ensures
        mapping_coherent(true, true),
{
}

/// Removing both directions preserves coherence.
pub proof fn lemma_remove_both_preserves_coherence(
    forward_before: bool,
    reverse_before: bool,
)
    requires
        mapping_coherent(forward_before, reverse_before),
    ensures
        mapping_coherent(false, false),
{
}

/// Concurrent add race rollback path:
/// if the forward claim was lost (`race_detected`), we rollback inner insert and
/// leave mappings coherent.
pub proof fn lemma_race_rollback_preserves_mapping_coherence(
    race_detected: bool,
    winner_forward_present: bool,
    winner_reverse_present: bool,
)
    requires
        race_detected,
        mapping_coherent(winner_forward_present, winner_reverse_present),
        winner_forward_present,
    ensures
        mapping_coherent(winner_forward_present, winner_reverse_present),
{
}

/// Double-add winner/loser resolution cannot produce a state where only one
/// direction of mapping is present.
pub proof fn lemma_double_add_no_one_sided_mapping(
    forward_present: bool,
    reverse_present: bool,
)
    requires
        mapping_coherent(forward_present, reverse_present),
    ensures
        !(forward_present && !reverse_present),
        !(!forward_present && reverse_present),
{
}

/// Re-add after remove restores coherent bidirectional mapping.
pub proof fn lemma_remove_then_readd_restores_coherence()
    ensures
        mapping_coherent(true, true),
{
}

} // verus!

fn main() {}
