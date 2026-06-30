//! Uniqueness constraint registry and enforcement types.
//!
//! Lives in `core` so that both `storage::recovery` (WAL replay) and `db` (API surface)
//! can import it without creating circular module dependencies.

use crate::core::error::{ConstraintError, Result};
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::{PropertyMap, PropertyValue};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

/// Canonical hashable + equatable key derived from a `PropertyValue`.
///
/// Only scalar types that can serve as unique keys are supported.
/// Null, Vector, SparseVector, and Array are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ValueKey {
    /// Boolean value.
    Bool(bool),
    /// Integer value.
    Int(i64),
    /// Float value (stored as bits for `Hash`+`Eq`).
    Float(u64),
    /// String value.
    String(Arc<str>),
    /// Bytes value.
    Bytes(Arc<[u8]>),
}

impl ValueKey {
    /// Try to create a `ValueKey` from a `PropertyValue`.
    /// Returns `None` for unsupported types (Null, Vector, SparseVector, Array).
    pub fn from_property_value(v: &PropertyValue) -> Option<Self> {
        match v {
            PropertyValue::Bool(b) => Some(ValueKey::Bool(*b)),
            PropertyValue::Int(i) => Some(ValueKey::Int(*i)),
            PropertyValue::Float(f) => {
                // Normalize all NaN representations to one canonical bit pattern so two
                // NaN floats are treated as the same unique key (instead of different
                // bit-patterns bypassing the constraint).
                let bits = if f.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    f.to_bits()
                };
                Some(ValueKey::Float(bits))
            }
            PropertyValue::String(s) => Some(ValueKey::String(Arc::clone(s))),
            PropertyValue::Bytes(b) => Some(ValueKey::Bytes(Arc::clone(b))),
            PropertyValue::Null
            | PropertyValue::Vector(_)
            | PropertyValue::SparseVector(_)
            | PropertyValue::Array(_) => None,
        }
    }

    /// Human-readable display for error messages.
    pub fn display_string(&self) -> String {
        match self {
            ValueKey::Bool(b) => b.to_string(),
            ValueKey::Int(i) => i.to_string(),
            ValueKey::Float(bits) => f64::from_bits(*bits).to_string(),
            ValueKey::String(s) => s.as_ref().to_string(),
            ValueKey::Bytes(b) => format!("<bytes:{}>", b.len()),
        }
    }
}

/// Reservation key — identifies a unique slot in the reservation index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReservationKey {
    /// Interned label ID.
    pub label: InternedString,
    /// Interned property key ID.
    pub property: InternedString,
    /// Canonical value key.
    pub value: ValueKey,
}

/// RAII guard holding in-flight constraint reservations for one transaction.
///
/// - **Rollback path** (`Drop`): releases all added reservations so the index
///   has no orphan entries.
/// - **Success path** (`commit()`): keeps added reservations and removes the
///   old-value entries that were displaced by update/delete operations.
pub struct ReservationGuard {
    registry: Arc<ConstraintRegistry>,
    /// Keys added during this transaction (released on rollback).
    added: Vec<ReservationKey>,
    /// Old-value keys to remove after commit (freed on the success path).
    /// HashSet for O(1) dedup in the removed-entries loop.
    to_remove: std::collections::HashSet<ReservationKey>,
    committed: bool,
}

impl ReservationGuard {
    fn new(registry: Arc<ConstraintRegistry>) -> Self {
        Self {
            registry,
            added: Vec::new(),
            to_remove: std::collections::HashSet::new(),
            committed: false,
        }
    }

    /// Call on the success path to lock in added reservations and free old ones.
    pub fn commit(mut self) {
        self.committed = true;
        for key in &self.to_remove {
            self.registry.reservation_index.remove(key);
        }
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        if !self.committed {
            for key in &self.added {
                self.registry.reservation_index.remove(key);
            }
        }
    }
}

/// Central registry for uniqueness constraints.
///
/// Thread-safe via DashMap; designed for concurrent read/write access.
///
/// # Lock Order
///
/// The `ConstraintRegistry` is a **leaf** in AletheiaDB's lock hierarchy.
/// It is acquired (via DashMap shard locks) before `current_timestamp` and
/// never held across any other synchronization primitive.
pub struct ConstraintRegistry {
    /// Active constraint declarations: (label_id, property_id) → present = enabled.
    declarations: DashMap<(InternedString, InternedString), ()>,
    /// Currently-valid reservation index: ReservationKey → owning NodeId.
    pub(crate) reservation_index: DashMap<ReservationKey, NodeId>,
}

impl ConstraintRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            declarations: DashMap::new(),
            reservation_index: DashMap::new(),
        }
    }

    /// Returns `true` if there is an active constraint for `(label, property)`.
    #[inline]
    pub fn is_constrained(&self, label: InternedString, property: InternedString) -> bool {
        self.declarations.contains_key(&(label, property))
    }

    /// Record a constraint declaration (called by `enable()` and WAL replay).
    pub fn declare(&self, label: InternedString, property: InternedString) {
        self.declarations.insert((label, property), ());
    }

    /// Remove a constraint declaration (called by `disable()` and WAL replay).
    pub fn undeclare(&self, label: InternedString, property: InternedString) {
        self.declarations.remove(&(label, property));
        self.reservation_index
            .retain(|k, _| !(k.label == label && k.property == property));
    }

    /// List all declared constraints as `(label_string, property_string)` pairs.
    pub fn list(&self) -> Vec<(String, String)> {
        self.declarations
            .iter()
            .filter_map(|entry| {
                let (label_id, prop_id) = entry.key();
                let label = GLOBAL_INTERNER.resolve_with(*label_id, |s| s.to_string())?;
                let property = GLOBAL_INTERNER.resolve_with(*prop_id, |s| s.to_string())?;
                Some((label, property))
            })
            .collect()
    }

    /// Atomically attempt to reserve `(label, property, value) → node_id`.
    ///
    /// - If the slot is vacant, inserts and returns `Ok(())`.
    /// - If the slot is already owned by `node_id`, returns `Ok(())` (idempotent).
    /// - If the slot is owned by a different node, returns `Err(UniqueViolation)`.
    pub fn try_reserve(
        &self,
        key: ReservationKey,
        node_id: NodeId,
        label_str: &str,
        property_str: &str,
    ) -> std::result::Result<bool, ConstraintError> {
        use dashmap::mapref::entry::Entry;
        match self.reservation_index.entry(key.clone()) {
            Entry::Vacant(e) => {
                e.insert(node_id);
                Ok(true) // newly inserted
            }
            Entry::Occupied(e) => {
                let existing = *e.get();
                if existing == node_id {
                    Ok(false) // same-node idempotent, not a new insertion
                } else {
                    Err(ConstraintError::UniqueViolation {
                        label: label_str.to_string(),
                        property: property_str.to_string(),
                        value: key.value.display_string(),
                        existing_node_id: existing,
                    })
                }
            }
        }
    }

    /// Pre-flight scan: check that enabling `(label, property)` on `nodes` would
    /// not reveal existing duplicates. Fails with `DuplicateOnEnable` if any exist.
    pub fn check_no_duplicates(
        nodes: &[crate::core::graph::Node],
        label: InternedString,
        property: InternedString,
        label_str: &str,
        property_str: &str,
    ) -> Result<()> {
        let mut seen: HashMap<ValueKey, Vec<NodeId>> = HashMap::new();

        for node in nodes {
            if node.label != label {
                continue;
            }
            if let Some(pv) = node.properties.get_by_interned_key(&property) {
                // Null means "property not set" — treat it as absent, not as a
                // constraint violation or an unsupported type.
                if matches!(pv, PropertyValue::Null) {
                    continue;
                }
                match ValueKey::from_property_value(pv) {
                    None => {
                        return Err(ConstraintError::UnsupportedKeyType {
                            label: label_str.to_string(),
                            property: property_str.to_string(),
                            type_name: pv.type_name().to_string(),
                        }
                        .into());
                    }
                    Some(vk) => {
                        seen.entry(vk).or_default().push(node.id);
                    }
                }
            }
        }

        for (vk, ids) in &seen {
            if ids.len() >= 2 {
                return Err(ConstraintError::DuplicateOnEnable {
                    label: label_str.to_string(),
                    property: property_str.to_string(),
                    value: vk.display_string(),
                    node_ids: ids.clone(),
                }
                .into());
            }
        }

        Ok(())
    }

    /// Rebuild the reservation index from a slice of currently-valid nodes for one
    /// `(label, property)` constraint pair. Called after WAL replay.
    pub fn rebuild_from_nodes(
        &self,
        nodes: &[crate::core::graph::Node],
        label: InternedString,
        property: InternedString,
    ) {
        for node in nodes {
            if node.label != label {
                continue;
            }
            if let Some(value) = node.properties.get_by_interned_key(&property)
                && let Some(vk) = ValueKey::from_property_value(value)
            {
                let key = ReservationKey {
                    label,
                    property,
                    value: vk,
                };
                self.reservation_index.insert(key, node.id);
            }
        }
    }

    /// Build a `ReservationGuard` by atomically reserving all constraint keys
    /// that the transaction's operations would create/update/delete.
    ///
    /// `added_entries`: post-tx (label, properties, node_id) for created/updated nodes.
    /// `removed_entries`: pre-tx (label, properties, node_id) for updated/deleted nodes
    ///   (these keys are freed on commit).
    pub fn reserve_for_transaction(
        self: &Arc<Self>,
        added_entries: &[(InternedString, &PropertyMap, NodeId)],
        removed_entries: &[(InternedString, &PropertyMap, NodeId)],
    ) -> std::result::Result<ReservationGuard, ConstraintError> {
        let mut guard = ReservationGuard::new(Arc::clone(self));

        // Track all keys "claimed" by this tx post-commit (newly reserved OR
        // idempotently re-confirmed on same node).  Keys in `claimed` must NOT
        // be freed on commit even if they also appear in the pre-tx state.
        let mut claimed: std::collections::HashSet<ReservationKey> =
            std::collections::HashSet::new();

        for (label_id, props, node_id) in added_entries {
            for (prop_id, value) in props.iter() {
                let prop_id = *prop_id;
                if !self.is_constrained(*label_id, prop_id) {
                    continue;
                }
                let vk = match ValueKey::from_property_value(value) {
                    Some(k) => k,
                    None => continue,
                };
                let key = ReservationKey {
                    label: *label_id,
                    property: prop_id,
                    value: vk,
                };
                let label_str = GLOBAL_INTERNER
                    .resolve_with(*label_id, |s| s.to_string())
                    .unwrap_or_default();
                let prop_str = GLOBAL_INTERNER
                    .resolve_with(prop_id, |s| s.to_string())
                    .unwrap_or_default();

                match self.try_reserve(key.clone(), *node_id, &label_str, &prop_str) {
                    Ok(true) => {
                        // Newly reserved — must roll back on tx failure.
                        guard.added.push(key.clone());
                        claimed.insert(key);
                    }
                    Ok(false) => {
                        // Same node idempotent — already in the index; not a new
                        // insertion, but still "claimed" so we don't free it.
                        claimed.insert(key);
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
        }

        for (label_id, props, _node_id) in removed_entries {
            for (prop_id, value) in props.iter() {
                let prop_id = *prop_id;
                if !self.is_constrained(*label_id, prop_id) {
                    continue;
                }
                if let Some(vk) = ValueKey::from_property_value(value) {
                    let key = ReservationKey {
                        label: *label_id,
                        property: prop_id,
                        value: vk,
                    };
                    // Only free the old key if it is not still claimed by this tx.
                    // (Handles the case where a node updates a property to its own
                    // current value — the slot must not be freed.)
                    // HashSet insert is idempotent, so no explicit contains check needed.
                    if !claimed.contains(&key) {
                        guard.to_remove.insert(key);
                    }
                }
            }
        }

        Ok(guard)
    }
}

impl Default for ConstraintRegistry {
    fn default() -> Self {
        Self::new()
    }
}
