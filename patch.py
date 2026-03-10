import re

def modify_file(filepath):
    with open(filepath, 'r') as f:
        content = f.read()

    new_content = content.replace(
        """        if let Some(&max_id) = node_ids.iter().max() {
            if max_id > MAX_VALID_ID {
                return Err(format!(
                    "CSR node_ids contains an invalid ID: {} > MAX_VALID_ID ({})",
                    max_id, MAX_VALID_ID
                ));
            }
        }""",
        """        #[allow(clippy::collapsible_if)]
        if let Some(&max_id) = node_ids.iter().max() {
            if max_id > MAX_VALID_ID {
                return Err(format!(
                    "CSR node_ids contains an invalid ID: {} > MAX_VALID_ID ({})",
                    max_id, MAX_VALID_ID
                ));
            }
        }"""
    )

    new_content = new_content.replace(
        """        if let Some(&max_edge) = edge_ids.iter().max() {
            if max_edge > MAX_VALID_ID {
                return Err(format!(
                    "CSR edge_ids contains an invalid ID: {} > MAX_VALID_ID ({})",
                    max_edge, MAX_VALID_ID
                ));
            }
        }""",
        """        #[allow(clippy::collapsible_if)]
        if let Some(&max_edge) = edge_ids.iter().max() {
            if max_edge > MAX_VALID_ID {
                return Err(format!(
                    "CSR edge_ids contains an invalid ID: {} > MAX_VALID_ID ({})",
                    max_edge, MAX_VALID_ID
                ));
            }
        }"""
    )

    with open(filepath, 'w') as f:
        f.write(new_content)

modify_file('src/index/adjacency.rs')
