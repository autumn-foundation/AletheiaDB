import re

with open('src/storage/index_persistence/operations.rs', 'r') as f:
    lines = f.readlines()

node_restore_start = 730
node_restore_end = 787
edge_restore_start = 790
edge_restore_end = 848

node_helper_code = """
#[derive(Default)]
struct RestoreStats {
    loaded: usize,
    failed_label: usize,
    failed_properties: usize,
    failed_version: usize,
}

fn restore_nodes_startup(
    persisted_nodes: &[crate::storage::index_persistence::formats::graph::PersistedNode],
    current: &std::sync::Arc<crate::storage::CurrentStorage>,
    current_time: crate::core::hlc::HybridTimestamp,
) -> RestoreStats {
    let mut stats = RestoreStats::default();

""" + "".join(lines[node_restore_start:node_restore_end+1]) + """

    stats
}
"""

edge_helper_code = """
fn restore_edges_startup(
    persisted_edges: &[crate::storage::index_persistence::formats::graph::PersistedEdge],
    current: &std::sync::Arc<crate::storage::CurrentStorage>,
    current_time: crate::core::hlc::HybridTimestamp,
) -> RestoreStats {
    let mut stats = RestoreStats::default();

""" + "".join(lines[edge_restore_start:edge_restore_end+1]) + """

    stats
}
"""

def fix_stats(code, prefix="nodes"):
    code = code.replace(f"{prefix}_loaded += 1;", "stats.loaded += 1;")
    code = code.replace(f"{prefix}_failed_label += 1;", "stats.failed_label += 1;")
    code = code.replace(f"{prefix}_failed_properties += 1;", "stats.failed_properties += 1;")
    code = code.replace(f"{prefix}_failed_version += 1;", "stats.failed_version += 1;")
    code = code.replace("restore_property_map(", "crate::storage::index_persistence::graph::restore_property_map(")
    return code

node_helper_code = fix_stats(node_helper_code, "nodes")
edge_helper_code = fix_stats(edge_helper_code, "edges")

node_helper_code = node_helper_code.replace("for persisted_node in &graph_data.nodes {", "for persisted_node in persisted_nodes {")
edge_helper_code = edge_helper_code.replace("for persisted_edge in &graph_data.edges {", "for persisted_edge in persisted_edges {")

replacement = """
                let node_stats = restore_nodes_startup(&graph_data.nodes, current, current_time);
                let nodes_loaded = node_stats.loaded;
                let nodes_failed_label = node_stats.failed_label;
                let nodes_failed_properties = node_stats.failed_properties;
                let nodes_failed_version = node_stats.failed_version;

                let edge_stats = restore_edges_startup(&graph_data.edges, current, current_time);
                let edges_loaded = edge_stats.loaded;
                let edges_failed_label = edge_stats.failed_label;
                let edges_failed_properties = edge_stats.failed_properties;
                let edges_failed_version = edge_stats.failed_version;
"""

with open('src/storage/index_persistence/operations.rs.new', 'w') as f:
    f.writelines(lines[:node_restore_start])
    f.write(replacement)
    f.writelines(lines[edge_restore_end+1:])
    f.write("\n")
    f.write(node_helper_code)
    f.write("\n")
    f.write(edge_helper_code)
