import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

# Find the block:
# ```rust
# // [dependencies]
# // aletheiadb = { version = "0.1", features = ["nova"] }
#
# # #[cfg(feature = "nova")]
# # {

old_str = """//! ```rust
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # {
//! use aletheiadb::AletheiaDB;"""

new_str = """//! ```rust
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;"""

content = content.replace(old_str, new_str)

old_str2 = """//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;"""

new_str2 = """//! # let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;"""

content = content.replace(old_str2, new_str2)

# Now fix the end of the doctest
old_end = """//! # Ok(())
//! # }
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

new_end = """//! # Ok(())
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

content = content.replace(old_end, new_end)

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
