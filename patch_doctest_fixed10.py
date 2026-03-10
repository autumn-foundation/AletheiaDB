import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

# Replace:
# # #[cfg(feature = "nova")]
# # {
# with:
# # #[cfg(feature = "nova")]
# # fn main() -> Result<(), Box<dyn std::error::Error>> {
content = content.replace('# #[cfg(feature = "nova")]\n//! # {', '# #[cfg(feature = "nova")]\n//! fn main() -> Result<(), Box<dyn std::error::Error>> {')

# Replace:
# # fn main() -> Result<(), Box<dyn std::error::Error>> {
# let db = AletheiaDB::new()?;
# with:
# let db = AletheiaDB::new()?;
content = content.replace('# fn main() -> Result<(), Box<dyn std::error::Error>> {\n//! let db = AletheiaDB::new()?;', 'let db = AletheiaDB::new()?;')

# Replace:
# # Ok(())
# # }
# # }
# # #[cfg(not(feature = "nova"))]
# # fn main() {}
# with:
# # Ok(())
# }
# # #[cfg(not(feature = "nova"))]
# # fn main() {}
content = content.replace('# Ok(())\n//! # }\n//! # }\n//! # #[cfg(not(feature = "nova"))]\n//! # fn main() {}', '# Ok(())\n//! }\n//! # #[cfg(not(feature = "nova"))]\n//! # fn main() {}')

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
