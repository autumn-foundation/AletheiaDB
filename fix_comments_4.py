import re

with open("src/storage/redb_cold_storage.rs", "r") as f:
    content = f.read()

def replace_or_fail(search, replace):
    global content
    if search not in content:
        raise ValueError(f"Search string not found:\n{search}")
    content = content.replace(search, replace)


replace_or_fail('''/// Encode a NodeVersion to bytes for storage.
///
/// This function is public for use by all cold storage implementations.
pub fn encode_node_version(version: &NodeVersion) -> Vec<u8> {''', '''/// Encode a `NodeVersion` into a byte payload for storage.
///
/// Uses `bitcode` for extremely fast serialization. The resulting byte array
/// is deterministic, compact, and optimized for compression.
///
/// #[doc(hidden)]
pub fn encode_node_version(version: &NodeVersion) -> Vec<u8> {''')

replace_or_fail('''/// Decode a NodeVersion from bytes.
///
/// This function is public for use by all cold storage implementations.
pub fn decode_node_version(data: &[u8]) -> Result<NodeVersion> {''', '''/// Decode a `NodeVersion` from a previously serialized byte payload.
///
/// Reconstructs the internal references (like `InternedString`) alongside
/// the core node temporal data.
///
/// #[doc(hidden)]
pub fn decode_node_version(data: &[u8]) -> Result<NodeVersion> {''')

replace_or_fail('''/// Encode an EdgeVersion to bytes for storage.
///
/// This function is public for use by all cold storage implementations.
pub fn encode_edge_version(version: &EdgeVersion) -> Vec<u8> {''', '''/// Encode an `EdgeVersion` into a byte payload.
///
/// Converts the complex `EdgeVersion` (including source and target pointers)
/// into a flat, binary `bitcode` representation.
///
/// #[doc(hidden)]
pub fn encode_edge_version(version: &EdgeVersion) -> Vec<u8> {''')

replace_or_fail('''/// Decode an EdgeVersion from bytes.
///
/// This function is public for use by all cold storage implementations.
pub fn decode_edge_version(data: &[u8]) -> Result<EdgeVersion> {''', '''/// Decode an `EdgeVersion` from a byte payload.
///
/// Reconstructs all internal metadata, including `InternedString` labels
/// and associated endpoints.
///
/// #[doc(hidden)]
pub fn decode_edge_version(data: &[u8]) -> Result<EdgeVersion> {''')


with open("src/storage/redb_cold_storage.rs", "w") as f:
    f.write(content)
