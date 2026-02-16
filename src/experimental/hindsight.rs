//! Hindsight: Counterfactual Graph Analysis Engine.
//!
//! "Stop guessing. Start simulating."
//!
//! Hindsight allows you to create a lightweight, in-memory overlay on top of
//! your database state. You can simulate adding, modifying, or removing nodes
//! and edges, then run complex queries (pathfinding, vector search) on the
//! virtual graph without mutating the actual data.
//!
//! # Use Cases
//! - **LLM Reasoning**: "What if this fact were true?"
//! - **Impact Analysis**: "If I delete this edge, is the graph disconnected?"
//! - **Planning**: "If I add these 5 steps, does it create a valid plan?"

use crate::AletheiaDB;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, MAX_VALID_ID, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyMapBuilder};
use crate::utils::error::{Result, StorageError};
use std::collections::{HashMap, HashSet, VecDeque};

/// A scenario representing a set of hypothetical changes to the graph.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// Nodes added in this scenario.
    pub added_nodes: HashMap<NodeId, Node>,
    /// Properties modified on existing nodes (Patch semantics).
    pub modified_nodes: HashMap<NodeId, PropertyMap>,
    /// Nodes removed in this scenario.
    pub removed_nodes: HashSet<NodeId>,

    /// Edges added in this scenario.
    pub added_edges: HashMap<EdgeId, Edge>,
    /// Edges removed in this scenario.
    pub removed_edges: HashSet<EdgeId>,

    /// Adjacency index for added edges (Source -> [EdgeId]).
    pub added_outgoing: HashMap<NodeId, Vec<EdgeId>>,

    /// Counter for generating temporary node IDs.
    next_node_id: u64,
    /// Counter for generating temporary edge IDs.
    next_edge_id: u64,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            added_nodes: HashMap::new(),
            modified_nodes: HashMap::new(),
            removed_nodes: HashSet::new(),
            added_edges: HashMap::new(),
            removed_edges: HashSet::new(),
            added_outgoing: HashMap::new(),
            next_node_id: MAX_VALID_ID + 1,
            next_edge_id: MAX_VALID_ID + 1,
        }
    }
}

impl Scenario {
    /// Create a new empty scenario.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The Hindsight engine wrapping the database and a scenario.
pub struct Hindsight<'a> {
    db: &'a AletheiaDB,
    scenario: Scenario,
}

impl<'a> Hindsight<'a> {
    /// Create a new Hindsight engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            scenario: Scenario::new(),
        }
    }

    /// Get the current scenario.
    pub fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// Generate a temporary NodeId.
    fn next_node_id(&mut self) -> NodeId {
        let id = self.scenario.next_node_id;
        self.scenario.next_node_id += 1;
        NodeId::new_unchecked(id)
    }

    /// Generate a temporary EdgeId.
    fn next_edge_id(&mut self) -> EdgeId {
        let id = self.scenario.next_edge_id;
        self.scenario.next_edge_id += 1;
        EdgeId::new_unchecked(id)
    }

    // ==================== Mutation Methods ====================

    /// Simulate adding a node.
    pub fn add_node(&mut self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        let id = self.next_node_id();
        let interned_label = GLOBAL_INTERNER.intern(label)?;
        let node = Node::new(
            id,
            interned_label,
            properties,
            VersionId::new_unchecked(0), // Dummy version
        );
        self.scenario.added_nodes.insert(id, node);
        Ok(id)
    }

    /// Simulate adding an edge.
    pub fn add_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        let id = self.next_edge_id();
        let interned_label = GLOBAL_INTERNER.intern(label)?;
        let edge = Edge::new(
            id,
            interned_label,
            source,
            target,
            properties,
            VersionId::new_unchecked(0), // Dummy version
        );

        self.scenario.added_edges.insert(id, edge);
        self.scenario
            .added_outgoing
            .entry(source)
            .or_default()
            .push(id);

        Ok(id)
    }

    /// Simulate removing a node.
    pub fn remove_node(&mut self, id: NodeId) {
        if self.scenario.added_nodes.contains_key(&id) {
            // Revert addition
            self.scenario.added_nodes.remove(&id);
            // Also need to clean up added_outgoing if we want to be thorough,
            // but filtered access handles it.
        } else {
            self.scenario.removed_nodes.insert(id);
        }
    }

    /// Simulate removing an edge.
    pub fn remove_edge(&mut self, id: EdgeId) {
        if self.scenario.added_edges.contains_key(&id) {
            // Revert addition
            if let Some(edge) = self.scenario.added_edges.remove(&id) {
                // Remove from adjacency
                if let Some(list) = self.scenario.added_outgoing.get_mut(&edge.source) {
                    list.retain(|&e| e != id);
                }
            }
        } else {
            self.scenario.removed_edges.insert(id);
        }
    }

    /// Simulate updating a node (Patch).
    pub fn update_node(&mut self, id: NodeId, properties: PropertyMap) -> Result<()> {
        if let Some(node) = self.scenario.added_nodes.get_mut(&id) {
            // Merge properties into the added node
            let mut builder = PropertyMapBuilder::new();
            for (k, v) in node.properties.iter() {
                if let Some(key_str) = GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()) {
                    builder = builder.insert(&key_str, v.clone());
                }
            }

            // Apply updates
            for (k, v) in properties.iter() {
                if let Some(key_str) = GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()) {
                    builder = builder.insert(&key_str, v.clone());
                }
            }

            node.properties = builder.build();
        } else {
            // Record patch for existing node
            // If there's already a patch, we need to merge it.
            if let Some(existing_patch) = self.scenario.modified_nodes.get_mut(&id) {
                let mut builder = PropertyMapBuilder::new();
                // Rebuild from existing patch
                for (k, v) in existing_patch.iter() {
                    if let Some(key_str) = GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()) {
                        builder = builder.insert(&key_str, v.clone());
                    }
                }
                // Apply new updates
                for (k, v) in properties.iter() {
                    if let Some(key_str) = GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()) {
                        builder = builder.insert(&key_str, v.clone());
                    }
                }
                *existing_patch = builder.build();
            } else {
                self.scenario.modified_nodes.insert(id, properties);
            }
        }
        Ok(())
    }

    // ==================== Read Methods ====================

    /// Get a node from the virtual graph.
    pub fn get_node(&self, id: NodeId) -> Result<Node> {
        // 1. Check if removed
        if self.scenario.removed_nodes.contains(&id) {
            return Err(crate::utils::Error::Storage(StorageError::NodeNotFound(id)));
        }

        // 2. Check if added
        if let Some(node) = self.scenario.added_nodes.get(&id) {
            return Ok(node.clone());
        }

        // 3. Fetch from DB
        let mut node = self.db.get_node(id)?;

        // 4. Apply modifications
        if let Some(patch) = self.scenario.modified_nodes.get(&id) {
            // Merge properties
            let mut builder = PropertyMapBuilder::new();

            // Base properties
            for (k, v) in node.properties.iter() {
                if let Some(key_str) = GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()) {
                    builder = builder.insert(&key_str, v.clone());
                }
            }

            // Patch properties
            for (k, v) in patch.iter() {
                if let Some(key_str) = GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string()) {
                    builder = builder.insert(&key_str, v.clone());
                }
            }

            node.properties = builder.build();
        }

        Ok(node)
    }

    /// Get an edge from the virtual graph.
    pub fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        // 1. Check if removed
        if self.scenario.removed_edges.contains(&id) {
            return Err(crate::utils::Error::Storage(StorageError::EdgeNotFound(id)));
        }

        // 2. Check if added
        if let Some(edge) = self.scenario.added_edges.get(&id) {
            return Ok(edge.clone());
        }

        // 3. Fetch from DB
        let edge = self.db.get_edge(id)?;

        // Check if endpoints are removed (implicitly removes edge)
        if self.scenario.removed_nodes.contains(&edge.source)
            || self.scenario.removed_nodes.contains(&edge.target)
        {
            return Err(crate::utils::Error::Storage(StorageError::EdgeNotFound(id)));
        }

        Ok(edge)
    }

    /// Get outgoing edges from a node in the virtual graph.
    pub fn get_outgoing_edges(&self, id: NodeId) -> Vec<EdgeId> {
        // 1. Check if node removed
        if self.scenario.removed_nodes.contains(&id) {
            return Vec::new();
        }

        let mut edges = Vec::new();

        // 2. Get edges from DB (if node is not purely virtual)
        if !self.scenario.added_nodes.contains_key(&id) {
            // It's a DB node
            // get_outgoing_edges returns Vec<EdgeId>
            let db_edges = self.db.current.get_outgoing_edges(id);
            for edge_id in db_edges {
                if !self.scenario.removed_edges.contains(&edge_id) {
                    // Also check if target is removed.
                    if let Ok(target) = self.db.current.get_edge_target(edge_id) {
                        if !self.scenario.removed_nodes.contains(&target) {
                            edges.push(edge_id);
                        }
                    }
                }
            }
        }

        // 3. Add added edges
        if let Some(added) = self.scenario.added_outgoing.get(&id) {
            for &edge_id in added {
                // Check if target is removed (if target was existing node)
                // For added edges, we have them in memory
                if let Some(edge) = self.scenario.added_edges.get(&edge_id) {
                    if !self.scenario.removed_nodes.contains(&edge.target) {
                        edges.push(edge_id);
                    }
                }
            }
        }

        edges
    }

    // ==================== Analysis Methods ====================

    /// Find a path between two nodes using Breadth-First Search on the virtual graph.
    pub fn find_path_bfs(&self, start: NodeId, end: NodeId) -> Option<Vec<EdgeId>> {
        // Check if start or end are removed
        if self.scenario.removed_nodes.contains(&start)
            || self.scenario.removed_nodes.contains(&end)
        {
            return None;
        }

        if start == end {
            return Some(Vec::new());
        }

        let mut queue = VecDeque::new();
        queue.push_back((start, Vec::new()));

        let mut visited = HashSet::new();
        visited.insert(start);

        // Safety break to prevent infinite loops in malformed graphs or extremely large traversals
        let max_depth = 1000;

        while let Some((current, path)) = queue.pop_front() {
            if path.len() > max_depth {
                continue;
            }

            if current == end {
                return Some(path);
            }

            for edge_id in self.get_outgoing_edges(current) {
                // Resolve target
                let target_opt = if let Some(edge) = self.scenario.added_edges.get(&edge_id) {
                    Some(edge.target)
                } else {
                    // DB edge
                    self.db.current.get_edge_target(edge_id).ok()
                };

                if let Some(target) = target_opt {
                    if !visited.contains(&target) {
                        visited.insert(target);
                        let mut new_path = path.clone();
                        new_path.push(edge_id);
                        queue.push_back((target, new_path));
                    }
                }
            }
        }

        None
    }

    /// Find nodes with similar vector embeddings, respecting the scenario.
    ///
    /// This performs a hybrid search:
    /// 1. Searches the database (filtering out removed/modified nodes).
    /// 2. Scans added/modified nodes in the scenario.
    /// 3. Merges and sorts the results.
    pub fn find_similar(
        &self,
        property: &str,
        vector: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // 1. Identify virtual candidates (Added nodes + Modified nodes with vector update)
        let mut candidates = Vec::new();

        // Scan added nodes
        for (id, node) in &self.scenario.added_nodes {
            if let Some(val) = node.properties.get(property) {
                if let Some(vec) = val.as_vector() {
                    let score = crate::core::vector::cosine_similarity(vector, vec)?;
                    candidates.push((*id, score));
                }
            }
        }

        // Scan modified nodes (if they have the vector property, they override DB)
        for (id, patch) in &self.scenario.modified_nodes {
            if let Some(val) = patch.get(property) {
                if let Some(vec) = val.as_vector() {
                    let score = crate::core::vector::cosine_similarity(vector, vec)?;
                    candidates.push((*id, score));
                }
            }
        }

        // 2. Search DB with predicate
        // We filter out:
        // - Removed nodes
        // - Modified nodes (since we handled them above if they have the vector)
        //   Note: If a modified node *doesn't* touch the vector, we *should* get it from DB.
        //   So we only filter modified nodes if the patch *contains* the vector property.

        // Build set of IDs to exclude from DB search
        let mut exclude_ids = self.scenario.removed_nodes.clone();
        for (id, patch) in &self.scenario.modified_nodes {
            if patch.contains_key(property) {
                exclude_ids.insert(*id);
            }
        }

        let db_results = if self.db.has_vector_index(property) {
            self.db
                .find_similar_with_predicate(property, vector, k, |id| !exclude_ids.contains(id))?
        } else {
            Vec::new() // Or error? Let's be robust and just return virtual results if no index.
        };

        // 3. Merge
        candidates.extend(db_results);

        // 4. Sort and Top-K
        // Sort descending by score
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(k);

        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_hindsight_basic_crud() {
        let db = AletheiaDB::new().unwrap();
        let mut hindsight = Hindsight::new(&db);

        // Add Node
        let props = PropertyMapBuilder::new().insert("name", "Ghost").build();
        let id = hindsight.add_node("Spirit", props).unwrap();

        assert!(id.as_u64() > MAX_VALID_ID);

        // Get Node
        let node = hindsight.get_node(id).unwrap();
        assert!(node.has_label_str("Spirit"));
        assert_eq!(
            node.get_property("name").unwrap().as_str().unwrap(),
            "Ghost"
        );

        // Update Node
        let update_props = PropertyMapBuilder::new().insert("age", 100).build();
        hindsight.update_node(id, update_props).unwrap();

        let node_updated = hindsight.get_node(id).unwrap();
        assert_eq!(
            node_updated.get_property("age").unwrap().as_int().unwrap(),
            100
        );
        assert_eq!(
            node_updated.get_property("name").unwrap().as_str().unwrap(),
            "Ghost"
        );

        // Remove Node
        hindsight.remove_node(id);
        assert!(hindsight.get_node(id).is_err());
    }

    #[test]
    fn test_hindsight_pathfinding() {
        let db = AletheiaDB::new().unwrap();

        // Create DB state: A --(NEXT)--> B   D
        let props = PropertyMapBuilder::new().build();
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let d = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(a, b, "NEXT", props.clone()).unwrap();

        let mut hindsight = Hindsight::new(&db);

        // Verify path A->B exists
        let path = hindsight.find_path_bfs(a, b).unwrap();
        assert_eq!(path.len(), 1);

        // Verify path B->D does NOT exist
        assert!(hindsight.find_path_bfs(b, d).is_none());

        // Add virtual edge B->D
        let _e_bd = hindsight.add_edge(b, d, "NEXT", props.clone()).unwrap();

        // Verify path A->D exists now (A->B->D)
        let path_new = hindsight.find_path_bfs(a, d).unwrap();
        assert_eq!(path_new.len(), 2);

        // Remove edge A->B virtually
        // Need to find the edge ID.
        let edges = hindsight.get_outgoing_edges(a);
        let e_ab = edges[0];
        hindsight.remove_edge(e_ab);

        // Verify path A->D broken
        assert!(hindsight.find_path_bfs(a, d).is_none());
    }

    #[test]
    fn test_hindsight_vector_search() {
        let db = AletheiaDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // DB: Node 1 at [1.0, 0.0]
        let props1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let n1 = db.create_node("Node", props1).unwrap();

        // DB: Node 2 at [0.0, 1.0]
        let props2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let n2 = db.create_node("Node", props2).unwrap();

        let mut hindsight = Hindsight::new(&db);

        // Virtual: Node 3 at [0.9, 0.1] (Close to N1)
        let props3 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.9, 0.1])
            .build();
        let n3 = hindsight.add_node("Node", props3).unwrap();

        // Remove Node 2 virtually
        hindsight.remove_node(n2);

        // Search for [1.0, 0.0]
        // Should find N1 (DB) and N3 (Virtual). Should NOT find N2.
        let results = hindsight.find_similar("vec", &[1.0, 0.0], 5).unwrap();

        assert_eq!(results.len(), 2);

        // N1 (1.0) and N3 (~0.99) should be top results.
        let ids: Vec<NodeId> = results.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&n1));
        assert!(ids.contains(&n3));
        assert!(!ids.contains(&n2));
    }
}
