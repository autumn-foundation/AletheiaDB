# ADR-0004: Anchor+Delta Compression

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** storage, performance

## Context

GallifreyDB tracks all historical versions of nodes and edges, which creates significant storage pressure. Without compression, storage grows linearly with the number of updates.

Consider a node with 1000 updates over its lifetime:
- **Naive approach**: Store 1000 full copies → 1000x storage
- **Delta-only**: Store 1 base + 999 deltas → Must traverse all deltas to reconstruct

The challenge is balancing:
1. **Storage efficiency**: Minimize space used
2. **Reconstruction speed**: Quickly reconstruct any historical version
3. **Append-only writes**: Historical versions are immutable

## Decision

We will implement **anchor+delta compression** for version chains:

### Version Chain Structure

```
Version Chain:
  [Anchor] → [Delta] → [Delta] → ... → [Anchor] → [Delta] → ...
     ↑                                      ↑
   Full snapshot                        New anchor every N versions
```

### Data Structures

```rust
pub struct NodeVersion {
    pub version_id: VersionId,
    pub node_id: NodeId,
    pub temporal: BiTemporalInterval,
    pub label: InternedString,
    pub data: VersionData,
}

pub enum VersionData {
    /// Full snapshot of all properties
    Anchor(PropertyMap),

    /// Changes relative to previous version
    Delta(PropertyDelta, VersionId),  // delta + prev_version_id
}

pub struct PropertyDelta {
    /// Properties that changed (new or updated values)
    pub changed: HashMap<String, PropertyValue>,

    /// Properties that were removed
    pub removed: HashSet<String>,
}
```

### Configuration

```rust
pub struct AnchorConfig {
    /// Create new anchor every N versions
    pub anchor_interval: usize,  // Default: 10

    /// Force anchor if delta chain exceeds this length
    pub max_delta_chain: usize,  // Default: 20
}
```

### Reconstruction Algorithm

```rust
fn reconstruct_at_version(&self, target_version_id: VersionId) -> PropertyMap {
    // 1. Walk backward to find nearest anchor
    let mut deltas = Vec::new();
    let mut current = self.get_version(target_version_id);

    while let VersionData::Delta(delta, prev_id) = &current.data {
        deltas.push(delta.clone());
        current = self.get_version(*prev_id);
    }

    // 2. Start from anchor
    let VersionData::Anchor(base) = &current.data else { unreachable!() };
    let mut result = base.clone();

    // 3. Apply deltas in forward order
    for delta in deltas.into_iter().rev() {
        delta.apply(&mut result);
    }

    result
}
```

### Storage Analysis

| Scenario | Full Copies | Anchor+Delta (interval=10) |
|----------|-------------|---------------------------|
| 100 versions, 10% change each | 100x base | 10 anchors + 90 deltas ≈ 19x base |
| 1000 versions, 5% change each | 1000x base | 100 anchors + 900 deltas ≈ 145x base |
| **Compression ratio** | 1x | **5-7x** |

## Consequences

### Positive

- **5-6x storage reduction**: Significant space savings for frequently updated entities
- **Bounded reconstruction cost**: At most `anchor_interval` deltas to traverse
- **Arc-based sharing**: Unchanged properties share memory via Arc
- **Append-only friendly**: New versions append, old versions never modified
- **Tunable trade-off**: Anchor interval adjusts space/time balance

### Negative

- **Reconstruction overhead**: Must apply deltas vs. direct read
- **More complex queries**: Time-travel requires chain walking
- **Delta computation**: Must compute diff on each update
- **Memory allocation**: Reconstruction creates new PropertyMap

### Neutral

- Standard technique in version control (git) and databases
- Well-understood performance characteristics
- Anchor placement is configurable per use case

## Alternatives Considered

### Alternative 1: Full Copy per Version

Store complete snapshot for each version.

**Rejected because:**
- Unbounded storage growth
- Properties that rarely change are duplicated
- 5-10x more storage than anchor+delta

### Alternative 2: Delta-Only (No Anchors)

Store base version plus deltas only.

**Rejected because:**
- Reconstruction requires traversing entire history
- O(n) reconstruction time for n versions
- Single base version is a bottleneck

### Alternative 3: Copy-on-Write B-Tree

Use persistent data structure like HAMT.

**Considered for future because:**
- More complex implementation
- Good for fine-grained sharing
- May be valuable for very large property maps

### Alternative 4: Periodic Snapshots (Not Version-Based)

Create snapshots at time intervals rather than version intervals.

**Rejected because:**
- Uneven delta chain lengths
- Some entities would have very long chains
- Version-based is more predictable

## Implementation Notes

### Delta Computation

```rust
impl PropertyDelta {
    pub fn from_diff(old: &PropertyMap, new: &PropertyMap) -> Self {
        let mut changed = HashMap::new();
        let mut removed = HashSet::new();

        // Find changed and new properties
        for (key, new_value) in new.iter() {
            match old.get(key) {
                Some(old_value) if old_value == new_value => continue,
                _ => { changed.insert(key.clone(), new_value.clone()); }
            }
        }

        // Find removed properties
        for key in old.keys() {
            if !new.contains_key(key) {
                removed.insert(key.clone());
            }
        }

        PropertyDelta { changed, removed }
    }

    pub fn apply(&self, base: &mut PropertyMap) {
        for (key, value) in &self.changed {
            base.insert(key.clone(), value.clone());
        }
        for key in &self.removed {
            base.remove(key);
        }
    }
}
```

### Anchor Decision

```rust
fn should_create_anchor(&self, version_count: usize, delta_chain_length: usize) -> bool {
    version_count % self.config.anchor_interval == 0 ||
    delta_chain_length >= self.config.max_delta_chain
}
```

### Performance Targets

| Operation | Target |
|-----------|--------|
| Reconstruction (10 deltas) | <100µs |
| Delta computation | <50µs |
| Storage overhead vs non-temporal | <2x |

## References

- [Git Packfiles](https://git-scm.com/book/en/v2/Git-Internals-Packfiles)
- [Apache Iceberg Time Travel](https://iceberg.apache.org/docs/latest/spark-queries/#time-travel)
- [Dolt Version Control for SQL](https://docs.dolthub.com/concepts/dolt/git/version-control)
- ADR-0001: Hybrid Storage Architecture
- ADR-0002: Bi-Temporal Data Model
