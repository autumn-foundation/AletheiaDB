import re

with open("src/storage/index_persistence/temporal.rs", "r") as f:
    content = f.read()

content = content.replace(
    "/// - A `VectorDelta::Sparse` is encountered (preventing data loss).\n/// Convert a NodeVersion to an on-disk format entry.",
    "/// - A `VectorDelta::Sparse` is encountered (preventing data loss).\n///\n/// Convert a NodeVersion to an on-disk format entry."
)

with open("src/storage/index_persistence/temporal.rs", "w") as f:
    f.write(content)
