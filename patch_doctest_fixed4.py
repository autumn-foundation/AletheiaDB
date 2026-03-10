import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

# Original doctest has:
# # #[cfg(feature = "nova")]
# # {
# ...
# # Ok(())
# # }
# # }
# # #[cfg(not(feature = "nova"))]
# # fn main() {}

# Let's change it to what the journal says EXACTLY:
# `# #[cfg(feature = "nova")] \n fn main() { ... }`, and crucially, provide an empty fallback `main` function for when the feature is disabled: `# #[cfg(not(feature = "nova"))] \n # fn main() {}`.

old_start = """# #[cfg(feature = "nova")]
//! # {"""

new_start = """# #[cfg(feature = "nova")]
//! fn main() -> Result<(), Box<dyn std::error::Error>> {"""

content = content.replace(old_start, new_start)

# The original has:
# # fn main() -> Result<(), Box<dyn std::error::Error>> {
# let db = AletheiaDB::new()?;

old_mid = """//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;"""

new_mid = """//! let db = AletheiaDB::new()?;"""

content = content.replace(old_mid, new_mid)

# The original has:
# # Ok(())
# # }
# # }
# # #[cfg(not(feature = "nova"))]
# # fn main() {}

old_end = """# Ok(())
//! # }
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}"""

new_end = """# Ok(())
//! }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}"""

content = content.replace(old_end, new_end)

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
