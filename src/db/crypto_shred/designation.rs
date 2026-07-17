//! Designation targets — which entities / property keys a subject seals
//! (Issue #3359, slice PR-1a).

use bitcode::{Decode, Encode};

/// Reserved-key prefix: engine-internal property keys are **never** sealed, so
/// crypto-shred cannot break engine invariants that read these keys. This is the
/// binding-coordinator rule (engine-reserved keys are exempt from sealing).
pub const RESERVED_KEY_PREFIX: &str = "__aletheia_";

/// This lane's own internal marker-key prefix (reserved for future PR-1b use).
pub const SHRED_KEY_PREFIX: &str = "__shred_";

/// Whether a property key is engine-reserved and therefore exempt from sealing.
#[must_use]
pub fn is_reserved_key(key: &str) -> bool {
    key.starts_with(RESERVED_KEY_PREFIX) || key.starts_with(SHRED_KEY_PREFIX)
}

/// A single designation target under a subject.
///
/// A subject's designation is a set of these. `WholeNode` / `WholeEdge` seal all
/// (non-reserved) property keys of the entity; the `*Properties` variants seal
/// only the explicitly-listed keys.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum DesignationTarget {
    /// Seal all non-reserved property keys of this node.
    WholeNode(u64),
    /// Seal all non-reserved property keys of this edge.
    WholeEdge(u64),
    /// Seal only the listed property keys of this node.
    NodeProperties(u64, Vec<String>),
    /// Seal only the listed property keys of this edge.
    EdgeProperties(u64, Vec<String>),
}

/// Whether a target refers to a node (`true`) or an edge (`false`).
impl DesignationTarget {
    /// The entity id this target applies to.
    #[must_use]
    pub fn entity_id(&self) -> u64 {
        match self {
            DesignationTarget::WholeNode(id)
            | DesignationTarget::WholeEdge(id)
            | DesignationTarget::NodeProperties(id, _)
            | DesignationTarget::EdgeProperties(id, _) => *id,
        }
    }

    /// `true` if this target applies to a node.
    #[must_use]
    pub fn is_node(&self) -> bool {
        matches!(
            self,
            DesignationTarget::WholeNode(_) | DesignationTarget::NodeProperties(_, _)
        )
    }

    /// Whether, under this target, property `key` of the given entity should be
    /// sealed under the subject key.
    ///
    /// - **Whole-entity** targets seal every key **except** engine-reserved
    ///   (`__aletheia_*`) / shred-internal (`__shred_*`) keys.
    /// - **Property-scoped** targets seal only listed keys, and **never** a
    ///   reserved key even if (mistakenly) listed.
    ///
    /// `is_node` selects whether `entity` is interpreted as a node id; a
    /// node-scoped target never matches an edge and vice-versa.
    #[must_use]
    pub fn should_seal_key(&self, is_node: bool, entity: u64, key: &str) -> bool {
        if is_reserved_key(key) {
            return false;
        }
        match self {
            DesignationTarget::WholeNode(id) => is_node && *id == entity,
            DesignationTarget::WholeEdge(id) => !is_node && *id == entity,
            DesignationTarget::NodeProperties(id, keys) => {
                is_node && *id == entity && keys.iter().any(|k| k == key)
            }
            DesignationTarget::EdgeProperties(id, keys) => {
                !is_node && *id == entity && keys.iter().any(|k| k == key)
            }
        }
    }
}

/// Whether **any** target in a designation set seals `key` of the entity.
///
/// This is the collection-level `should_seal_key` an apply-time seal hook (PR-1b)
/// will consult; provided here so the rule is unit-tested in the foundation.
#[must_use]
pub fn any_should_seal(
    targets: &[DesignationTarget],
    is_node: bool,
    entity: u64,
    key: &str,
) -> bool {
    targets
        .iter()
        .any(|t| t.should_seal_key(is_node, entity, key))
}
