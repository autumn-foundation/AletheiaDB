//! Uniqueness constraint registry and enforcement types.
//!
//! Lives in `core` so that both `storage::recovery` (WAL replay) and `db` (API surface)
//! can import it without creating circular module dependencies.

use crate::core::changefeed::EntityKind;
use crate::core::error::{ConstraintError, Result};
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::{PropertyMap, PropertyValue};
use bitcode::{Decode, Encode};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum number of sample offending entity ids retained per conformance
/// violation (keeps `NonConformingOnEnable` reports bounded on large graphs).
pub const MAX_CONFORMANCE_SAMPLE_IDS: usize = 16;

/// A declared property type for a schema constraint (Issue #3378).
///
/// Maps each declarable type to the concrete [`PropertyValue`] variant it
/// accepts. Note there is no dedicated temporal `PropertyValue`: AletheiaDB
/// stores temporal/timestamp property values as microseconds-since-epoch
/// [`PropertyValue::Int`] (see [`crate::core::temporal`]), so
/// [`DeclaredType::Temporal`] matches `Int`. This is documented here and in
/// `docs/guides/schema-constraints.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DeclaredType {
    /// UTF-8 string values ([`PropertyValue::String`]).
    String,
    /// 64-bit signed integer values ([`PropertyValue::Int`]).
    Integer,
    /// 64-bit floating point values ([`PropertyValue::Float`]).
    Float,
    /// Boolean values ([`PropertyValue::Bool`]).
    Boolean,
    /// Temporal values, stored as microseconds-since-epoch integers
    /// ([`PropertyValue::Int`]).
    Temporal,
    /// Byte-array values ([`PropertyValue::Bytes`]).
    Bytes,
    /// Dense embedding vectors ([`PropertyValue::Vector`]). When `dim` is
    /// `Some(d)`, the vector's length must equal exactly `d`.
    Vector {
        /// Required vector dimension, or `None` to accept any dimension.
        dim: Option<usize>,
    },
}

impl DeclaredType {
    /// Stable token for this declared type, aligned with
    /// [`PropertyValue::type_name`] where a 1:1 mapping exists.
    pub const fn type_name(&self) -> &'static str {
        match self {
            DeclaredType::String => "string",
            DeclaredType::Integer => "int",
            DeclaredType::Float => "float",
            DeclaredType::Boolean => "bool",
            DeclaredType::Temporal => "temporal",
            DeclaredType::Bytes => "bytes",
            DeclaredType::Vector { .. } => "vector",
        }
    }

    /// Returns `true` if `value` conforms to this declared type.
    ///
    /// `Null` never matches any declared type here — null/absence handling is
    /// done by the required/nullable logic in
    /// [`ConstraintRegistry::check_entity`], not by the type matcher.
    pub fn matches(&self, value: &PropertyValue) -> bool {
        match self {
            DeclaredType::String => matches!(value, PropertyValue::String(_)),
            DeclaredType::Integer => matches!(value, PropertyValue::Int(_)),
            DeclaredType::Float => matches!(value, PropertyValue::Float(_)),
            DeclaredType::Boolean => matches!(value, PropertyValue::Bool(_)),
            // Temporal values are stored as micros-since-epoch integers.
            DeclaredType::Temporal => matches!(value, PropertyValue::Int(_)),
            DeclaredType::Bytes => matches!(value, PropertyValue::Bytes(_)),
            DeclaredType::Vector { dim } => match value {
                PropertyValue::Vector(v) => dim.is_none_or(|d| v.len() == d),
                _ => false,
            },
        }
    }
}

/// A single property-level schema constraint held in the registry (interned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyConstraint {
    /// The interned property key this constraint applies to.
    pub property: InternedString,
    /// The declared type the value must have, or `None` to accept any type.
    pub declared_type: Option<DeclaredType>,
    /// If `true`, the key must be present with a non-null value.
    pub required: bool,
    /// If `false`, an explicitly-`Null` value violates the constraint.
    pub nullable: bool,
}

/// A serializable, string-keyed descriptor for one property constraint.
///
/// Used as the public shape returned by
/// [`crate::db::AletheiaDB::list_schema_constraints`] / embedded in
/// `get_schema`, and as the on-disk record for the constraint sidecar and the
/// `.albk` backup payload (interned ids are not stable across restarts, so the
/// durable form carries resolved strings).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PropertyConstraintDescriptor {
    /// The property key name.
    pub property: String,
    /// The declared type, or `None` for any type.
    pub declared_type: Option<DeclaredType>,
    /// Whether the key must be present with a non-null value.
    pub required: bool,
    /// Whether an explicit `Null` value is permitted.
    pub nullable: bool,
}

/// A serializable descriptor for all constraints declared on one
/// `(entity_kind, label)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SchemaConstraintDescriptor {
    /// `"node"` or `"edge"`.
    pub entity_kind: String,
    /// The node label or edge type the constraints are scoped to.
    pub label: String,
    /// The per-property constraints.
    pub properties: Vec<PropertyConstraintDescriptor>,
}

/// A serializable descriptor for one declared uniqueness constraint
/// (Issue #3218).
///
/// Uniqueness constraints are normally persisted via the WAL
/// (`DeclareUniqueConstraint`) and reconstructed by WAL replay. A `.albk`
/// backup restores into a **fresh WAL**, so — like the #3378 schema
/// constraints — the declarations must ride along in the backup payload
/// (interned ids are not stable across restarts, so the durable form carries
/// resolved strings) and be re-declared on restore.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UniqueConstraintDescriptor {
    /// The node label the constraint is scoped to.
    pub label: String,
    /// The property key whose value must be unique per label.
    pub property: String,
}

/// One aggregated violation in a [`ConformanceReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceViolation {
    /// `"node"` or `"edge"`.
    pub entity_kind: String,
    /// The label/type scanned.
    pub label: String,
    /// The offending property key, or `None` when not property-specific.
    pub property: Option<String>,
    /// Human-readable reason (e.g. `"missing required key"`, `"expected int, got string"`).
    pub reason: String,
    /// A bounded sample of offending entity ids (up to
    /// [`MAX_CONFORMANCE_SAMPLE_IDS`]).
    pub sample_ids: Vec<u64>,
}

/// Report produced by declaring schema constraints on a (possibly populated)
/// label: whether the current state conforms, how many entities were checked,
/// how many failed, and the aggregated violations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    /// `true` iff every checked entity conforms.
    pub conforms: bool,
    /// Number of current-state entities checked.
    pub total_checked: usize,
    /// Number of non-conforming entities.
    pub total_non_conforming: usize,
    /// Aggregated per-(property, reason) violations.
    pub violations: Vec<ConformanceViolation>,
}

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

/// Outcome of checking one property value against one [`PropertyConstraint`].
///
/// Shared by the write-path validator ([`ConstraintRegistry::check_entity`])
/// and the enable-time scan ([`ConstraintRegistry::evaluate_conformance`]) so
/// both compute an identical category for every value (Issue #3378 FIX-5).
enum PropertyCheck {
    /// The value conforms to this constraint.
    Ok,
    /// The value is missing/absent for a required key, or an explicit `Null`
    /// on a non-nullable key. `reason` is the canonical human-readable string
    /// used in conformance reports; both sub-cases are the Missing category
    /// and both surface as `MissingRequiredKey` on the write path.
    Missing {
        /// Canonical reason string (`"missing required key"` or
        /// `"null value not permitted"`).
        reason: &'static str,
    },
    /// A present, non-null value whose type does not match the declared type.
    TypeMismatch {
        /// The declared type's token.
        expected: &'static str,
        /// The actual value's type token.
        actual: &'static str,
    },
}

/// The single per-constraint predicate shared by both validators (Issue #3378
/// FIX-5). Classifying every `(constraint, value)` here removes the risk of the
/// write path and the enable-time scan drifting on null/required/type
/// semantics.
fn check_property(c: &PropertyConstraint, value: Option<&PropertyValue>) -> PropertyCheck {
    let is_null = matches!(value, Some(PropertyValue::Null));
    let present_nonnull = value.is_some() && !is_null;

    if c.required && !present_nonnull {
        return PropertyCheck::Missing {
            reason: "missing required key",
        };
    }
    if is_null {
        if !c.nullable {
            return PropertyCheck::Missing {
                reason: "null value not permitted",
            };
        }
        return PropertyCheck::Ok;
    }
    // Present, non-null value: enforce the declared type if any.
    if let (Some(dt), Some(v)) = (c.declared_type, value)
        && !dt.matches(v)
    {
        return PropertyCheck::TypeMismatch {
            expected: dt.type_name(),
            actual: v.type_name(),
        };
    }
    PropertyCheck::Ok
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
    /// Property type / required-key schema constraints (Issue #3378), keyed by
    /// `(entity_kind, label_id)`. Opt-in: a label with no entry is fully
    /// schemaless. `Arc<Vec<..>>` so the hot write-path check clones a cheap
    /// pointer rather than the constraint vector.
    schema_constraints: DashMap<(EntityKind, InternedString), Arc<Vec<PropertyConstraint>>>,
    /// Serializes schema-constraint DDL (Issue #3378): the whole
    /// export→persist-sidecar→declare/drop read-modify-write in
    /// `enable_schema_constraints` / `drop_schema_constraint` runs under this
    /// lock so two concurrent DDL calls on different labels cannot race the
    /// sidecar file (last-write-wins silently dropping the other label from
    /// disk). A **leaf** lock: acquired only around that critical section and
    /// never held across any other AletheiaDB synchronization primitive, so it
    /// introduces no lock-order risk. `()` payload — it guards the external
    /// side effect (the sidecar file + the in-memory map staying consistent),
    /// not shared data of its own.
    schema_ddl_lock: parking_lot::Mutex<()>,
}

impl ConstraintRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            declarations: DashMap::new(),
            reservation_index: DashMap::new(),
            schema_constraints: DashMap::new(),
            schema_ddl_lock: parking_lot::Mutex::new(()),
        }
    }

    /// Acquire the schema-constraint DDL lock (Issue #3378). Callers hold the
    /// returned guard across the entire export→persist→declare/drop critical
    /// section in `enable`/`drop` so the on-disk sidecar and the in-memory
    /// registry stay consistent under concurrent DDL. Leaf lock — see the
    /// field docs.
    #[must_use = "hold the guard across the whole DDL critical section"]
    pub fn lock_schema_ddl(&self) -> parking_lot::MutexGuard<'_, ()> {
        self.schema_ddl_lock.lock()
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

    /// Export all declared uniqueness constraints as serializable, string-keyed
    /// descriptors (for the `.albk` backup payload, Issue #3218). Sorted
    /// deterministically by `(label, property)` so backups are reproducible.
    pub fn export_unique_constraints(&self) -> Vec<UniqueConstraintDescriptor> {
        let mut out: Vec<UniqueConstraintDescriptor> = self
            .list()
            .into_iter()
            .map(|(label, property)| UniqueConstraintDescriptor { label, property })
            .collect();
        out.sort_by(|a, b| {
            a.label
                .cmp(&b.label)
                .then_with(|| a.property.cmp(&b.property))
        });
        out
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

    // ========================================================================
    // Schema constraints (property type + required-key), Issue #3378
    // ========================================================================

    /// Returns `true` if no schema constraints are declared (used to skip the
    /// write-path check with zero overhead when the feature is unused).
    #[inline]
    pub fn schema_constraints_empty(&self) -> bool {
        self.schema_constraints.is_empty()
    }

    /// Declare (or replace) the schema constraints for `(kind, label)`.
    pub fn declare_schema_constraints(
        &self,
        kind: EntityKind,
        label: InternedString,
        constraints: Vec<PropertyConstraint>,
    ) {
        self.schema_constraints
            .insert((kind, label), Arc::new(constraints));
    }

    /// Drop the schema constraints for `(kind, label)`. Returns `true` if an
    /// entry was removed.
    pub fn drop_schema_constraints(&self, kind: EntityKind, label: InternedString) -> bool {
        self.schema_constraints.remove(&(kind, label)).is_some()
    }

    /// Fetch the schema constraints declared for `(kind, label)`, if any.
    pub fn schema_constraints_for(
        &self,
        kind: EntityKind,
        label: InternedString,
    ) -> Option<Arc<Vec<PropertyConstraint>>> {
        self.schema_constraints
            .get(&(kind, label))
            .map(|e| Arc::clone(e.value()))
    }

    /// Validate one entity's effective property map against the declared
    /// schema constraints for `(kind, label)`. Returns `Ok(())` when no
    /// constraints are declared (schemaless) or the map conforms.
    ///
    /// Semantics (see `docs/guides/schema-constraints.md`):
    /// - `required`: the key must be present with a non-null value; a missing
    ///   or `Null` value yields `MissingRequiredKey`.
    /// - `nullable == false`: an explicitly-`Null` value yields
    ///   `MissingRequiredKey` (null is not permitted).
    /// - `declared_type`: a present, non-null value whose type does not match
    ///   yields `TypeViolation`. `Null`/absent values skip the type check.
    ///
    /// The write path ([`Self::check_entity`]) and the enable-time scan
    /// ([`Self::evaluate_conformance`]) share one per-constraint predicate
    /// ([`check_property`]) so a value's category (missing / null-not-permitted
    /// / type-mismatch) is guaranteed identical in both paths (Issue #3378
    /// FIX-5) — no risk of the two validators drifting.
    pub fn check_entity(
        &self,
        kind: EntityKind,
        label: InternedString,
        properties: &PropertyMap,
    ) -> std::result::Result<(), ConstraintError> {
        let Some(constraints) = self.schema_constraints_for(kind, label) else {
            return Ok(());
        };
        let label_str = || GLOBAL_INTERNER.resolve_or_else(label, String::new);
        let prop_str = |p: InternedString| GLOBAL_INTERNER.resolve_or_else(p, String::new);

        let mut missing_keys: Vec<String> = Vec::new();
        for c in constraints.iter() {
            let value = properties.get_by_interned_key(&c.property);
            match check_property(c, value) {
                // Both "missing required key" and "null value not permitted"
                // are the Missing category and surface as MissingRequiredKey on
                // the write path (unchanged behavior); the shared predicate
                // guarantees the category matches the enable-time report.
                PropertyCheck::Missing { .. } => missing_keys.push(prop_str(c.property)),
                PropertyCheck::TypeMismatch { expected, actual } => {
                    return Err(ConstraintError::TypeViolation {
                        entity_kind: kind.as_str().to_string(),
                        label: label_str(),
                        property: prop_str(c.property),
                        expected_type: expected,
                        actual_type: actual,
                    });
                }
                PropertyCheck::Ok => {}
            }
        }

        if !missing_keys.is_empty() {
            return Err(ConstraintError::MissingRequiredKey {
                entity_kind: kind.as_str().to_string(),
                label: label_str(),
                missing_keys,
            });
        }
        Ok(())
    }

    /// Evaluate one entity's property map against `constraints`, returning a
    /// list of `(property, reason)` violations (used to build a
    /// [`ConformanceReport`] on enable). Unlike [`Self::check_entity`], this
    /// collects *all* violations rather than short-circuiting. Shares the
    /// per-constraint predicate [`check_property`] with the write path so the
    /// two never disagree on a value's category (Issue #3378 FIX-5).
    pub fn evaluate_conformance(
        constraints: &[PropertyConstraint],
        properties: &PropertyMap,
    ) -> Vec<(Option<String>, String)> {
        let mut out = Vec::new();
        for c in constraints {
            let value = properties.get_by_interned_key(&c.property);
            let reason = match check_property(c, value) {
                PropertyCheck::Ok => continue,
                PropertyCheck::Missing { reason } => reason.to_string(),
                PropertyCheck::TypeMismatch { expected, actual } => {
                    format!("expected type {expected}, got {actual}")
                }
            };
            let prop = GLOBAL_INTERNER.resolve_or_else(c.property, String::new);
            out.push((Some(prop), reason));
        }
        out
    }

    /// Export all declared schema constraints as serializable, string-keyed
    /// descriptors (for the sidecar, the `.albk` backup, and the public list
    /// API). Deterministically sorted by (entity_kind, label).
    pub fn export_schema_constraints(&self) -> Vec<SchemaConstraintDescriptor> {
        let mut out: Vec<SchemaConstraintDescriptor> = self
            .schema_constraints
            .iter()
            .filter_map(|entry| {
                let (kind, label_id) = *entry.key();
                let label = GLOBAL_INTERNER.resolve_with(label_id, |s| s.to_string())?;
                let properties = entry
                    .value()
                    .iter()
                    .filter_map(|c| {
                        let property =
                            GLOBAL_INTERNER.resolve_with(c.property, |s| s.to_string())?;
                        Some(PropertyConstraintDescriptor {
                            property,
                            declared_type: c.declared_type,
                            required: c.required,
                            nullable: c.nullable,
                        })
                    })
                    .collect();
                Some(SchemaConstraintDescriptor {
                    entity_kind: kind.as_str().to_string(),
                    label,
                    properties,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.entity_kind
                .cmp(&b.entity_kind)
                .then_with(|| a.label.cmp(&b.label))
        });
        out
    }

    /// Import schema constraints from serializable descriptors (re-interning
    /// labels/properties), replacing any existing schema-constraint state.
    /// Used by the sidecar / backup load path.
    pub fn import_schema_constraints(&self, descriptors: &[SchemaConstraintDescriptor]) {
        self.schema_constraints.clear();
        for d in descriptors {
            let kind = if d.entity_kind == "edge" {
                EntityKind::Edge
            } else {
                EntityKind::Node
            };
            let Ok(label_id) = GLOBAL_INTERNER.intern(&d.label) else {
                continue;
            };
            let mut constraints = Vec::with_capacity(d.properties.len());
            for p in &d.properties {
                if let Ok(prop_id) = GLOBAL_INTERNER.intern(&p.property) {
                    constraints.push(PropertyConstraint {
                        property: prop_id,
                        declared_type: p.declared_type,
                        required: p.required,
                        nullable: p.nullable,
                    });
                }
            }
            self.schema_constraints
                .insert((kind, label_id), Arc::new(constraints));
        }
    }
}

impl Default for ConstraintRegistry {
    fn default() -> Self {
        Self::new()
    }
}
