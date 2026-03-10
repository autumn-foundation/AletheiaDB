import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

# Fix the issue: the problem was `# {` right after `#[cfg(feature="nova")]`. It causes the "expressions at top level" warning and compiling errors because the test harness generates `fn main() { #[cfg(feature="nova")] { ... } }` which is valid rust, BUT if we put `fn main` *inside* the doctest, we can't have `#[cfg]` before it.
# Wait, if rustdoc generates `fn main() { #[cfg(feature="nova")] { ... } }`, then we CANNOT have `# fn main() {` inside the doctest if we also have `# #[cfg(feature="nova")]`.
# Wait, let's look at the journal AGAIN.
# "conditionally compile the imports and the `main` function itself: `# #[cfg(feature = "nova")] \n fn main() { ... }`, and crucially, provide an empty fallback `main` function for when the feature is disabled: `# #[cfg(not(feature = "nova"))] \n # fn main() {}`."
# Notice the journal explicitly says: `# #[cfg(feature = "nova")] \n fn main() { ... }`

old_doc = """//! ```rust
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! use aletheiadb::core::property::PropertyValue;
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;
//!
//! // Define a mystery: User logs in, then deletes file within 1 second
//! let mystery = Mystery::new(Duration::from_secs(1))
//!     .add_clue(Clue::PropertyState {
//!         key: "status".to_string(),
//!         value: Some(PropertyValue::from("LoggedIn")),
//!     })
//!     .add_clue(Clue::PropertyState {
//!         key: "action".to_string(),
//!         value: Some(PropertyValue::from("DeleteFile")),
//!     });
//!
//! let sherlock = Sherlock::new(&db);
//! let detections = sherlock.investigate(node_id, &mystery)?;
//!
//! if !detections.is_empty() {
//!     println!("🕵️ Sherlock found {} suspicious sequences!", detections.len());
//! }
//! # Ok(())
//! # }
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

new_doc = """//! ```rust
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # use aletheiadb::AletheiaDB;
//! # use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! # use aletheiadb::core::property::PropertyValue;
//! # use std::time::Duration;
//! # let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("User", Default::default())?;
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! use aletheiadb::core::property::PropertyValue;
//! use std::time::Duration;
//!
//! let db = AletheiaDB::new()?;
//!
//! // Define a mystery: User logs in, then deletes file within 1 second
//! let mystery = Mystery::new(Duration::from_secs(1))
//!     .add_clue(Clue::PropertyState {
//!         key: "status".to_string(),
//!         value: Some(PropertyValue::from("LoggedIn")),
//!     })
//!     .add_clue(Clue::PropertyState {
//!         key: "action".to_string(),
//!         value: Some(PropertyValue::from("DeleteFile")),
//!     });
//!
//! let sherlock = Sherlock::new(&db);
//! let detections = sherlock.investigate(node_id, &mystery)?;
//!
//! if !detections.is_empty() {
//!     println!("🕵️ Sherlock found {} suspicious sequences!", detections.len());
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

content = content.replace(old_doc, new_doc)

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
