import re

with open('tests/snapshot_race_condition.rs', 'r') as f:
    content = f.read()

content = content.replace("for (&node_id, _versions) in &historical_node_versions {", "for &node_id in historical_node_versions.keys() {")

with open('tests/snapshot_race_condition.rs', 'w') as f:
    f.write(content)
