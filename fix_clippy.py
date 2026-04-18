def update_file(path, replacements):
    with open(path, 'r') as f:
        content = f.read()
    for old, new in replacements:
        if old not in content:
            print(f"Failed to find target in {path}:\n{old}")
        content = content.replace(old, new)
    with open(path, 'w') as f:
        f.write(content)

replacements_distributed = [
    (
        "        let target_per_node = if node_count > 0 {\n            total_vectors / node_count\n        } else {\n            0\n        };",
        "        let target_per_node = if node_count > 0 {\n            total_vectors.checked_div(node_count).unwrap_or(0)\n        } else {\n            0\n        };"
    )
]

replacements_migration = [
    (
        "        } else {\n            // Age mode: sort by age (oldest first) to prioritize older versions\n            candidates.sort_by(|a, b| b.age.cmp(&a.age));\n        }",
        "        } else {\n            // Age mode: sort by age (oldest first) to prioritize older versions\n            candidates.sort_by_key(|b| std::cmp::Reverse(b.age));\n        }"
    )
]

update_file('src/index/vector/distributed.rs', replacements_distributed)
update_file('src/storage/migration.rs', replacements_migration)
