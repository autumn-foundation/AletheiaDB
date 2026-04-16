with open("src/storage/current/iterators.rs", "r") as f:
    content = f.read()

target = """pub struct CurrentEdgesIterator<'a> {"""
replacement = """/// Iterator for streaming edges out of the current (live) graph storage.
///
/// # The Context
/// Because AletheiaDB allows concurrent reads and writes, we cannot safely hand out references
/// to internal storage collections. This iterator streams edges by copying them into an
/// isolated transaction context.
pub struct CurrentEdgesIterator<'a> {"""
content = content.replace(target, replacement)

target = """pub struct CurrentNodesIterator<'a> {"""
replacement = """/// Iterator for streaming nodes out of the current (live) graph storage.
///
/// # The Context
/// This iterator enables streaming large collections of nodes (such as full table scans)
/// without pulling them all into memory at once. It safely traverses the current storage
/// snapshot while adhering to MVCC transaction isolation.
pub struct CurrentNodesIterator<'a> {"""
content = content.replace(target, replacement)

with open("src/storage/current/iterators.rs", "w") as f:
    f.write(content)
