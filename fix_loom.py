with open("tests/havoc/havoc_loom_sharding_coordinator_deadlock.rs", "r") as f:
    content = f.read()

content = content.replace("let connections = self.connections.read().unwrap();", "let _connections = self.connections.read().unwrap();")
with open("tests/havoc/havoc_loom_sharding_coordinator_deadlock.rs", "w") as f:
    f.write(content)
