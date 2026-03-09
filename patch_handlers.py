with open("src/http/handlers.rs", "r") as f:
    text = f.read()

import re

# Patch 1: handle_create_node
text = re.sub(
    r"let result = web::block\(move \|\| match db\.create_node\(&label, props\) \{.*?\n        \},\n        Err\(e\) => Err::<serde_json::Value, String>\(e\.to_string\(\)\),\n    \}\)\n    \.await;",
    r"""let result = web::block(move || {
        let node_id = db.create_node(&label, props).map_err(|e| e.to_string())?;
        let node = db.get_node(node_id).map_err(|e| e.to_string())?;

        let props_json =
            property_map_to_json(&node.properties).map_err(|e| e.to_string())?;
        let node_json = json!({
            "id": node.id.as_u64(),
            "label": interned_to_string(node.label),
            "properties": props_json
        });
        Ok::<serde_json::Value, String>(node_json)
    })
    .await;""",
    text,
    flags=re.DOTALL
)

# Patch 2: handle_find_node
text = re.sub(
    r"match builder\.limit\(limit_val\)\.execute\(&db\) \{.*?\n        \}",
    r"""let results = builder.limit(limit_val).execute(&db).map_err(|e| e.to_string())?;
        let mut nodes = Vec::new();
        for row in results.flatten() {
            if let crate::query::executor::EntityResult::Node(node) = row.entity {
                let props_json =
                    property_map_to_json(&node.properties).map_err(|e| e.to_string())?;
                nodes.push(json!({
                    "id": node.id.as_u64(),
                    "label": interned_to_string(node.label),
                    "properties": props_json
                }));
            }
        }
        Ok::<Vec<serde_json::Value>, String>(nodes)""",
    text,
    flags=re.DOTALL
)

# Patch 3: handle_find_neighbors
text = re.sub(
    r"for neighbor_id in combined_iter \{.*?\n        \}",
    r"""for neighbor_id in combined_iter {
            // Node ID found in edge but not in node index? Should be impossible unless corrupted.
            // Propagating error if it occurs.
            let node = db.get_node(neighbor_id).map_err(|e| e.to_string())?;
            let props_json =
                property_map_to_json(&node.properties).map_err(|e| e.to_string())?;
            neighbors.push(json!({
                "id": node.id.as_u64(),
                "label": interned_to_string(node.label),
                "properties": props_json
            }));
        }""",
    text,
    flags=re.DOTALL
)

# Patch 4: handle_execute_query
text = re.sub(
    r"// 2\. Execute the query\n\s*match db\.execute_query\(parsed_query\) \{.*?\n        \}",
    r"""// 2. Execute the query
        let results = db.execute_query(parsed_query).map_err(|e| e.to_string())?;

        // 3. Serialize results with a strict limit to prevent OOM DOS
        let max_results_limit = 10_000;

        results
            .take(max_results_limit)
            .map(|row_result| {
                let row = row_result.map_err(|e| e.to_string())?;
                query_row_to_json(row)
            })
            .collect::<Result<Vec<_>, String>>()""",
    text,
    flags=re.DOTALL
)


with open("src/http/handlers.rs", "w") as f:
    f.write(text)
