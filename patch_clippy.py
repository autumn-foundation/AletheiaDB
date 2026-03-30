with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

content = content.replace("    /// Create a new mock client.\n    \n    pub fn is_empty", "    /// Create a new mock client.\n    pub fn is_empty")

with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)
