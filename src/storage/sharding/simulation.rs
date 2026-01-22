//! Sharding simulation for analyzing edge cuts and strategy effectiveness.
//!
//! This module provides tools to simulate different sharding strategies
//! and analyze their impact on edge cuts, query latency, and storage overhead.

use super::types::ShardId;
use std::collections::HashMap;

/// Result of a sharding simulation.
#[derive(Debug, Clone)]
pub struct SimulationResult {
    /// Number of nodes in the simulation.
    pub total_nodes: u64,
    /// Number of edges in the simulation.
    pub total_edges: u64,
    /// Number of shards.
    pub num_shards: usize,
    /// Nodes per shard.
    pub nodes_per_shard: HashMap<ShardId, u64>,
    /// Edges per shard (including replicated cross-shard edges).
    pub edges_per_shard: HashMap<ShardId, u64>,
    /// Edge cut analysis.
    pub edge_cuts: EdgeCutAnalysis,
    /// Estimated query latency metrics.
    pub latency_estimates: LatencyEstimates,
    /// Storage overhead analysis.
    pub storage_analysis: StorageAnalysis,
}

impl SimulationResult {
    /// Create a new simulation result.
    pub fn new(total_nodes: u64, total_edges: u64, num_shards: usize) -> Self {
        Self {
            total_nodes,
            total_edges,
            num_shards,
            nodes_per_shard: HashMap::new(),
            edges_per_shard: HashMap::new(),
            edge_cuts: EdgeCutAnalysis::default(),
            latency_estimates: LatencyEstimates::default(),
            storage_analysis: StorageAnalysis::default(),
        }
    }

    /// Calculate the node balance ratio (coefficient of variation).
    pub fn node_balance_ratio(&self) -> f64 {
        if self.nodes_per_shard.is_empty() {
            return 0.0;
        }

        let values: Vec<u64> = self.nodes_per_shard.values().copied().collect();
        coefficient_of_variation(&values)
    }

    /// Calculate the edge balance ratio.
    pub fn edge_balance_ratio(&self) -> f64 {
        if self.edges_per_shard.is_empty() {
            return 0.0;
        }

        let values: Vec<u64> = self.edges_per_shard.values().copied().collect();
        coefficient_of_variation(&values)
    }
}

/// Analysis of edge cuts in a sharded graph.
#[derive(Debug, Clone, Default)]
pub struct EdgeCutAnalysis {
    /// Number of edges that cross shard boundaries.
    pub cross_shard_edges: u64,
    /// Number of edges within the same shard.
    pub local_edges: u64,
    /// Cross-shard edge ratio (0.0 = all local, 1.0 = all cross-shard).
    pub cross_shard_ratio: f64,
    /// Storage overhead from edge replication.
    pub replication_overhead: f64,
    /// Edge cuts by shard pair.
    pub cuts_by_shard_pair: HashMap<(ShardId, ShardId), u64>,
}

impl EdgeCutAnalysis {
    /// Create a new edge cut analysis.
    pub fn new(cross_shard: u64, local: u64) -> Self {
        let total = cross_shard + local;
        let ratio = if total > 0 {
            cross_shard as f64 / total as f64
        } else {
            0.0
        };

        // With edge replication, each cross-shard edge is stored twice
        let overhead = if total > 0 {
            cross_shard as f64 / total as f64
        } else {
            0.0
        };

        Self {
            cross_shard_edges: cross_shard,
            local_edges: local,
            cross_shard_ratio: ratio,
            replication_overhead: overhead,
            cuts_by_shard_pair: HashMap::new(),
        }
    }

    /// Add edge cut statistics for a shard pair.
    pub fn add_shard_pair_cut(&mut self, shard1: ShardId, shard2: ShardId, count: u64) {
        // Normalize to ensure consistent ordering
        let key = if shard1.as_u16() <= shard2.as_u16() {
            (shard1, shard2)
        } else {
            (shard2, shard1)
        };
        *self.cuts_by_shard_pair.entry(key).or_insert(0) += count;
    }

    /// Get the shard pair with the most edge cuts.
    pub fn most_connected_pair(&self) -> Option<((ShardId, ShardId), u64)> {
        self.cuts_by_shard_pair
            .iter()
            .max_by_key(|(_, count)| **count)
            .map(|(pair, count)| (*pair, *count))
    }
}

/// Estimated query latency for different query patterns.
#[derive(Debug, Clone, Default)]
pub struct LatencyEstimates {
    /// Single-node lookup latency (microseconds).
    pub single_node_lookup_us: f64,
    /// Single-hop traversal latency (microseconds).
    pub single_hop_us: f64,
    /// Multi-hop traversal latency (microseconds).
    pub multi_hop_us: f64,
    /// Cross-shard query penalty (microseconds per shard crossed).
    pub cross_shard_penalty_us: f64,
    /// Estimated 3-hop query latency.
    pub three_hop_estimated_us: f64,
}

impl LatencyEstimates {
    /// Create latency estimates based on simulation results.
    pub fn estimate(cross_shard_ratio: f64, _num_shards: usize) -> Self {
        // Base latencies from performance targets
        let single_node_lookup_us = 1.0; // <1µs target
        let single_hop_us = 30.0; // ~30µs for local
        let cross_shard_penalty_us = 1000.0; // ~1ms network penalty

        // Multi-hop estimation
        // Probability of crossing shards at each hop
        let p_cross = cross_shard_ratio;
        let expected_crosses_per_hop = p_cross;

        let multi_hop_us = single_hop_us + expected_crosses_per_hop * cross_shard_penalty_us;

        // 3-hop estimation
        // Expected number of cross-shard hops in 3 hops
        let expected_crosses_3hop = 3.0 * p_cross;
        let three_hop_estimated_us =
            3.0 * single_hop_us + expected_crosses_3hop * cross_shard_penalty_us;

        Self {
            single_node_lookup_us,
            single_hop_us,
            multi_hop_us,
            cross_shard_penalty_us,
            three_hop_estimated_us,
        }
    }
}

/// Storage overhead analysis.
#[derive(Debug, Clone, Default)]
pub struct StorageAnalysis {
    /// Total storage without replication (bytes).
    pub base_storage_bytes: u64,
    /// Additional storage from edge replication (bytes).
    pub replication_overhead_bytes: u64,
    /// Total storage with replication (bytes).
    pub total_storage_bytes: u64,
    /// Overhead ratio (replication / base).
    pub overhead_ratio: f64,
}

impl StorageAnalysis {
    /// Calculate storage analysis.
    ///
    /// Assumptions:
    /// - Average node size: 256 bytes
    /// - Average edge size: 64 bytes
    pub fn calculate(total_nodes: u64, total_edges: u64, cross_shard_edges: u64) -> Self {
        const AVG_NODE_SIZE: u64 = 256;
        const AVG_EDGE_SIZE: u64 = 64;

        let node_storage = total_nodes * AVG_NODE_SIZE;
        let edge_storage = total_edges * AVG_EDGE_SIZE;
        let base_storage = node_storage + edge_storage;

        // Cross-shard edges are stored twice (once on each shard)
        let replication_overhead = cross_shard_edges * AVG_EDGE_SIZE;
        let total_storage = base_storage + replication_overhead;

        let overhead_ratio = if base_storage > 0 {
            replication_overhead as f64 / base_storage as f64
        } else {
            0.0
        };

        Self {
            base_storage_bytes: base_storage,
            replication_overhead_bytes: replication_overhead,
            total_storage_bytes: total_storage,
            overhead_ratio,
        }
    }
}

/// Simulation configuration.
#[derive(Debug, Clone)]
pub struct SimulationConfig {
    /// Number of nodes to simulate.
    pub num_nodes: u64,
    /// Number of edges to simulate.
    pub num_edges: u64,
    /// Number of shards.
    pub num_shards: usize,
    /// Node labels and their distribution.
    pub label_distribution: HashMap<String, f64>,
    /// Edge label distribution (cross-label edges).
    pub edge_distribution: Vec<EdgeTypeConfig>,
}

/// Configuration for an edge type.
#[derive(Debug, Clone)]
pub struct EdgeTypeConfig {
    /// Source node label.
    pub source_label: String,
    /// Target node label.
    pub target_label: String,
    /// Edge label.
    pub edge_label: String,
    /// Proportion of edges of this type.
    pub proportion: f64,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        let mut label_distribution = HashMap::new();
        label_distribution.insert("Person".to_string(), 0.5);
        label_distribution.insert("Place".to_string(), 0.3);
        label_distribution.insert("Event".to_string(), 0.2);

        Self {
            num_nodes: 100_000,
            num_edges: 500_000,
            num_shards: 3,
            label_distribution,
            edge_distribution: vec![
                EdgeTypeConfig {
                    source_label: "Person".to_string(),
                    target_label: "Person".to_string(),
                    edge_label: "KNOWS".to_string(),
                    proportion: 0.4, // 40% person-person
                },
                EdgeTypeConfig {
                    source_label: "Person".to_string(),
                    target_label: "Place".to_string(),
                    edge_label: "VISITED".to_string(),
                    proportion: 0.3, // 30% person-place
                },
                EdgeTypeConfig {
                    source_label: "Person".to_string(),
                    target_label: "Event".to_string(),
                    edge_label: "ATTENDED".to_string(),
                    proportion: 0.2, // 20% person-event
                },
                EdgeTypeConfig {
                    source_label: "Event".to_string(),
                    target_label: "Place".to_string(),
                    edge_label: "OCCURRED_AT".to_string(),
                    proportion: 0.1, // 10% event-place
                },
            ],
        }
    }
}

/// Sharding strategy to simulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShardingStrategy {
    /// Domain-based partitioning (by node label).
    DomainBased,
    /// Hash-based partitioning (by node ID hash).
    HashBased,
    /// Random assignment.
    Random,
}

/// Simulator for analyzing sharding strategies.
pub struct ShardingSimulation {
    config: SimulationConfig,
    /// Label to shard assignment (for domain-based).
    label_to_shard: HashMap<String, ShardId>,
}

impl ShardingSimulation {
    /// Create a new simulation with the given config.
    pub fn new(config: SimulationConfig) -> Self {
        Self {
            config,
            label_to_shard: HashMap::new(),
        }
    }

    /// Create a simulation with default config.
    pub fn with_defaults() -> Self {
        Self::new(SimulationConfig::default())
    }

    /// Set the label-to-shard mapping for domain-based sharding.
    pub fn set_domain_mapping(&mut self, mapping: HashMap<String, ShardId>) {
        self.label_to_shard = mapping;
    }

    /// Run the simulation with the specified strategy.
    pub fn run(&self, strategy: ShardingStrategy) -> SimulationResult {
        let mut result = SimulationResult::new(
            self.config.num_nodes,
            self.config.num_edges,
            self.config.num_shards,
        );

        // Initialize shard counts
        for i in 0..self.config.num_shards {
            let shard_id = ShardId::new_unchecked(i as u16);
            result.nodes_per_shard.insert(shard_id, 0);
            result.edges_per_shard.insert(shard_id, 0);
        }

        // Simulate node distribution
        let node_distribution = self.simulate_node_distribution(strategy);
        for (shard_id, count) in &node_distribution {
            result.nodes_per_shard.insert(*shard_id, *count);
        }

        // Simulate edge distribution and count cuts
        let (local_edges, cross_shard_edges, shard_pair_cuts) =
            self.simulate_edge_distribution(strategy, &node_distribution);

        result.edge_cuts = EdgeCutAnalysis::new(cross_shard_edges, local_edges);
        for ((s1, s2), count) in shard_pair_cuts {
            result.edge_cuts.add_shard_pair_cut(s1, s2, count);
        }

        // Calculate edges per shard (local + replicated cross-shard)
        self.calculate_edges_per_shard(&mut result);

        // Estimate latencies
        result.latency_estimates =
            LatencyEstimates::estimate(result.edge_cuts.cross_shard_ratio, self.config.num_shards);

        // Calculate storage overhead
        result.storage_analysis = StorageAnalysis::calculate(
            self.config.num_nodes,
            self.config.num_edges,
            cross_shard_edges,
        );

        result
    }

    /// Simulate node distribution across shards.
    fn simulate_node_distribution(&self, strategy: ShardingStrategy) -> HashMap<ShardId, u64> {
        let mut distribution = HashMap::new();

        match strategy {
            ShardingStrategy::DomainBased => {
                // Distribute based on label -> shard mapping
                for (label, proportion) in &self.config.label_distribution {
                    let node_count = (self.config.num_nodes as f64 * proportion) as u64;
                    let shard_id = self
                        .label_to_shard
                        .get(label)
                        .copied()
                        .unwrap_or_else(|| ShardId::new_unchecked(0));
                    *distribution.entry(shard_id).or_insert(0) += node_count;
                }
            }
            ShardingStrategy::HashBased | ShardingStrategy::Random => {
                // Evenly distribute across all shards
                let nodes_per_shard = self.config.num_nodes / self.config.num_shards as u64;
                let remainder = self.config.num_nodes % self.config.num_shards as u64;

                for i in 0..self.config.num_shards {
                    let shard_id = ShardId::new_unchecked(i as u16);
                    let extra = if (i as u64) < remainder { 1 } else { 0 };
                    distribution.insert(shard_id, nodes_per_shard + extra);
                }
            }
        }

        distribution
    }

    /// Simulate edge distribution and calculate edge cuts.
    fn simulate_edge_distribution(
        &self,
        strategy: ShardingStrategy,
        _node_distribution: &HashMap<ShardId, u64>,
    ) -> (u64, u64, HashMap<(ShardId, ShardId), u64>) {
        let mut local_edges = 0u64;
        let mut cross_shard_edges = 0u64;
        let mut shard_pair_cuts: HashMap<(ShardId, ShardId), u64> = HashMap::new();

        for edge_type in &self.config.edge_distribution {
            let edge_count = (self.config.num_edges as f64 * edge_type.proportion) as u64;

            match strategy {
                ShardingStrategy::DomainBased => {
                    let source_shard = self
                        .label_to_shard
                        .get(&edge_type.source_label)
                        .copied()
                        .unwrap_or_else(|| ShardId::new_unchecked(0));
                    let target_shard = self
                        .label_to_shard
                        .get(&edge_type.target_label)
                        .copied()
                        .unwrap_or_else(|| ShardId::new_unchecked(0));

                    if source_shard == target_shard {
                        local_edges += edge_count;
                    } else {
                        cross_shard_edges += edge_count;
                        // Normalize key ordering
                        let key = if source_shard.as_u16() <= target_shard.as_u16() {
                            (source_shard, target_shard)
                        } else {
                            (target_shard, source_shard)
                        };
                        *shard_pair_cuts.entry(key).or_insert(0) += edge_count;
                    }
                }
                ShardingStrategy::HashBased | ShardingStrategy::Random => {
                    // With hash-based or random, edges cross shards with probability
                    // (num_shards - 1) / num_shards
                    let cross_probability =
                        (self.config.num_shards - 1) as f64 / self.config.num_shards as f64;
                    let cross = (edge_count as f64 * cross_probability) as u64;
                    let local = edge_count - cross;

                    local_edges += local;
                    cross_shard_edges += cross;

                    // Distribute cross-shard edges across shard pairs
                    let num_pairs = (self.config.num_shards * (self.config.num_shards - 1)) / 2;
                    if num_pairs > 0 {
                        let edges_per_pair = cross / num_pairs as u64;
                        for i in 0..self.config.num_shards {
                            for j in (i + 1)..self.config.num_shards {
                                let key = (
                                    ShardId::new_unchecked(i as u16),
                                    ShardId::new_unchecked(j as u16),
                                );
                                *shard_pair_cuts.entry(key).or_insert(0) += edges_per_pair;
                            }
                        }
                    }
                }
            }
        }

        (local_edges, cross_shard_edges, shard_pair_cuts)
    }

    /// Calculate edges per shard including replicated cross-shard edges.
    fn calculate_edges_per_shard(&self, result: &mut SimulationResult) {
        // Start with local edges distributed proportionally to nodes
        let total_nodes: u64 = result.nodes_per_shard.values().sum();
        if total_nodes == 0 {
            return;
        }

        for (shard_id, node_count) in &result.nodes_per_shard {
            let node_ratio = *node_count as f64 / total_nodes as f64;
            let local_edge_share = (result.edge_cuts.local_edges as f64 * node_ratio) as u64;
            result.edges_per_shard.insert(*shard_id, local_edge_share);
        }

        // Add replicated cross-shard edges (stored on both endpoints)
        for ((shard1, shard2), count) in &result.edge_cuts.cuts_by_shard_pair {
            *result.edges_per_shard.entry(*shard1).or_insert(0) += count;
            *result.edges_per_shard.entry(*shard2).or_insert(0) += count;
        }
    }

    /// Compare multiple sharding strategies.
    pub fn compare_strategies(&self) -> HashMap<ShardingStrategy, SimulationResult> {
        let mut results = HashMap::new();

        results.insert(
            ShardingStrategy::DomainBased,
            self.run(ShardingStrategy::DomainBased),
        );
        results.insert(
            ShardingStrategy::HashBased,
            self.run(ShardingStrategy::HashBased),
        );
        results.insert(ShardingStrategy::Random, self.run(ShardingStrategy::Random));

        results
    }

    /// Generate a summary report of the simulation.
    pub fn generate_report(&self, result: &SimulationResult) -> String {
        let mut report = String::new();

        report.push_str("=== Sharding Simulation Report ===\n\n");

        report.push_str("Graph Size:\n");
        report.push_str(&format!("  Nodes: {}\n", result.total_nodes));
        report.push_str(&format!("  Edges: {}\n", result.total_edges));
        report.push_str(&format!("  Shards: {}\n\n", result.num_shards));

        report.push_str("Node Distribution:\n");
        for (shard_id, count) in &result.nodes_per_shard {
            report.push_str(&format!("  {}: {} nodes\n", shard_id, count));
        }
        report.push_str(&format!(
            "  Balance ratio (CV): {:.2}%\n\n",
            result.node_balance_ratio() * 100.0
        ));

        report.push_str("Edge Cut Analysis:\n");
        report.push_str(&format!(
            "  Local edges: {}\n",
            result.edge_cuts.local_edges
        ));
        report.push_str(&format!(
            "  Cross-shard edges: {}\n",
            result.edge_cuts.cross_shard_edges
        ));
        report.push_str(&format!(
            "  Cross-shard ratio: {:.2}%\n",
            result.edge_cuts.cross_shard_ratio * 100.0
        ));

        if let Some((pair, count)) = result.edge_cuts.most_connected_pair() {
            report.push_str(&format!(
                "  Most connected pair: {} <-> {} ({} edges)\n\n",
                pair.0, pair.1, count
            ));
        }

        report.push_str("Latency Estimates:\n");
        report.push_str(&format!(
            "  Single node lookup: {:.1} µs\n",
            result.latency_estimates.single_node_lookup_us
        ));
        report.push_str(&format!(
            "  Single hop traversal: {:.1} µs\n",
            result.latency_estimates.single_hop_us
        ));
        report.push_str(&format!(
            "  Multi-hop (avg): {:.1} µs\n",
            result.latency_estimates.multi_hop_us
        ));
        report.push_str(&format!(
            "  3-hop estimated: {:.1} µs\n\n",
            result.latency_estimates.three_hop_estimated_us
        ));

        report.push_str("Storage Analysis:\n");
        report.push_str(&format!(
            "  Base storage: {:.2} MB\n",
            result.storage_analysis.base_storage_bytes as f64 / 1_000_000.0
        ));
        report.push_str(&format!(
            "  Replication overhead: {:.2} MB\n",
            result.storage_analysis.replication_overhead_bytes as f64 / 1_000_000.0
        ));
        report.push_str(&format!(
            "  Total storage: {:.2} MB\n",
            result.storage_analysis.total_storage_bytes as f64 / 1_000_000.0
        ));
        report.push_str(&format!(
            "  Overhead ratio: {:.2}%\n",
            result.storage_analysis.overhead_ratio * 100.0
        ));

        report
    }
}

/// Calculate the coefficient of variation for a set of values.
fn coefficient_of_variation(values: &[u64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    let mean = values.iter().sum::<u64>() as f64 / values.len() as f64;
    if mean == 0.0 {
        return 0.0;
    }

    let variance = values
        .iter()
        .map(|&x| {
            let diff = x as f64 - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;

    variance.sqrt() / mean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_simulation() -> ShardingSimulation {
        let mut sim = ShardingSimulation::with_defaults();

        let mut mapping = HashMap::new();
        mapping.insert("Person".to_string(), ShardId::new_unchecked(0));
        mapping.insert("Place".to_string(), ShardId::new_unchecked(1));
        mapping.insert("Event".to_string(), ShardId::new_unchecked(2));
        sim.set_domain_mapping(mapping);

        sim
    }

    #[test]
    fn test_simulation_result_balance_ratio() {
        let mut result = SimulationResult::new(100, 200, 3);
        result.nodes_per_shard.insert(ShardId::new_unchecked(0), 40);
        result.nodes_per_shard.insert(ShardId::new_unchecked(1), 30);
        result.nodes_per_shard.insert(ShardId::new_unchecked(2), 30);

        let ratio = result.node_balance_ratio();
        assert!(ratio > 0.0);
        assert!(ratio < 0.5); // Relatively balanced
    }

    #[test]
    fn test_edge_cut_analysis() {
        let mut analysis = EdgeCutAnalysis::new(300, 700);
        assert_eq!(analysis.cross_shard_edges, 300);
        assert_eq!(analysis.local_edges, 700);
        assert!((analysis.cross_shard_ratio - 0.3).abs() < 0.01);

        analysis.add_shard_pair_cut(ShardId::new_unchecked(0), ShardId::new_unchecked(1), 100);
        analysis.add_shard_pair_cut(ShardId::new_unchecked(1), ShardId::new_unchecked(0), 50); // Same pair

        let (pair, count) = analysis.most_connected_pair().unwrap();
        assert_eq!(count, 150);
    }

    #[test]
    fn test_latency_estimates() {
        let estimates = LatencyEstimates::estimate(0.3, 3);

        assert!(estimates.single_node_lookup_us < 10.0);
        assert!(estimates.single_hop_us < 100.0);
        assert!(estimates.multi_hop_us > estimates.single_hop_us);
        assert!(estimates.three_hop_estimated_us > estimates.multi_hop_us);
    }

    #[test]
    fn test_latency_estimates_zero_cross_shard() {
        let estimates = LatencyEstimates::estimate(0.0, 3);

        // With no cross-shard edges, multi-hop should equal single-hop * hops
        assert!((estimates.multi_hop_us - estimates.single_hop_us).abs() < 1.0);
    }

    #[test]
    fn test_storage_analysis() {
        let analysis = StorageAnalysis::calculate(1000, 5000, 1000);

        assert!(analysis.base_storage_bytes > 0);
        assert!(analysis.replication_overhead_bytes > 0);
        assert_eq!(
            analysis.total_storage_bytes,
            analysis.base_storage_bytes + analysis.replication_overhead_bytes
        );
        assert!(analysis.overhead_ratio < 0.5);
    }

    #[test]
    fn test_storage_analysis_no_cross_shard() {
        let analysis = StorageAnalysis::calculate(1000, 5000, 0);

        assert_eq!(analysis.replication_overhead_bytes, 0);
        assert_eq!(analysis.total_storage_bytes, analysis.base_storage_bytes);
        assert_eq!(analysis.overhead_ratio, 0.0);
    }

    #[test]
    fn test_simulation_domain_based() {
        let sim = create_test_simulation();
        let result = sim.run(ShardingStrategy::DomainBased);

        assert_eq!(result.total_nodes, 100_000);
        assert_eq!(result.total_edges, 500_000);
        assert_eq!(result.num_shards, 3);

        // Domain-based should have lower cross-shard ratio than random
        // because same-label edges stay local
        assert!(result.edge_cuts.cross_shard_ratio < 0.7);
    }

    #[test]
    fn test_simulation_hash_based() {
        let sim = create_test_simulation();
        let result = sim.run(ShardingStrategy::HashBased);

        // Hash-based distributes evenly
        let min_nodes = result.nodes_per_shard.values().min().unwrap();
        let max_nodes = result.nodes_per_shard.values().max().unwrap();
        assert!((*max_nodes - *min_nodes) < 100); // Very balanced

        // But has high cross-shard ratio
        assert!(result.edge_cuts.cross_shard_ratio > 0.5);
    }

    #[test]
    fn test_simulation_comparison() {
        let sim = create_test_simulation();
        let results = sim.compare_strategies();

        assert_eq!(results.len(), 3);
        assert!(results.contains_key(&ShardingStrategy::DomainBased));
        assert!(results.contains_key(&ShardingStrategy::HashBased));
        assert!(results.contains_key(&ShardingStrategy::Random));

        // Domain-based should have lower cross-shard ratio
        let domain_ratio = results[&ShardingStrategy::DomainBased]
            .edge_cuts
            .cross_shard_ratio;
        let hash_ratio = results[&ShardingStrategy::HashBased]
            .edge_cuts
            .cross_shard_ratio;

        assert!(domain_ratio < hash_ratio);
    }

    #[test]
    fn test_simulation_report() {
        let sim = create_test_simulation();
        let result = sim.run(ShardingStrategy::DomainBased);
        let report = sim.generate_report(&result);

        assert!(report.contains("Sharding Simulation Report"));
        assert!(report.contains("Nodes:"));
        assert!(report.contains("Edges:"));
        assert!(report.contains("Cross-shard ratio:"));
        assert!(report.contains("Latency Estimates:"));
        assert!(report.contains("Storage Analysis:"));
    }

    #[test]
    fn test_coefficient_of_variation() {
        // Equal values should have CV = 0
        assert_eq!(coefficient_of_variation(&[100, 100, 100]), 0.0);

        // Empty should return 0
        assert_eq!(coefficient_of_variation(&[]), 0.0);

        // All zeros should return 0
        assert_eq!(coefficient_of_variation(&[0, 0, 0]), 0.0);

        // Unequal values should have CV > 0
        let cv = coefficient_of_variation(&[100, 200, 300]);
        assert!(cv > 0.0);
        assert!(cv < 1.0);
    }

    #[test]
    fn test_simulation_config_default() {
        let config = SimulationConfig::default();
        assert_eq!(config.num_nodes, 100_000);
        assert_eq!(config.num_edges, 500_000);
        assert_eq!(config.num_shards, 3);
        assert!(!config.label_distribution.is_empty());
        assert!(!config.edge_distribution.is_empty());
    }

    #[test]
    fn test_simulation_empty_graph() {
        let mut config = SimulationConfig::default();
        config.num_nodes = 0;
        config.num_edges = 0;

        let sim = ShardingSimulation::new(config);
        let result = sim.run(ShardingStrategy::DomainBased);

        assert_eq!(result.total_nodes, 0);
        assert_eq!(result.total_edges, 0);
        assert_eq!(result.edge_cuts.cross_shard_ratio, 0.0);
    }

    #[test]
    fn test_sharding_strategy_equality() {
        assert_eq!(ShardingStrategy::DomainBased, ShardingStrategy::DomainBased);
        assert_ne!(ShardingStrategy::DomainBased, ShardingStrategy::HashBased);
    }
}
