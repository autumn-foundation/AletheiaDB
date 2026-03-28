# 🗣️ Echo: Getting Started example is broken

## 🤦 The Confusion:
Tried to run the Transactions snippet from the README by literally copy-pasting it into a fresh `main.rs`. The code block failed to compile with the error "cannot initialize a tuple struct which contains private fields" when I tried to guess how to construct `NodeId`. The snippet also lacked variable definitions for `db` and `alice_id`.

## 🕵️‍♂️ The Reality:
The code blocks in the README were not wrapped in a `fn main() -> ...` function and were missing variable definitions, making them impossible to run via simple copy-paste for a new user.

## 💡 The Fix:
Modified `README.md` to ensure every code block is a complete, runnable program wrapped in a `fn main()` block, and added missing definitions like `let db = AletheiaDB::new().unwrap();` and `let alice_id = db.create_node(...)`.