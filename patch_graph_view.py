with open("src/query/traits.rs", "r") as f:
    content = f.read()

# remove GraphView trait
trait_start = content.find("/// A trait representing a read-only view of the graph.")
content = content[:trait_start]

with open("src/query/traits.rs", "w") as f:
    f.write(content)
