with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()
content = content.replace("total_vectors / node_count", "total_vectors.checked_div(node_count).unwrap_or(0)")
with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)

with open("src/storage/migration.rs", "r") as f:
    content = f.read()
content = content.replace("candidates.sort_by(|a, b| b.age.cmp(&a.age));", "candidates.sort_by_key(|b| std::cmp::Reverse(b.age));")
with open("src/storage/migration.rs", "w") as f:
    f.write(content)
