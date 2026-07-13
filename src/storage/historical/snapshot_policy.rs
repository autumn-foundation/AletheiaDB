//! Per-entity temporal vector snapshot policy (Issue #383).
//!
//! The pre-anchor hook machinery (Issue #3525) fires a hook **before** every
//! anchor is stored so an external index (e.g. a temporal vector index) can
//! create a synchronized snapshot. Historically that hook was *global*: every
//! anchor on **any** entity triggered a whole-index vector snapshot, even for
//! entities whose embedding never changed. At production scale this is far too
//! coarse — updating a single node re-snapshots every indexed vector.
//!
//! This module adds **per-entity control** over whether an entity's anchor
//! triggers the pre-anchor snapshot hooks. A [`SnapshotPolicyRegistry`] maps
//! individual entities (nodes or edges) to a [`SnapshotPolicy`], falling back to
//! a configurable default. The historical write path consults the resolved
//! policy at anchor time and **skips** the snapshot hooks entirely for entities
//! whose policy is [`SnapshotPolicy::Skip`], decoupling graph anchors from
//! vector snapshots.
//!
//! ## Backward compatibility
//!
//! The default policy is [`SnapshotPolicy::Snapshot`], so a database that never
//! configures a per-entity policy behaves **exactly** as before Issue #383:
//! every anchor runs the registered pre-anchor hooks. Callers opt into
//! fine-grained control by setting per-entity overrides (mark stable entities
//! `Skip`) or by flipping the default to `Skip` and opting specific entities
//! back in with `Snapshot`.
//!
//! ## Locking
//!
//! The registry is a plain in-struct map. It is consulted while the caller
//! already holds the `historical` write lock — the same critical section that
//! runs the hooks — so it acquires **no new** synchronization primitive, never
//! calls back into `wal` / `current_timestamp` / the adjacency indexes, and can
//! only ever *reduce* the work done under the lock (a `Skip` decision avoids the
//! hook run altogether). The lock-order contract in `CLAUDE.md` and the #3525
//! bounded-hook-execution guarantees are therefore fully preserved.

use crate::core::version::FastHashMap;
use std::hash::Hash;

/// Per-entity decision for whether an anchor triggers the temporal vector
/// snapshot (pre-anchor) hooks (Issue #383).
///
/// This gates the **trigger**, not the graph anchor itself: a `Skip` entity
/// still forms graph anchors normally; only its vector-snapshot hook run is
/// suppressed, so no vector snapshot is captured for that anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SnapshotPolicy {
    /// Run the registered pre-anchor snapshot hooks when this entity anchors.
    ///
    /// This is the default and reproduces the pre-#383 global behavior.
    #[default]
    Snapshot,
    /// Do **not** run the pre-anchor snapshot hooks when this entity anchors —
    /// no vector snapshot is captured for the entity's anchors.
    Skip,
}

impl SnapshotPolicy {
    /// Returns `true` if this policy means the pre-anchor snapshot hooks should
    /// run for the entity's anchor.
    #[inline]
    pub fn should_snapshot(self) -> bool {
        matches!(self, SnapshotPolicy::Snapshot)
    }
}

/// A registry mapping individual entities of one kind (nodes **or** edges) to a
/// [`SnapshotPolicy`], with a configurable fall-through default.
///
/// Resolution is: per-entity override if present, otherwise the registry
/// default. The default default (sic) is [`SnapshotPolicy::Snapshot`] for
/// backward compatibility (Issue #383).
///
/// Keyed by an entity id (`NodeId` / `EdgeId`), which are cheap `Copy` newtypes
/// over `u64`; the [`FastHashMap`] identity hasher makes lookups near-free on
/// the write path.
#[derive(Debug, Clone)]
pub(crate) struct SnapshotPolicyRegistry<K: Eq + Hash + Copy> {
    /// Per-entity policy overrides. Absent key ⇒ use `default_policy`.
    overrides: FastHashMap<K, SnapshotPolicy>,
    /// Policy applied to any entity without an explicit override.
    default_policy: SnapshotPolicy,
}

impl<K: Eq + Hash + Copy> Default for SnapshotPolicyRegistry<K> {
    fn default() -> Self {
        Self {
            overrides: FastHashMap::default(),
            default_policy: SnapshotPolicy::default(),
        }
    }
}

impl<K: Eq + Hash + Copy> SnapshotPolicyRegistry<K> {
    /// Resolve the effective policy for `key`: the per-entity override if one is
    /// set, otherwise the registry default.
    #[inline]
    pub(crate) fn resolve(&self, key: K) -> SnapshotPolicy {
        self.overrides
            .get(&key)
            .copied()
            .unwrap_or(self.default_policy)
    }

    /// Set an explicit per-entity policy override for `key`.
    pub(crate) fn set(&mut self, key: K, policy: SnapshotPolicy) {
        self.overrides.insert(key, policy);
    }

    /// Remove any explicit override for `key`, reverting it to the default.
    ///
    /// Returns the override that was removed, if any.
    pub(crate) fn clear(&mut self, key: K) -> Option<SnapshotPolicy> {
        self.overrides.remove(&key)
    }

    /// Set the fall-through default policy applied to entities without an
    /// explicit override.
    pub(crate) fn set_default(&mut self, policy: SnapshotPolicy) {
        self.default_policy = policy;
    }

    /// Return the current fall-through default policy.
    #[inline]
    pub(crate) fn default_policy(&self) -> SnapshotPolicy {
        self.default_policy
    }

    /// Number of explicit per-entity overrides currently registered.
    ///
    /// Primarily for observability/testing.
    #[cfg(test)]
    pub(crate) fn override_count(&self) -> usize {
        self.overrides.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::NodeId;

    fn nid(v: u64) -> NodeId {
        NodeId::new(v).unwrap()
    }

    #[test]
    fn default_policy_is_snapshot_backward_compat() {
        let reg: SnapshotPolicyRegistry<NodeId> = SnapshotPolicyRegistry::default();
        // No overrides, default default => Snapshot for every entity.
        assert_eq!(reg.resolve(nid(1)), SnapshotPolicy::Snapshot);
        assert!(reg.resolve(nid(999)).should_snapshot());
        assert_eq!(reg.override_count(), 0);
    }

    #[test]
    fn override_beats_default() {
        let mut reg: SnapshotPolicyRegistry<NodeId> = SnapshotPolicyRegistry::default();
        reg.set(nid(1), SnapshotPolicy::Skip);
        assert_eq!(reg.resolve(nid(1)), SnapshotPolicy::Skip);
        assert!(!reg.resolve(nid(1)).should_snapshot());
        // Other entities still follow the default.
        assert_eq!(reg.resolve(nid(2)), SnapshotPolicy::Snapshot);
        assert_eq!(reg.override_count(), 1);
    }

    #[test]
    fn set_default_flips_fallthrough_but_not_overrides() {
        let mut reg: SnapshotPolicyRegistry<NodeId> = SnapshotPolicyRegistry::default();
        reg.set(nid(1), SnapshotPolicy::Snapshot); // explicit opt-in
        reg.set_default(SnapshotPolicy::Skip);
        assert_eq!(reg.default_policy(), SnapshotPolicy::Skip);
        // Opted-in entity keeps Snapshot; everything else follows the new default.
        assert_eq!(reg.resolve(nid(1)), SnapshotPolicy::Snapshot);
        assert_eq!(reg.resolve(nid(2)), SnapshotPolicy::Skip);
    }

    #[test]
    fn clear_reverts_to_default() {
        let mut reg: SnapshotPolicyRegistry<NodeId> = SnapshotPolicyRegistry::default();
        reg.set(nid(1), SnapshotPolicy::Skip);
        assert_eq!(reg.clear(nid(1)), Some(SnapshotPolicy::Skip));
        assert_eq!(reg.resolve(nid(1)), SnapshotPolicy::Snapshot);
        // Clearing an absent key is a no-op.
        assert_eq!(reg.clear(nid(1)), None);
        assert_eq!(reg.override_count(), 0);
    }
}
