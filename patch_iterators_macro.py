with open("src/storage/current/iterators.rs", "r") as f:
    content = f.read()

target = """        pub struct $name<'a> {"""
replacement = """        /// Zero-allocation edge iterator.
        ///
        /// (Detailed docs are generated automatically above via macro attributes).
        pub struct $name<'a> {"""
content = content.replace(target, replacement)

with open("src/storage/current/iterators.rs", "w") as f:
    f.write(content)
