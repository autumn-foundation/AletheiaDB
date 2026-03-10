import re

with open('src/experimental/mod.rs', 'r') as f:
    content = f.read()

clean_doctest = """//! # Example: Detecting Suspicious Patterns with Sherlock
//!
//! > ⚠️ **REQUIRES FEATURE 'NOVA'**
//! >
//! > This feature is experimental and requires the `nova` feature flag.
//! > Add `features = ["nova"]` to your `Cargo.toml`.
//!
//! ```rust
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::sherlock::{Sherlock, Mystery, Clue};
//! use aletheiadb::core::property::PropertyValue;
//! use std::time::Duration;
//!
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
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```"""

content = re.sub(r"//! # Example: Detecting Suspicious Patterns with Sherlock.*?\! ```", clean_doctest, content, flags=re.DOTALL)

with open('src/experimental/mod.rs', 'w') as f:
    f.write(content)
