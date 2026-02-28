# 🗣️ Echo: Transactions example is broken

## Description

🤦 **The Confusion:** "Tried to run the `Transactions` example in the README. The compiler complained about `db` not being found, `alice_id` not being found, and missing types. I thought I just had to wrap it in a `fn main() -> Result<(), Box<dyn std::error::Error>>` like the other examples, but then it started complaining about `type alias takes 1 generic argument but 2 generic arguments were supplied`. What gives?"

🕵️ **The Reality:** "Turns out the example was missing the `main` function wrapper, the database initialization (`let db = AletheiaDB::new().unwrap();`), and the creation of `alice_id`. Even trickier, because `aletheiadb::prelude::*` imports a custom `Result` type, trying to use `Result<(), Box<dyn std::error::Error>>` for the `main` function causes a collision. You have to explicitly use `std::result::Result`."

💡 **The Fix:** "Update the Transactions code block in the README to include the `main` function with the correct `std::result::Result` return type, initialize the database, and create the node before trying to read it. I've updated the README with a working example."
