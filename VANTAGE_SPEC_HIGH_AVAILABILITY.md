# 🔭 Vantage: Spec for High Availability Replication

## 👤 User Story
**As a** Database Administrator or SRE,
**I want** to deploy AletheiaDB with automated cross-node replication and failover,
**so that** my mission-critical temporal graph and vector queries remain accessible and durable even if a primary server crashes or experiences a network partition.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
While AletheiaDB currently supports single-node persistence (via WAL) and horizontal sharding, it lacks built-in replica sets for high availability (HA). Enterprise users cannot rely on a database for production RAG (Retrieval-Augmented Generation) applications or compliance auditing if a single machine failure causes downtime. High Availability Replication solves the "single point of failure" problem, ensuring 99.99% uptime and zero data loss (RPO=0) during unplanned outages. This feature moves AletheiaDB from a "research database" to a "production-ready enterprise database."

**Success Metric Definition:**
- **Availability:** The cluster remains fully operational for read and write queries even if a single node fails.
- **Failover Latency:** Automatic leader election and failover complete in <5 seconds.
- **Replication Lag:** Synchronous replication adds <5ms overhead to transaction commit times, and asynchronous replicas maintain <50ms lag under normal load.

**Gap Analysis:**
- Market alternatives like Neo4j, PostgreSQL, and Milvus all offer robust Raft-based or primary-backup replication strategies. AletheiaDB must match this baseline reliability to compete in the enterprise space.

## ✅ Acceptance Criteria
- Must define a consensus protocol (e.g., Raft) for leader election and log replication across a cluster of 3 or more nodes.
- Must support both synchronous (RPO=0) and asynchronous replication modes per transaction.
- Must automatically route read queries to read-replicas (followers) to scale read throughput.
- Must automatically promote a healthy follower to leader within 5 seconds of the primary node failing.
- Must ensure that temporal invariants (valid time, transaction time) are strictly maintained and monotonic across the entire replica set.
- Must handle network partitions gracefully (preventing split-brain scenarios).

## 🚫 Out of Scope
- Geo-distributed multi-region active-active clusters (Phase 2). MVP focuses on single-region primary-backup HA.
- Distributed transactions across *shards* and *replicas* simultaneously (MVP focuses on replicating a single shard/database instance).
