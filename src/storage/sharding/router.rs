//! Query routing for the sharding system.

use super::config::ShardConfig;
use super::types::ShardId;
use crate::core::id::NodeId;
use std::collections::{HashMap, HashSet};

/// A step in a multi-shard traversal plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalStep {
    /// The shard to query at this step.
    pub shard_id: ShardId,
    /// Edge labels to traverse.
    pub edge_labels: Vec<String>,
    /// Whether this step may produce results on other shards.
    pub may_cross_shard: bool,
}

impl TraversalStep {
    /// Create a new traversal step.
    pub fn new(shard_id: ShardId, edge_labels: Vec<String>, may_cross_shard: bool) -> Self {
        Self {
            shard_id,
            edge_labels,
            may_cross_shard,
        }
    }
}

/// A plan for executing a multi-shard traversal query.
#[derive(Debug, Clone)]
pub struct TraversalPlan {
    /// Starting shard for the traversal.
    pub start_shard: ShardId,
    /// Shards that need to be queried.
    pub involved_shards: HashSet<ShardId>,
    /// Ordered steps for the traversal.
    pub steps: Vec<TraversalStep>,
    /// Whether this traversal requires cross-shard coordination.
    pub is_distributed: bool,
    /// Estimated cost (for query planning).
    pub estimated_cost: f64,
}

impl TraversalPlan {
    /// Create a single-shard traversal plan.
    pub fn single_shard(shard_id: ShardId) -> Self {
        let mut involved = HashSet::new();
        involved.insert(shard_id);
        Self {
            start_shard: shard_id,
            involved_shards: involved,
            steps: Vec::new(),
            is_distributed: false,
            estimated_cost: 1.0,
        }
    }

    /// Create a multi-shard traversal plan.
    pub fn multi_shard(start_shard: ShardId, shards: HashSet<ShardId>) -> Self {
        let is_distributed = shards.len() > 1;
        Self {
            start_shard,
            involved_shards: shards,
            steps: Vec::new(),
            is_distributed,
            estimated_cost: 1.0,
        }
    }

    /// Add a step to the traversal plan.
    pub fn add_step(&mut self, step: TraversalStep) {
        self.involved_shards.insert(step.shard_id);
        if step.may_cross_shard {
            self.is_distributed = true;
        }
        self.steps.push(step);
    }

    /// Set the estimated cost.
    pub fn with_cost(mut self, cost: f64) -> Self {
        self.estimated_cost = cost;
        self
    }
}

/// Routes queries to appropriate shards based on node labels.
#[derive(Debug)]
pub struct ShardRouter {
    /// The shard configuration.
    config: ShardConfig,
    /// Label to shard mapping for O(1) routing.
    label_to_shard: HashMap<String, ShardId>,
}

impl ShardRouter {
    /// Create a new shard router.
    pub fn new(config: ShardConfig) -> Self {
        let label_to_shard = config.build_label_map();
        Self {
            config,
            label_to_shard,
        }
    }

    /// Route a node query to the appropriate shard based on label.
    pub fn route_node(&self, label: &str) -> ShardId {
        *self
            .label_to_shard
            .get(label)
            .unwrap_or(&self.config.default_shard)
    }

    /// Route a node by its ID if we have cached label information.
    /// Falls back to default shard if label is unknown.
    pub fn route_node_by_id(&self, _node_id: NodeId, label: Option<&str>) -> ShardId {
        match label {
            Some(l) => self.route_node(l),
            None => self.config.default_shard,
        }
    }

    /// Determine which shards are involved in a traversal query.
    ///
    /// # Arguments
    /// * `start_label` - Label of the starting node
    /// * `target_labels` - Labels of potential target nodes
    pub fn route_traversal(&self, start_label: &str, target_labels: &[&str]) -> TraversalPlan {
        let start_shard = self.route_node(start_label);

        if target_labels.is_empty() {
            // No target labels means we might traverse to any shard
            return TraversalPlan::multi_shard(
                start_shard,
                self.config.shard_ids().into_iter().collect(),
            );
        }

        // Collect all shards that might be involved
        let mut involved_shards: HashSet<ShardId> = HashSet::new();
        involved_shards.insert(start_shard);

        for label in target_labels {
            involved_shards.insert(self.route_node(label));
        }

        if involved_shards.len() == 1 {
            TraversalPlan::single_shard(start_shard)
        } else {
            // Calculate cost based on number of shards involved
            // Cross-shard queries have higher cost due to network latency
            let base_cost = 1.0;
            let cross_shard_penalty = 2.0; // ~2x cost per additional shard
            let cost = base_cost + (involved_shards.len() as f64 - 1.0) * cross_shard_penalty;

            TraversalPlan::multi_shard(start_shard, involved_shards).with_cost(cost)
        }
    }

    /// Plan a multi-hop traversal.
    ///
    /// # Arguments
    /// * `start_label` - Label of the starting node
    /// * `edge_labels` - Edge labels for each hop
    /// * `expected_target_labels` - Expected labels at each hop (if known)
    pub fn plan_multi_hop(
        &self,
        start_label: &str,
        edge_labels: &[&str],
        expected_target_labels: Option<&[&str]>,
    ) -> TraversalPlan {
        let start_shard = self.route_node(start_label);
        let mut plan = TraversalPlan::single_shard(start_shard);

        let target_labels = expected_target_labels.unwrap_or(&[]);
        let mut current_shard = start_shard;

        for (i, edge_label) in edge_labels.iter().enumerate() {
            // Determine target shard if we know the target label
            let target_shard = if i < target_labels.len() {
                self.route_node(target_labels[i])
            } else {
                // Unknown target - might cross shards
                current_shard
            };

            let may_cross_shard = target_shard != current_shard || i >= target_labels.len();

            plan.add_step(TraversalStep::new(
                current_shard,
                vec![(*edge_label).to_string()],
                may_cross_shard,
            ));

            if i < target_labels.len() {
                current_shard = target_shard;
            }
        }

        // Estimate cost based on hops and potential cross-shard operations
        let hop_cost = 1.5; // Cost per hop
        let cross_shard_cost = 3.0; // Additional cost for cross-shard hops
        let mut cost = 1.0;
        for step in &plan.steps {
            cost += hop_cost;
            if step.may_cross_shard {
                cost += cross_shard_cost;
            }
        }
        plan.estimated_cost = cost;

        plan
    }

    /// Determine which shards need to be involved in a write operation.
    ///
    /// # Arguments
    /// * `source_label` - Label of the source node (for edges)
    /// * `target_label` - Label of the target node (for edges)
    ///
    /// Returns a tuple of (source_shard, target_shard, is_cross_shard).
    pub fn route_edge_write(
        &self,
        source_label: &str,
        target_label: &str,
    ) -> (ShardId, ShardId, bool) {
        let source_shard = self.route_node(source_label);
        let target_shard = self.route_node(target_label);
        let is_cross_shard = source_shard != target_shard;
        (source_shard, target_shard, is_cross_shard)
    }

    /// Get all shards that store a given label.
    pub fn shards_for_label(&self, label: &str) -> Vec<ShardId> {
        match self.label_to_shard.get(label) {
            Some(shard_id) => vec![*shard_id],
            None => vec![self.config.default_shard],
        }
    }

    /// Get the default shard.
    pub fn default_shard(&self) -> ShardId {
        self.config.default_shard
    }

    /// Get the underlying configuration.
    pub fn config(&self) -> &ShardConfig {
        &self.config
    }

    /// Check if a label is explicitly assigned to a shard.
    pub fn has_explicit_assignment(&self, label: &str) -> bool {
        self.label_to_shard.contains_key(label)
    }

    /// Get all explicitly assigned labels.
    pub fn assigned_labels(&self) -> Vec<&String> {
        self.label_to_shard.keys().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sharding::config::ShardDefinition;

    fn test_config() -> ShardConfig {
        ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person", "User", "Account"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location", "Address"]),
            ShardDefinition::new(2, "shard2:9000", vec!["Event", "Transaction", "Activity"]),
        ])
    }

    #[test]
    fn test_router_creation() {
        let config = test_config();
        let router = ShardRouter::new(config);

        assert!(router.has_explicit_assignment("Person"));
        assert!(router.has_explicit_assignment("Place"));
        assert!(!router.has_explicit_assignment("Unknown"));
    }

    #[test]
    fn test_route_node() {
        let router = ShardRouter::new(test_config());

        // Explicit assignments
        assert_eq!(router.route_node("Person").as_u16(), 0);
        assert_eq!(router.route_node("User").as_u16(), 0);
        assert_eq!(router.route_node("Place").as_u16(), 1);
        assert_eq!(router.route_node("Event").as_u16(), 2);

        // Unknown label falls back to default
        assert_eq!(router.route_node("Unknown").as_u16(), 0);
    }

    #[test]
    fn test_route_node_by_id() {
        let router = ShardRouter::new(test_config());
        let node_id = NodeId::new(100).unwrap();

        assert_eq!(router.route_node_by_id(node_id, Some("Person")).as_u16(), 0);
        assert_eq!(router.route_node_by_id(node_id, Some("Place")).as_u16(), 1);
        assert_eq!(router.route_node_by_id(node_id, None).as_u16(), 0); // Default
    }

    #[test]
    fn test_route_traversal_single_shard() {
        let router = ShardRouter::new(test_config());

        // Traversal within same shard
        let plan = router.route_traversal("Person", &["User", "Account"]);
        assert!(!plan.is_distributed);
        assert_eq!(plan.start_shard.as_u16(), 0);
        assert_eq!(plan.involved_shards.len(), 1);
    }

    #[test]
    fn test_route_traversal_cross_shard() {
        let router = ShardRouter::new(test_config());

        // Traversal crossing shards
        let plan = router.route_traversal("Person", &["Place", "Event"]);
        assert!(plan.is_distributed);
        assert_eq!(plan.involved_shards.len(), 3);
        assert!(plan.estimated_cost > 1.0); // Cross-shard has higher cost
    }

    #[test]
    fn test_route_traversal_empty_targets() {
        let router = ShardRouter::new(test_config());

        // Empty target labels means we might traverse anywhere
        let plan = router.route_traversal("Person", &[]);
        assert!(plan.is_distributed);
        assert_eq!(plan.involved_shards.len(), 3); // All shards
    }

    #[test]
    fn test_plan_multi_hop() {
        let router = ShardRouter::new(test_config());

        // Multi-hop within same shard
        let plan = router.plan_multi_hop("Person", &["KNOWS", "KNOWS"], Some(&["User", "Account"]));
        assert_eq!(plan.steps.len(), 2);
        assert!(!plan.is_distributed);

        // Multi-hop crossing shards
        let plan = router.plan_multi_hop(
            "Person",
            &["VISITED", "OCCURRED_AT"],
            Some(&["Place", "Event"]),
        );
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.is_distributed);
        assert!(plan.estimated_cost > 4.0); // Higher due to cross-shard
    }

    #[test]
    fn test_plan_multi_hop_unknown_targets() {
        let router = ShardRouter::new(test_config());

        // Unknown targets means potentially cross-shard
        let plan = router.plan_multi_hop("Person", &["KNOWS", "VISITED"], None);
        assert_eq!(plan.steps.len(), 2);
        // All steps may cross shard when targets are unknown
        for step in &plan.steps {
            assert!(step.may_cross_shard);
        }
    }

    #[test]
    fn test_route_edge_write() {
        let router = ShardRouter::new(test_config());

        // Same-shard edge
        let (src, tgt, cross) = router.route_edge_write("Person", "User");
        assert_eq!(src.as_u16(), 0);
        assert_eq!(tgt.as_u16(), 0);
        assert!(!cross);

        // Cross-shard edge
        let (src, tgt, cross) = router.route_edge_write("Person", "Place");
        assert_eq!(src.as_u16(), 0);
        assert_eq!(tgt.as_u16(), 1);
        assert!(cross);
    }

    #[test]
    fn test_shards_for_label() {
        let router = ShardRouter::new(test_config());

        let shards = router.shards_for_label("Person");
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].as_u16(), 0);

        let shards = router.shards_for_label("Unknown");
        assert_eq!(shards.len(), 1);
        assert_eq!(shards[0].as_u16(), 0); // Default shard
    }

    #[test]
    fn test_traversal_step() {
        let step = TraversalStep::new(ShardId::new(1).unwrap(), vec!["KNOWS".to_string()], false);

        assert_eq!(step.shard_id.as_u16(), 1);
        assert_eq!(step.edge_labels, vec!["KNOWS"]);
        assert!(!step.may_cross_shard);
    }

    #[test]
    fn test_traversal_plan_operations() {
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();

        let mut plan = TraversalPlan::single_shard(shard0);
        assert!(!plan.is_distributed);
        assert_eq!(plan.involved_shards.len(), 1);

        plan.add_step(TraversalStep::new(shard0, vec!["KNOWS".to_string()], false));
        assert!(!plan.is_distributed);

        plan.add_step(TraversalStep::new(
            shard1,
            vec!["VISITED".to_string()],
            true,
        ));
        assert!(plan.is_distributed);
        assert_eq!(plan.involved_shards.len(), 2);
    }

    #[test]
    fn test_assigned_labels() {
        let router = ShardRouter::new(test_config());
        let labels = router.assigned_labels();

        assert!(labels.contains(&&"Person".to_string()));
        assert!(labels.contains(&&"Place".to_string()));
        assert!(labels.contains(&&"Event".to_string()));
        assert_eq!(labels.len(), 9); // All labels from config
    }

    #[test]
    fn test_router_default_shard() {
        let router = ShardRouter::new(test_config());
        assert_eq!(router.default_shard().as_u16(), 0);
    }

    #[test]
    fn test_router_config_access() {
        let config = test_config();
        let router = ShardRouter::new(config.clone());
        assert_eq!(router.config().num_shards(), 3);
    }
}
