//! Cost Model for Query Optimization
//!
//! Provides cost estimation for physical operators, enabling the planner
//! to choose the most efficient execution strategy.
//!
//! Cost values are calibrated from AletheiaDB benchmarks:
//! - Node lookup: ~0.5µs
//! - Single-hop traversal: ~1µs
//! - HNSW search per k: ~0.3µs (log scale with index size)
//! - Temporal delta reconstruction: ~10µs per delta
//! - Filter evaluation: ~0.1µs per row

use super::physical::PhysicalOp;
use super::stats::Statistics;

/// Estimated cost of executing a query plan.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cost {
    /// CPU cost in abstract units (roughly microseconds)
    pub cpu: f64,
    /// I/O cost (storage accesses)
    pub io: f64,
    /// Memory cost in bytes
    pub memory: usize,
    /// Network cost (for future distributed support)
    pub network: f64,
}

impl Cost {
    /// Create a new cost with all zero values
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::Cost;
    ///
    /// let cost = Cost::zero();
    /// assert_eq!(cost.cpu, 0.0);
    /// assert_eq!(cost.io, 0.0);
    /// ```
    #[must_use]
    pub fn zero() -> Self {
        Cost::default()
    }

    /// Calculate total weighted cost for comparison
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::{Cost, CostWeights};
    ///
    /// let cost = Cost { cpu: 10.0, io: 5.0, memory: 100, network: 0.0 };
    /// let weights = CostWeights { cpu_weight: 1.0, io_weight: 10.0, memory_weight: 0.1, network_weight: 1.0 };
    ///
    /// // 10.0 * 1.0 + 5.0 * 10.0 + 100.0 * 0.1 = 10.0 + 50.0 + 10.0 = 70.0
    /// assert_eq!(cost.total(&weights), 70.0);
    /// ```
    #[must_use]
    pub fn total(&self, weights: &CostWeights) -> f64 {
        self.cpu * weights.cpu_weight
            + self.io * weights.io_weight
            + (self.memory as f64) * weights.memory_weight
            + self.network * weights.network_weight
    }

    /// Add two costs together
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::Cost;
    ///
    /// let c1 = Cost { cpu: 1.0, io: 2.0, memory: 100, network: 0.0 };
    /// let c2 = Cost { cpu: 2.0, io: 3.0, memory: 50, network: 0.0 };
    ///
    /// let sum = c1.add(&c2);
    /// assert_eq!(sum.cpu, 3.0);
    /// assert_eq!(sum.io, 5.0);
    /// assert_eq!(sum.memory, 150);
    /// ```
    #[must_use]
    pub fn add(&self, other: &Cost) -> Cost {
        Cost {
            cpu: self.cpu + other.cpu,
            io: self.io + other.io,
            memory: self.memory.saturating_add(other.memory),
            network: self.network + other.network,
        }
    }

    /// Scale cost by a factor
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::Cost;
    ///
    /// let cost = Cost { cpu: 2.0, io: 4.0, memory: 100, network: 0.0 };
    /// let scaled = cost.scale(1.5);
    ///
    /// assert_eq!(scaled.cpu, 3.0);
    /// assert_eq!(scaled.io, 6.0);
    /// assert_eq!(scaled.memory, 150);
    /// ```
    #[must_use]
    pub fn scale(&self, factor: f64) -> Cost {
        Cost {
            cpu: self.cpu * factor,
            io: self.io * factor,
            memory: (self.memory as f64 * factor) as usize,
            network: self.network * factor,
        }
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;

    fn add(self, rhs: Self) -> Self::Output {
        Cost::add(&self, &rhs)
    }
}

/// Weights for cost components.
///
/// These weights determine the relative importance of different
/// cost factors when comparing plans.
#[derive(Debug, Clone)]
pub struct CostWeights {
    /// Weight for CPU cost
    pub cpu_weight: f64,
    /// Weight for I/O cost (typically higher than CPU)
    pub io_weight: f64,
    /// Weight for memory usage
    pub memory_weight: f64,
    /// Weight for network cost (for distributed queries)
    pub network_weight: f64,
}

impl Default for CostWeights {
    fn default() -> Self {
        CostWeights {
            cpu_weight: 1.0,
            io_weight: 10.0,       // I/O is typically 10x more expensive than CPU
            memory_weight: 0.001,  // Memory is cheap but limited
            network_weight: 100.0, // Network is very expensive
        }
    }
}

/// Base costs for operations, calibrated from benchmarks.
#[derive(Debug, Clone)]
pub struct OperationCosts {
    /// O(1) node lookup from DashMap (~0.5µs)
    pub node_lookup: f64,
    /// Single-hop traversal via CSR adjacency (~1µs)
    pub single_hop_traversal: f64,
    /// HNSW search base cost per result (~0.3µs)
    pub hnsw_search_per_k: f64,
    /// HNSW search logarithmic factor
    pub hnsw_log_factor: f64,
    /// Temporal delta reconstruction (~10µs per delta)
    pub temporal_delta: f64,
    /// Filter evaluation per row (~0.1µs)
    pub filter_eval: f64,
    /// Vector similarity computation per pair (~0.5µs for 384-dim)
    pub vector_similarity: f64,
    /// Sort cost per element (n log n factor)
    pub sort_per_element: f64,
    /// Hash join build side per element
    pub hash_build: f64,
    /// Hash join probe per element
    pub hash_probe: f64,
    /// Lock acquisition overhead (~2µs for RwLock)
    pub lock_acquisition: f64,
    /// Threshold for using batch iterator (nodes)
    pub batch_threshold: usize,
}

impl Default for OperationCosts {
    fn default() -> Self {
        OperationCosts {
            node_lookup: 0.5,
            single_hop_traversal: 1.0,
            hnsw_search_per_k: 0.3,
            hnsw_log_factor: 1.0,
            temporal_delta: 10.0,
            filter_eval: 0.1,
            vector_similarity: 0.5,
            sort_per_element: 0.01,
            hash_build: 0.1,
            hash_probe: 0.05,
            lock_acquisition: 2.0,
            batch_threshold: 100,
        }
    }
}

/// Cost model for estimating query execution costs.
#[derive(Debug, Clone, Default)]
pub struct CostModel {
    /// Cost weights for combining different factors
    pub weights: CostWeights,
    /// Base operation costs
    pub operation_costs: OperationCosts,
}

impl CostModel {
    /// Create a new cost model with default values
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::CostModel;
    ///
    /// let model = CostModel::new();
    /// assert_eq!(model.weights.cpu_weight, 1.0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a cost model optimized for low-latency queries
    ///
    /// This model penalizes I/O and network operations more heavily to favor
    /// fast, in-memory execution strategies.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::CostModel;
    ///
    /// let model = CostModel::low_latency();
    /// assert!(model.weights.io_weight > 10.0); // Heavily penalizes IO
    /// ```
    #[must_use]
    pub fn low_latency() -> Self {
        CostModel {
            weights: CostWeights {
                cpu_weight: 1.0,
                io_weight: 20.0, // Penalize I/O more
                memory_weight: 0.0001,
                network_weight: 1000.0,
            },
            operation_costs: OperationCosts::default(),
        }
    }

    /// Create a cost model optimized for throughput
    ///
    /// This model is more tolerant of high memory usage and I/O if it means
    /// processing more data efficiently in parallel.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::CostModel;
    ///
    /// let model = CostModel::high_throughput();
    /// assert!(model.weights.cpu_weight < 1.0); // CPU is weighted less heavily
    /// ```
    #[must_use]
    pub fn high_throughput() -> Self {
        CostModel {
            weights: CostWeights {
                cpu_weight: 0.5,
                io_weight: 5.0,
                memory_weight: 0.01, // Allow more memory usage
                network_weight: 50.0,
            },
            operation_costs: OperationCosts::default(),
        }
    }

    /// Determine whether to use batch iterator for temporal lookup.
    ///
    /// Batch iterator is more efficient when the lock acquisition overhead
    /// of per-node iteration exceeds the cost of holding the lock longer.
    ///
    /// Returns true if batch mode should be used.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::CostModel;
    ///
    /// let model = CostModel::new();
    /// // Single lookup is cheaper with per-node lock
    /// assert!(!model.should_use_batch_temporal_lookup(1));
    /// // Large batches are cheaper holding the lock
    /// assert!(model.should_use_batch_temporal_lookup(1000));
    /// ```
    #[must_use]
    pub fn should_use_batch_temporal_lookup(&self, node_count: usize) -> bool {
        node_count >= self.operation_costs.batch_threshold
    }

    /// Estimate the cost of executing a physical operator
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::CostModel;
    /// use aletheiadb::query::planner::stats::Statistics;
    /// use aletheiadb::query::planner::physical::PhysicalOp;
    /// use aletheiadb::core::NodeId;
    ///
    /// let model = CostModel::new();
    /// let stats = Statistics::new();
    /// let op = PhysicalOp::NodeLookup {
    ///     node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()]
    /// };
    ///
    /// let cost = model.estimate(&op, &stats);
    /// assert!(cost.cpu > 0.0);
    /// assert_eq!(cost.io, 0.0); // Simple lookups are assumed in-memory
    /// ```
    #[must_use]
    pub fn estimate(&self, op: &PhysicalOp, stats: &Statistics) -> Cost {
        match op {
            PhysicalOp::NodeLookup { node_ids } => self.estimate_node_lookup(node_ids.len()),

            PhysicalOp::NodeScan { estimated_rows, .. } => self.estimate_node_scan(*estimated_rows),

            // PropertyScan is cheaper than NodeScan+Filter because it skips predicate
            // evaluation on non-matching nodes at the storage level
            PhysicalOp::PropertyScan { estimated_rows, .. } => {
                self.estimate_node_lookup(*estimated_rows)
            }

            PhysicalOp::HnswSearch {
                k, label_filter, ..
            } => self.estimate_hnsw_search(*k, label_filter.is_some(), stats),

            PhysicalOp::TemporalNodeLookup {
                node_ids,
                use_batch,
                ..
            } => self.estimate_temporal_lookup(node_ids.len(), *use_batch, stats),

            PhysicalOp::TemporalVectorSearch { k, .. } => {
                self.estimate_temporal_vector_search(*k, stats)
            }

            PhysicalOp::IndexedTraversal { input, depth, .. } => {
                let input_cost = self.estimate(input, stats);
                let input_card = self.estimate_cardinality(input, stats);
                self.estimate_traversal(input_cost, input_card, *depth, stats)
            }

            PhysicalOp::Filter { input, .. } => {
                let input_cost = self.estimate(input, stats);
                let input_card = self.estimate_cardinality(input, stats);
                self.estimate_filter(input_cost, input_card)
            }

            PhysicalOp::VectorRerank { input, k, .. } => {
                let input_cost = self.estimate(input, stats);
                let input_card = self.estimate_cardinality(input, stats);
                self.estimate_vector_rerank(input_cost, input_card, *k)
            }

            PhysicalOp::Sort { input, .. } => {
                let input_cost = self.estimate(input, stats);
                let input_card = self.estimate_cardinality(input, stats);
                self.estimate_sort(input_cost, input_card)
            }

            PhysicalOp::Limit { input, .. } => {
                // Limit doesn't add much cost, just passes through
                self.estimate(input, stats)
            }

            PhysicalOp::HashJoin { left, right, .. } => {
                let left_cost = self.estimate(left, stats);
                let right_cost = self.estimate(right, stats);
                let left_card = self.estimate_cardinality(left, stats);
                let right_card = self.estimate_cardinality(right, stats);
                self.estimate_hash_join(left_cost, right_cost, left_card, right_card)
            }

            PhysicalOp::Union { left, right }
            | PhysicalOp::Intersect { left, right }
            | PhysicalOp::Except { left, right } => {
                let left_cost = self.estimate(left, stats);
                let right_cost = self.estimate(right, stats);
                left_cost + right_cost
            }

            PhysicalOp::Project { input, .. }
            | PhysicalOp::Distinct { input }
            | PhysicalOp::Count { input }
            | PhysicalOp::Materialize { input }
            | PhysicalOp::TemporalTrack { input, .. } => self.estimate(input, stats),

            PhysicalOp::SimilarToNode {
                k, label_filter, ..
            } => {
                // SimilarToNode = node lookup + HNSW search
                let lookup_cost = self.estimate_node_lookup(1);
                let search_cost = self.estimate_hnsw_search(*k, label_filter.is_some(), stats);
                lookup_cost + search_cost
            }

            PhysicalOp::Empty => Cost::zero(),
        }
    }

    /// Estimate cardinality of a physical operator's output
    ///
    /// ## Examples
    ///
    /// ```rust
    /// use aletheiadb::query::planner::cost::CostModel;
    /// use aletheiadb::query::planner::stats::Statistics;
    /// use aletheiadb::query::planner::physical::PhysicalOp;
    /// use aletheiadb::core::NodeId;
    ///
    /// let model = CostModel::new();
    /// let stats = Statistics::new();
    /// let op = PhysicalOp::NodeLookup {
    ///     node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()]
    /// };
    ///
    /// assert_eq!(model.estimate_cardinality(&op, &stats), 2);
    /// ```
    #[must_use]
    pub fn estimate_cardinality(&self, op: &PhysicalOp, stats: &Statistics) -> usize {
        match op {
            PhysicalOp::NodeLookup { node_ids } => node_ids.len(),
            PhysicalOp::NodeScan { estimated_rows, .. } => *estimated_rows,
            PhysicalOp::PropertyScan { estimated_rows, .. } => *estimated_rows,
            PhysicalOp::HnswSearch { k, .. } => *k,
            PhysicalOp::TemporalNodeLookup { node_ids, .. } => node_ids.len(),
            PhysicalOp::TemporalVectorSearch { k, .. } => *k,
            PhysicalOp::IndexedTraversal { input, depth, .. } => {
                let input_card = self.estimate_cardinality(input, stats);
                let avg_degree = stats.average_out_degree();
                (input_card as f64 * avg_degree.powi(*depth as i32)) as usize
            }
            PhysicalOp::Filter { input, .. } => {
                // Assume 10% selectivity by default
                (self.estimate_cardinality(input, stats) as f64 * 0.1) as usize
            }
            PhysicalOp::VectorRerank { k, .. } => *k,
            PhysicalOp::Limit { count, input, .. } => {
                (*count).min(self.estimate_cardinality(input, stats))
            }
            PhysicalOp::Sort { input, .. } | PhysicalOp::Project { input, .. } => {
                self.estimate_cardinality(input, stats)
            }
            PhysicalOp::Distinct { input } => {
                // Assume 50% unique
                self.estimate_cardinality(input, stats) / 2
            }
            PhysicalOp::Count { .. } => 1,
            PhysicalOp::HashJoin { left, right, .. } => {
                // Assume 10% of cross product
                let left_card = self.estimate_cardinality(left, stats);
                let right_card = self.estimate_cardinality(right, stats);
                (left_card as f64 * right_card as f64 * 0.1) as usize
            }
            PhysicalOp::Union { left, right } => {
                self.estimate_cardinality(left, stats) + self.estimate_cardinality(right, stats)
            }
            PhysicalOp::Intersect { left, right } => self
                .estimate_cardinality(left, stats)
                .min(self.estimate_cardinality(right, stats)),
            PhysicalOp::Except { left, .. } => self.estimate_cardinality(left, stats),
            PhysicalOp::Materialize { input } | PhysicalOp::TemporalTrack { input, .. } => {
                self.estimate_cardinality(input, stats)
            }
            PhysicalOp::SimilarToNode { k, .. } => *k,
            PhysicalOp::Empty => 0,
        }
    }

    // Private estimation methods

    fn estimate_node_lookup(&self, count: usize) -> Cost {
        Cost {
            cpu: self.operation_costs.node_lookup * count as f64,
            io: 0.0,                                         // In-memory
            memory: count * std::mem::size_of::<u64>() * 10, // Rough estimate per node
            network: 0.0,
        }
    }

    fn estimate_node_scan(&self, estimated_rows: usize) -> Cost {
        Cost {
            cpu: self.operation_costs.node_lookup * estimated_rows as f64,
            io: (estimated_rows as f64 / 1000.0).ceil(), // Batch I/O
            memory: estimated_rows * std::mem::size_of::<u64>() * 10,
            network: 0.0,
        }
    }

    fn estimate_hnsw_search(&self, k: usize, has_filter: bool, stats: &Statistics) -> Cost {
        let vector_count = stats.vector_count().max(1) as f64;
        let log_factor = vector_count.log2().max(1.0);
        let filter_overhead = if has_filter { 2.0 } else { 1.0 };

        Cost {
            cpu: self.operation_costs.hnsw_search_per_k
                * k as f64
                * log_factor
                * self.operation_costs.hnsw_log_factor
                * filter_overhead,
            io: 0.0,
            memory: k * std::mem::size_of::<(u64, f32)>(),
            network: 0.0,
        }
    }

    fn estimate_temporal_lookup(&self, count: usize, use_batch: bool, stats: &Statistics) -> Cost {
        let avg_delta_chain = stats.average_delta_chain_length().max(1.0);

        // Calculate lock overhead based on iterator strategy:
        // - Per-node iterator: acquires lock for each node lookup
        // - Batch iterator: acquires lock once but holds it longer
        let lock_overhead = if use_batch {
            // Batch: single lock acquisition
            self.operation_costs.lock_acquisition
        } else {
            // Per-node: lock acquisition per node
            self.operation_costs.lock_acquisition * count as f64
        };

        Cost {
            cpu: lock_overhead
                + self.operation_costs.temporal_delta * avg_delta_chain * count as f64,
            io: avg_delta_chain * count as f64,
            memory: count * std::mem::size_of::<u64>() * 20, // Larger due to versioning
            network: 0.0,
        }
    }

    fn estimate_temporal_vector_search(&self, k: usize, stats: &Statistics) -> Cost {
        // Temporal vector search uses snapshots, so similar to regular HNSW
        // but may need to check multiple snapshots
        let base = self.estimate_hnsw_search(k, false, stats);
        Cost {
            cpu: base.cpu * 1.5, // Snapshot lookup overhead
            io: base.io + 1.0,   // Snapshot access
            memory: base.memory,
            network: 0.0,
        }
    }

    fn estimate_traversal(
        &self,
        input_cost: Cost,
        input_card: usize,
        depth: usize,
        stats: &Statistics,
    ) -> Cost {
        let avg_degree = stats.average_out_degree();
        let traversal_factor = avg_degree.powi(depth as i32);

        Cost {
            cpu: input_cost.cpu
                + self.operation_costs.single_hop_traversal * input_card as f64 * traversal_factor,
            io: input_cost.io,
            memory: input_cost.memory
                + (input_card as f64 * traversal_factor) as usize * std::mem::size_of::<u64>(),
            network: 0.0,
        }
    }

    fn estimate_filter(&self, input_cost: Cost, input_card: usize) -> Cost {
        Cost {
            cpu: input_cost.cpu + self.operation_costs.filter_eval * input_card as f64,
            io: input_cost.io,
            memory: input_cost.memory,
            network: 0.0,
        }
    }

    fn estimate_vector_rerank(&self, input_cost: Cost, input_card: usize, k: usize) -> Cost {
        Cost {
            cpu: input_cost.cpu + self.operation_costs.vector_similarity * input_card as f64,
            io: input_cost.io,
            memory: input_cost.memory + k * std::mem::size_of::<(u64, f32)>(),
            network: 0.0,
        }
    }

    fn estimate_sort(&self, input_cost: Cost, input_card: usize) -> Cost {
        let n = input_card.max(1) as f64;
        let nlogn = n * n.log2();

        Cost {
            cpu: input_cost.cpu + self.operation_costs.sort_per_element * nlogn,
            io: input_cost.io,
            memory: input_cost.memory + input_card * std::mem::size_of::<u64>(),
            network: 0.0,
        }
    }

    fn estimate_hash_join(
        &self,
        left_cost: Cost,
        right_cost: Cost,
        left_card: usize,
        right_card: usize,
    ) -> Cost {
        // Build phase on smaller side, probe on larger
        let (build_card, probe_card) = if left_card < right_card {
            (left_card, right_card)
        } else {
            (right_card, left_card)
        };

        Cost {
            cpu: left_cost.cpu
                + right_cost.cpu
                + self.operation_costs.hash_build * build_card as f64
                + self.operation_costs.hash_probe * probe_card as f64,
            io: left_cost.io + right_cost.io,
            memory: left_cost.memory
                + right_cost.memory
                + build_card * std::mem::size_of::<u64>() * 2,
            network: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NodeId;
    use std::sync::Arc;

    fn test_stats() -> Statistics {
        Statistics::default()
    }

    #[test]
    fn test_cost_arithmetic() {
        let a = Cost {
            cpu: 1.0,
            io: 2.0,
            memory: 100,
            network: 0.0,
        };
        let b = Cost {
            cpu: 0.5,
            io: 1.0,
            memory: 50,
            network: 0.0,
        };

        let sum = a.add(&b);
        assert_eq!(sum.cpu, 1.5);
        assert_eq!(sum.io, 3.0);
        assert_eq!(sum.memory, 150);

        let scaled = a.scale(2.0);
        assert_eq!(scaled.cpu, 2.0);
        assert_eq!(scaled.io, 4.0);
        assert_eq!(scaled.memory, 200);
    }

    #[test]
    fn test_cost_total() {
        let cost = Cost {
            cpu: 10.0,
            io: 5.0,
            memory: 1000,
            network: 0.0,
        };
        let weights = CostWeights::default();

        let total = cost.total(&weights);
        // cpu * 1.0 + io * 10.0 + memory * 0.001 + network * 100.0
        // = 10.0 + 50.0 + 1.0 + 0.0 = 61.0
        assert!((total - 61.0).abs() < 0.01);
    }

    #[test]
    fn test_node_lookup_cost() {
        let model = CostModel::default();
        let stats = test_stats();

        let op = PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()],
        };

        let cost = model.estimate(&op, &stats);
        assert!(cost.cpu > 0.0);
        assert_eq!(cost.io, 0.0); // In-memory
    }

    #[test]
    fn test_hnsw_search_cost() {
        let model = CostModel::default();
        let stats = test_stats();

        let op = PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: None,
        };

        let cost = model.estimate(&op, &stats);
        assert!(cost.cpu > 0.0);
    }

    #[test]
    fn test_traversal_cost() {
        let model = CostModel::default();
        let stats = test_stats();

        let op = PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
            }),
            direction: crate::query::ir::Direction::Outgoing,
            label: None,
            depth: 2,
            temporal_context: None,
        };

        let cost = model.estimate(&op, &stats);
        assert!(cost.cpu > 0.0);
    }

    #[test]
    fn test_cardinality_estimation() {
        let model = CostModel::default();
        let stats = test_stats();

        let lookup = PhysicalOp::NodeLookup {
            node_ids: vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()],
        };
        assert_eq!(model.estimate_cardinality(&lookup, &stats), 2);

        let search = PhysicalOp::HnswSearch {
            embedding: Arc::from([0.1f32; 4].as_slice()),
            k: 10,
            label_filter: None,
            property_key: None,
        };
        assert_eq!(model.estimate_cardinality(&search, &stats), 10);

        let limit = PhysicalOp::Limit {
            input: Box::new(search),
            count: 5,
            offset: 0,
        };
        assert_eq!(model.estimate_cardinality(&limit, &stats), 5);
    }
}
