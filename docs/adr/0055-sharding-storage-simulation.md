# ADR-0055: Sharding Storage Simulation

**Status:** Proposed
**Date:** 2026-03-24
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, storage, sharding, performance

## Context

When distributing a graph database across multiple shards (as outlined in ADR-0014), one of the key trade-offs is storage overhead. Specifically, edge replication is used to maintain single-hop traversal performance across shard boundaries.

AletheiaDB replicates edges when the source and target nodes reside on different shards. While this guarantees low-latency graph traversals, it introduces a storage penalty because cross-shard edges exist in two places.

To make informed decisions about shard sizes, domain partitioning, and overall cluster capacity planning, administrators need visibility into this storage overhead. Without a way to simulate and quantify the impact of different sharding strategies on storage, users risk either under-provisioning storage or creating highly imbalanced clusters.

## Decision

We have implemented a storage analysis component within the existing `ShardingSimulation` module (`src/storage/sharding/simulation.rs`).

This component introduces the `StorageAnalysis` struct, which calculates the total storage requirements for a given graph topology and sharding strategy, breaking down the costs into:
- **Base Storage**: The raw size of all nodes and edges.
- **Replication Overhead**: The extra storage consumed by replicating cross-shard edges.
- **Overhead Ratio**: The percentage of extra storage required due to sharding.

### Simulation Methodology

The simulation calculates storage using average byte sizes for nodes (256 bytes) and edges (64 bytes). It executes the sharding simulation to determine the number of cross-shard edges, and then applies these constants to yield precise storage metrics in bytes and megabytes.

## Consequences

### Positive

- **Capacity Planning**: Administrators can accurately predict storage requirements before deploying a sharded cluster or executing a rebalance.
- **Strategy Evaluation**: Different sharding strategies (e.g., Domain-Based vs Hash-Based) can be quantitatively compared not just on query latency (edge cuts) but also on storage efficiency.
- **Visibility**: The simulation report now provides clear, human-readable metrics (in MB) for base storage, overhead, and total storage.

### Negative

- **Approximation Error**: The simulation relies on constant average sizes (256 bytes for nodes, 64 bytes for edges). Actual storage overhead will vary depending on property density, string lengths, and vector embeddings.
- **Computational Cost**: Running large-scale simulations (millions of nodes) to calculate these metrics consumes memory and CPU, though it is an offline administrative task.

## Alternatives Considered

### Alternative 1: Dynamic Sampling

Instead of using constant average sizes, sample actual node and edge sizes from the live database during simulation.

* **Why not:** Significantly increases the complexity and runtime of the simulation. For capacity planning and strategy comparison, relative overhead ratios based on averages provide sufficient signal without the heavy I/O cost of live sampling.

### Alternative 2: No Simulation (Post-Facto Measurement)

Rely entirely on measuring actual disk usage after data is ingested or rebalanced.

* **Why not:** Violates the "Performance First" and predictability goals. Administrators need to know if a rebalance will exceed available disk space *before* it happens.
