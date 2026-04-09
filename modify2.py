import re

with open('tests/snapshot_race_condition.rs', 'r') as f:
    content = f.read()

content = content.replace("aletheiadb::storage::wal::ConcurrentWalSystemConfig", "aletheiadb::storage::wal::concurrent_system::ConcurrentWalSystemConfig")
content = content.replace("aletheiadb::storage::wal::ConcurrentWalSystem::", "aletheiadb::storage::wal::concurrent_system::ConcurrentWalSystem::")

with open('tests/snapshot_race_condition.rs', 'w') as f:
    f.write(content)
