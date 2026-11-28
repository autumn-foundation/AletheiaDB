//! Read the vector timeline of one property from history (Issue #3888).
//!
//! The temporal vector index follows current storage: a node has a vector
//! from the commit that sets it until the commit that removes it (update,
//! delete, or retract). Current storage holds the open head of each node. At
//! commit `c`, that head is the version visible at
//! `(valid = MAX_VALID_TIMESTAMP, tx = c)`:
//!
//! - A normal or backdated update leaves an open head with the new content.
//! - A backfill update re-asserts the open head, so the content stays.
//! - Structural carry-forward versions have a closed valid interval, so they
//!   are not visible at the far-future valid time.
//! - Delete and retract leave no open head, so the read gives "no vector".

use std::sync::Arc;

use super::HistoricalStorage;
use crate::core::error::Result;
use crate::core::hlc::HybridTimestamp;
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;
use crate::core::temporal::{MAX_VALID_TIMESTAMP, Timestamp};
use crate::core::version::FastHashMap;
use crate::index::vector::temporal::VectorChange;

impl HistoricalStorage {
    /// Get each change to the `property` vector of each node, from hot history.
    ///
    /// The result has one entry for each commit that changed the vector of a
    /// node. `vector: None` means the node has no vector after that commit.
    /// The result is not sorted. Versions in the cold tier are not read.
    ///
    /// # Errors
    ///
    /// Returns an error if the properties of a version cannot be rebuilt.
    pub fn node_vector_changes(&self, property: &str) -> Result<Vec<VectorChange>> {
        let mut commits: FastHashMap<NodeId, Vec<Timestamp>> = FastHashMap::default();
        for version in self.node_versions.values() {
            commits
                .entry(version.node_id)
                .or_default()
                .push(version.commit_timestamp);
        }

        let far_valid = HybridTimestamp::new_unchecked(MAX_VALID_TIMESTAMP, 0);
        let mut changes = Vec::new();
        for (node, mut times) in commits {
            times.sort_unstable();
            times.dedup();
            let mut previous: Option<Arc<[f32]>> = None;
            for at in times {
                let vector = match self.find_node_version_at_time(node, far_valid, at) {
                    Some(version_id) => {
                        match self.reconstruct_node_properties(version_id)?.get(property) {
                            Some(PropertyValue::Vector(v)) => Some(Arc::clone(v)),
                            _ => None,
                        }
                    }
                    None => None,
                };
                if vector != previous {
                    changes.push(VectorChange {
                        at,
                        node,
                        vector: vector.clone(),
                    });
                    previous = vector;
                }
            }
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::temporal::{Timestamp, time};
    use crate::db::AletheiaDB;
    use crate::{PropertyMap, PropertyMapBuilder};

    fn vector(y: f32) -> PropertyMap {
        PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, y])
            .build()
    }

    /// The vectors of one node after each change, in commit order.
    fn timeline(db: &AletheiaDB) -> Vec<Option<Vec<f32>>> {
        let mut changes = db
            .historical
            .read()
            .node_vector_changes("embedding")
            .unwrap();
        changes.sort_by_key(|c| c.at);
        changes
            .into_iter()
            .map(|c| c.vector.map(|v| v.to_vec()))
            .collect()
    }

    #[test]
    fn update_and_delete_give_changes() {
        let db = AletheiaDB::new().unwrap();
        let id = db.create_node("Doc", vector(0.0)).unwrap();
        db.update_node_with_valid_time(id, vector(1.0), None)
            .unwrap();
        // Same vector again: no change.
        db.update_node_with_valid_time(id, vector(1.0), None)
            .unwrap();
        db.delete_node_with_valid_time(id, None).unwrap();
        assert_eq!(
            timeline(&db),
            vec![Some(vec![1.0, 0.0]), Some(vec![1.0, 1.0]), None]
        );
    }

    #[test]
    fn retract_gives_no_vector() {
        let db = AletheiaDB::new().unwrap();
        let id = db.create_node("Doc", vector(0.0)).unwrap();
        db.retract_node(id, time::now()).unwrap();
        assert_eq!(timeline(&db), vec![Some(vec![1.0, 0.0]), None]);
    }

    #[test]
    fn backdated_update_before_head_keeps_current_vector() {
        // A backfill update (valid_from before the head) re-asserts the head,
        // so current storage keeps the head vector. The timeline must agree.
        let db = AletheiaDB::new().unwrap();
        let now = time::now().wallclock();
        let t0 = Timestamp::from(now - 3_000_000);
        let t1 = Timestamp::from(now - 2_000_000);
        let t2 = Timestamp::from(now - 1_000_000);
        let id = db
            .create_node_with_valid_time("Doc", vector(0.0), Some(t0))
            .unwrap();
        db.update_node_with_valid_time(id, vector(1.0), Some(t2))
            .unwrap();
        db.update_node_with_valid_time(id, vector(2.0), Some(t1))
            .unwrap();
        let current = db.get_node(id).unwrap();
        let live = current
            .properties
            .get("embedding")
            .and_then(|v| v.as_vector());
        assert_eq!(live, Some(&[1.0f32, 1.0][..]), "head is kept");
        let last = timeline(&db).pop().flatten();
        assert_eq!(last.as_deref(), live);
    }

    #[test]
    fn removed_property_gives_no_vector() {
        let db = AletheiaDB::new().unwrap();
        let id = db.create_node("Doc", vector(0.0)).unwrap();
        db.remove_node_property(id, "embedding").unwrap();
        assert_eq!(timeline(&db), vec![Some(vec![1.0, 0.0]), None]);
    }

    #[test]
    fn nodes_without_the_property_give_no_changes() {
        let db = AletheiaDB::new().unwrap();
        db.create_node("Pad", PropertyMapBuilder::new().insert("x", 1i64).build())
            .unwrap();
        assert!(timeline(&db).is_empty());
    }
}
