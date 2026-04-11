# 🔭 Vantage Spec: Distributed Replication (The "Resilience" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-011 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/storage/replication/` (To be created) |

## 1. 👤 User Stories

> **As a** DevOps Engineer, Database Administrator or SRE,
> **I want to** deploy AletheiaDB across multiple nodes in an active-passive or active-active configuration with automated cross-node replication and failover,
> **So that** my application can survive a single-node hardware failure without losing data or experiencing significant downtime, ensuring my mission-critical temporal graph and vector queries remain accessible and durable even if a primary server crashes or experiences a network partition.

> **As a** Solutions Architect,
> **I want to** route read-heavy analytical queries to read-only replicas,
> **So that** I don't impact the performance of the primary write node handling real-time ingestion.

> **As a** Global Platform Manager,
> **I want to** replicate my knowledge graph to data centers in different geographic regions,
> **So that** my users globally experience low-latency reads.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB currently runs as a single-node database. While it is highly performant and supports single-node persistence (via WAL) and horizontal sharding, a single node is a single point of failure (SPOF). It lacks built-in replica sets for high availability (HA).

**The Gap:**
- **Reliability:** Hardware fails. Cloud VMs restart. Without replication, a node going down means total downtime. Enterprise users cannot rely on a database for production RAG (Retrieval-Augmented Generation) applications or compliance auditing if a single machine failure causes downtime.
- **Read Scaling:** All read queries hit the same node that processes writes, creating a bottleneck for read-heavy workloads (like semantic search and historical traversals).
- **Enterprise Readiness:** Mission-critical applications require High Availability (HA) guarantees that we currently cannot provide. High Availability Replication solves the "single point of failure" problem, moving AletheiaDB from a "research database" to a "production-ready enterprise database."

**ROI:**
- **Enterprise Adoption:** Unlocks deals with enterprise customers who strictly require HA and Disaster Recovery (DR) capabilities, ensuring 99.99% uptime.
- **Performance:** Dramatically improves read latency and throughput by allowing read queries to be distributed across a cluster.
- **Trust:** Ensures zero data loss (RPO = 0) during unplanned outages in synchronous replication setups.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Topology Configuration**:
    -   Must support configuring one Leader node and N Follower (Replica) nodes.
    -   Followers must be able to connect to the Leader and stream WAL (Write-Ahead Log) updates.

2.  **Replication Modes**:
    -   **Asynchronous**: Leader acknowledges writes immediately and streams to followers in the background.
    -   **Synchronous**: Leader waits for acknowledgement from a quorum of followers before acknowledging the write to the client.

3.  **Read Scaling**:
    -   Follower nodes must be able to serve read-only queries (e.g., `MATCH`, `SIMILAR TO`, `AS OF`).
    -   If a write query is sent to a Follower, it must either be rejected or proxied to the Leader.

4.  **Failover**:
    -   Must define a protocol for automated or manual Leader Election and Failover.

5.  **Observability**:
    -   Must expose metrics for replication lag (in seconds and WAL LSN offset).
    -   Must expose cluster state and node roles via the `/status` HTTP API.

### Non-Functional Requirements
-   **Metric Definition:**
    -   **Uptime:** The database cluster remains available for reads and writes even if a single replica or leader node fails.
    -   **Failover Latency:** Automatic failover (if implemented) or manual promotion takes <5 seconds.
    -   **Replication Latency:** Asynchronous replication must add < 2ms overhead to the Leader's write path.
-   **Consistency**: A Follower serving a read query must guarantee causal consistency (Read-Your-Writes) if configured to do so.

## 4. 🚫 Out of Scope (Phase 1)

-   **Multi-Master (Active-Active)**: Conflict resolution is too complex for our bi-temporal model right now. We stick to Single-Leader.
-   **Partial Replication**: Replicating only specific shards/graphs to specific nodes. The entire dataset is replicated.
-   **Automatic Failover (Leader Election)**: Implementing Raft/Paxos for automatic leader election is complex. We will rely on external tools (like Consul/ZooKeeper) or manual intervention for Phase 1.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **WAL Arch** | Exists, local only | Network-streamable | Implement WAL streaming protocol over TCP/gRPC |
| **Node Roles** | All nodes are Master | Leader / Follower | Add node state machine and configuration |
| **Read Routing**| None | Read-only endpoints | Enforce read-only transactions on followers |
| **Metrics** | Local metrics | Replication metrics | Add lag/offset metrics to observability suite |

## 6. 📅 Execution Plan

1.  **Replication Protocol**: Define the wire protocol for streaming WAL records from Leader to Follower.
2.  **Follower State Machine**: Implement a process on the Follower that applies incoming WAL records to its local storage.
3.  **Read-Only Mode**: Ensure the storage layer can be started in a strictly read-only mode for Followers.
4.  **Configuration & Setup**: Add CLI flags and TOML configuration options for clustering (e.g., `role = "follower"`, `leader_addr = "..."`).
5.  **Metrics Integration**: Wire up replication lag metrics to the existing Prometheus/tracing infrastructure.
