## [Reduction]
**Bloat:** `WormholeDetector` struct and `Wormhole` data object in `src/experimental/wormhole.rs` used only by `Alchemist` in `src/experimental/alchemy.rs`.
**Cut:** Inlined `bfs_distance` logic and vector search directly into `Alchemist::crystallize_wormholes`. Deleted `wormhole.rs`.
**Saved:** 1 file, ~170 lines of code, 1 fewer abstraction layer.
