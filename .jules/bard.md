# Bard's Journal 🎻

## 2024-05-22 - The "Ignore" Trap
**Confusion:** Many doctests were marked `ignore` because they lacked setup code (creating a DB, nodes, etc.), making them effectively dead code that could rot without notice.
**Clarification:** I've started converting `ignore` blocks to full, runnable examples by adding the necessary boilerplate (hidden with `#` lines if needed) to ensure they compile and pass `cargo test`. This guarantees the documentation never lies.

## 2024-05-22 - The Missing "Why"
**Confusion:** Several core modules (`db/admin`, `db/config`, `db/ops`) lacked high-level explanations (`//!`) of *why* they exist, only listing *what* functions they contain.
**Clarification:** I'm adding `//!` module documentation to explain the "Soul" of each module—its role in the larger architecture and how it fits into the user's journey.
