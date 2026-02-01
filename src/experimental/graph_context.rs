use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::core::temporal::{time, Timestamp};
use crate::core::interning::GLOBAL_INTERNER;
use crate::utils::error::Result;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// Configuration for generating a graph context.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Maximum traversal depth from the root node.
    pub depth: usize,
    /// Optional timestamp to retrieve historical context.
    /// If None, retrieves current context.
    pub time: Option<Timestamp>,
    /// Optional filter for edge labels.
    /// If Some, only edges with these labels are traversed/included.
    pub edge_types: Option<Vec<String>>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            depth: 1,
            time: None,
            edge_types: None,
        }
    }
}

/// Generator for creating LLM-ready context from a subgraph.
pub struct GraphContextGenerator<'a> {
    db: &'a GallifreyDB,
}

impl<'a> GraphContextGenerator<'a> {
    /// Create a new graph context generator.
    pub fn new(db: &'a GallifreyDB) -> Self {
        Self { db }
    }

    /// Generate a markdown-formatted string describing the subgraph context.
    pub fn generate(&self, root_id: NodeId, config: ContextConfig) -> Result<String> {
        let mut output = String::new();
        let transaction_time = time::now(); // For temporal queries

        // Header
        if let Some(t) = config.time {
            writeln!(&mut output, "# Graph Context (as of {})", t).unwrap();
        } else {
            writeln!(&mut output, "# Graph Context (Current)").unwrap();
        }

        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        // (node_id, current_depth)
        queue.push_back((root_id, 0));
        visited.insert(root_id);

        while let Some((node_id, depth)) = queue.pop_front() {
            // Get node data
            let node = if let Some(valid_time) = config.time {
                match self.db.get_node_at_time(node_id, valid_time, transaction_time) {
                    Ok(n) => n,
                    Err(_) => {
                        let mut found = None;

                        // Fallback 1: Check if current version satisfies the time query
                        if let Ok(current_node) = self.db.get_node(node_id) {
                            if let Some(commit_ts) = current_node.metadata.commit_timestamp {
                                // Check if the current version was committed and valid by the requested time
                                if commit_ts <= transaction_time && commit_ts <= valid_time {
                                     found = Some(current_node);
                                }
                            }
                        }

                        // Fallback 2: Scan history manually (robustness against index delays)
                        if found.is_none() {
                            if let Ok(history) = self.db.get_node_history(node_id) {
                                for version in history.versions.iter().rev() {
                                    // Check valid time coverage and transaction time visibility (start only)
                                    // Note: we check start() <= transaction_time because updates might close the transaction interval
                                    // but the fact remains historically true. By scanning reverse, we find the latest belief.
                                    if version.temporal.valid_time().contains(valid_time)
                                        && version.temporal.transaction_time().start() <= transaction_time
                                    {
                                        if let Ok(n) = self.db.get_node_at_version(node_id, version.version_number) {
                                            found = Some(n);
                                            break;
                                        }
                                    }
                                }
                            }
                        }

                        if let Some(n) = found {
                            n
                        } else {
                            continue;
                        }
                    }
                }
            } else {
                match self.db.get_node(node_id) {
                    Ok(n) => n,
                    Err(_) => continue, // Node not found
                }
            };

            // Format Node
            let label_str = GLOBAL_INTERNER.resolve(node.label).map(|s| s.to_string()).unwrap_or_else(|| "Unknown".to_string());

            if depth == 0 {
                writeln!(&mut output, "\n## Central Node: {} ({})", node_id, label_str).unwrap();
            } else {
                writeln!(&mut output, "\n### Node: {} ({})", node_id, label_str).unwrap();
            }

            // Properties
            for (key, value) in node.properties.iter() {
                 let key_str = GLOBAL_INTERNER.resolve(*key).map(|s| s.to_string()).unwrap_or_else(|| "Unknown".to_string());
                writeln!(&mut output, "- {}: {}", key_str, value).unwrap();
            }

            // Stop traversal if max depth reached
            if depth >= config.depth {
                continue;
            }

            // Edges
            // Note: We traverse current edges because we don't have historical adjacency index yet.
            // However, we filter them by existence at `config.time`.
            let edge_ids = self.db.get_outgoing_edges(node_id);

            if !edge_ids.is_empty() {
                writeln!(&mut output, "\n#### Relationships").unwrap();
            }

            for edge_id in edge_ids {
                // Get edge data (possibly historical)
                let edge = if let Some(valid_time) = config.time {
                    match self.db.get_edge_at_time(edge_id, valid_time, transaction_time) {
                        Ok(e) => e,
                        Err(_) => {
                             let mut found = None;

                             // Fallback 1: Current
                             if let Ok(current_edge) = self.db.get_edge(edge_id) {
                                if let Some(commit_ts) = current_edge.metadata.commit_timestamp {
                                    if commit_ts <= transaction_time && commit_ts <= valid_time {
                                        found = Some(current_edge);
                                    }
                                }
                             }

                             // Fallback 2: History
                             if found.is_none() {
                                 if let Ok(history) = self.db.get_edge_history(edge_id) {
                                    for version in history.versions.iter().rev() {
                                        if version.temporal.valid_time().contains(valid_time)
                                            && version.temporal.transaction_time().start() <= transaction_time
                                        {
                                            // Reconstruct edge from history
                                            // We assume topology (source/target) is invariant for the same EdgeId
                                            if let (Ok(source), Ok(target)) = (self.db.get_edge_source(edge_id), self.db.get_edge_target(edge_id)) {
                                                if let Ok(label) = GLOBAL_INTERNER.intern(&version.label) {
                                                    let e = crate::core::graph::Edge::new(
                                                        edge_id,
                                                        label,
                                                        source,
                                                        target,
                                                        version.properties.clone(),
                                                        version.version_id
                                                    );
                                                    found = Some(e);
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                 }
                             }

                             if let Some(e) = found {
                                 e
                             } else {
                                 continue;
                             }
                        }
                    }
                } else {
                    match self.db.get_edge(edge_id) {
                        Ok(e) => e,
                        Err(_) => continue,
                    }
                };

                let edge_label_str = GLOBAL_INTERNER.resolve(edge.label).map(|s| s.to_string()).unwrap_or_else(|| "Unknown".to_string());

                // Filter by label if configured
                if let Some(ref allowed) = config.edge_types {
                    if !allowed.contains(&edge_label_str) {
                        continue;
                    }
                }

                // Format Edge
                let target_id = edge.target;
                // Try to get target label for context (optimistic)
                let target_label = self.db.get_node(target_id)
                    .ok()
                    .and_then(|n| GLOBAL_INTERNER.resolve(n.label).map(|s| s.to_string()))
                    .unwrap_or_else(|| "Unknown".to_string());

                writeln!(&mut output, "- [{}] -> {} ({})", edge_label_str, target_id, target_label).unwrap();

                for (key, value) in edge.properties.iter() {
                    let key_str = GLOBAL_INTERNER.resolve(*key).map(|s| s.to_string()).unwrap_or_else(|| "Unknown".to_string());
                    writeln!(&mut output, "  - {}: {}", key_str, value).unwrap();
                }

                // Enqueue target if not visited
                if !visited.contains(&target_id) {
                    visited.insert(target_id);
                    queue.push_back((target_id, depth + 1));
                }
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_graph_context_generation() {
        let db = GallifreyDB::new().unwrap();

        // Setup Graph
        let alice = db.create_node("Person", PropertyMapBuilder::new().insert("name", "Alice").insert("age", 30i64).build()).unwrap();
        let bob = db.create_node("Person", PropertyMapBuilder::new().insert("name", "Bob").build()).unwrap();
        let acme = db.create_node("Company", PropertyMapBuilder::new().insert("name", "Acme").build()).unwrap();

        db.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().insert("since", 2020i64).build()).unwrap();
        db.create_edge(alice, acme, "WORKS_AT", PropertyMapBuilder::new().insert("role", "Engineer").build()).unwrap();

        let generator = GraphContextGenerator::new(&db);
        let config = ContextConfig {
            depth: 1,
            ..Default::default()
        };

        let output = generator.generate(alice, config).unwrap();

        // Basic checks
        assert!(output.contains("Alice"));
        assert!(output.contains("Bob"));
        assert!(output.contains("Acme"));
        assert!(output.contains("KNOWS"));
        assert!(output.contains("WORKS_AT"));
        assert!(output.contains("since: 2020"));
    }

    #[test]
    fn test_temporal_context() {
        use std::thread;
        use std::time::Duration;

        let db = GallifreyDB::new().unwrap();

        // T1: Create Node
        // Use write_with_timestamp to ensure we capture the exact commit time
        let (node_id, t1) = db.write_with_timestamp(|tx| {
             tx.create_node("Test", PropertyMapBuilder::new().insert("status", "active").build())
        }).unwrap();

        // Wait to ensure distinct timestamps
        thread::sleep(Duration::from_millis(10));

        // T2: Update Node
        let (_, t2) = db.write_with_timestamp(|tx| {
            tx.update_node(node_id, PropertyMapBuilder::new().insert("status", "inactive").build())
        }).unwrap();

        let generator = GraphContextGenerator::new(&db);

        // Context at T1 (should be active)
        // We query EXACTLY at creation time
        let config_t1 = ContextConfig {
            depth: 0,
            time: Some(t1),
            ..Default::default()
        };
        let output_t1 = generator.generate(node_id, config_t1).unwrap();
        // Note: String properties are quoted in output
        assert!(output_t1.contains("status: \"active\""), "Expected 'status: \"active\"' at t1");
        assert!(!output_t1.contains("inactive"));

        // Context at T2 (should be inactive)
        let config_t2 = ContextConfig {
            depth: 0,
            time: Some(t2),
            ..Default::default()
        };
        let output_t2 = generator.generate(node_id, config_t2).unwrap();
        assert!(output_t2.contains("status: \"inactive\""), "Expected 'status: \"inactive\"' at t2");
    }
}
