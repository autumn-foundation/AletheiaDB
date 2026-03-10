import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

# the unclosed delimiter error happens because of the `# }`
old_end = """//! # Ok(())
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

new_end = """//! # Ok(())
//! # }
//!
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

content = content.replace(old_end, new_end)

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
