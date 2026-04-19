import re

with open("src/query/executor/mod.rs", "r") as f:
    content = f.read()

# Just append to the bottom if it's not exported
if "pub use iterators::EmptyIterator;" not in content:
    content = re.sub(
        r"pub use iterators::\{",
        r"pub use iterators::EmptyIterator;\npub use iterators::{",
        content,
        count=1
    )

with open("src/query/executor/mod.rs", "w") as f:
    f.write(content)
