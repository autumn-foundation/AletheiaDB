import re

with open("src/index/vector/distributed.rs", "r") as f:
    content = f.read()

# 1. Remove the VectorNodeClient trait definition
# From "pub trait VectorNodeClient" to "}\n\n// ======================================="
trait_start = content.find("pub trait VectorNodeClient")
trait_end = content.find("// Circuit Breaker for Nodes", trait_start)

# backtrack to the `// =====================================`
trait_end_full = content.rfind("// ============================================================================", trait_start, trait_end)
if trait_end_full == -1:
    trait_end_full = trait_end

# Also remove the doc comments before the trait
doc_start = content.rfind("/// Trait for communicating", 0, trait_start)

content = content[:doc_start] + content[trait_end_full:]

with open("src/index/vector/distributed.rs", "w") as f:
    f.write(content)
