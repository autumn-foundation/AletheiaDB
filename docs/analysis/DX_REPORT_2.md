# 🗣️ Echo DX Report: The Audit (Session 2)

**Auditor:** Echo (Voice of the User)
**Date:** 2026-02-02
**Subject:** README Examples & API Friction

## 🔍 The Walkthrough

I attempted to run the examples from `README.md` by copy-pasting them into a fresh `main.rs` (simulated by `examples/echo_check.rs`).

### 🚧 The Friction Points

#### 1. Unused Imports in Examples (Minor Friction)

**Scenario:** I copied the "Time-Travel Queries" example.
**Code:** `use aletheiadb::core::temporal::{Timestamp, time};`
**Warning:** `unused import: Timestamp`
**Why this hurts:** It makes the example look sloppy. New users might think they need `Timestamp` for something but can't figure out what.

**Scenario:** I copied the "Hybrid Queries" example.
**Code:** `use aletheiadb::query::QueryBuilder;`
**Warning:** `unused import: aletheiadb::query::QueryBuilder`
**Why this hurts:** The example uses `db.query()`, which returns a builder, so the import isn't needed for the code shown.

#### 2. The `NodeId` Mystery (Moderate Friction)

**Scenario:** I tried to query a node by ID, assuming I could just pass an integer or construct an ID.
**Code:** `let node_id = NodeId(99999);` or `use aletheiadb::core::types::NodeId;`
**Error:** `unresolved import` and `private field`.
**Reality:** `NodeId` is re-exported at the root, but the examples don't show this. To construct one, I need `NodeId::new(99999)?`.
**Fix:** Ensure examples show where types come from if they aren't obvious.

#### 3. Timestamp Ergonomics (Moderate Friction)

**Scenario:** I wanted to manipulate a timestamp (e.g., add 10 seconds).
**Code:** `ts.as_secs() + 10`
**Error:** `no method named as_secs found for struct HybridTimestamp`.
**Reality:** I have to use `aletheiadb::core::temporal::time::to_secs(ts)`.
**Why this hurts:** Object-oriented usage (`ts.as_secs()`) is much more discoverable than functional usage (`time::to_secs(ts)`). I have to look up the `time` module docs instead of just hitting `.` and seeing autocomplete.

## 💡 Recommendations

### 1. Clean up README Imports

**Recommendation:** Remove unused imports from the code blocks in `README.md`.

### 2. Improve `HybridTimestamp` DX

**Recommendation:** Add `as_secs()` and `as_millis()` methods directly to `HybridTimestamp` (and `Timestamp` alias) so users can easily convert without importing helper functions.

### 3. Show `NodeId` Usage

**Recommendation:** Add a small example or comment showing how to construct IDs manually if needed, or at least ensure `NodeId` is mentioned in the imports if it appears in the code.

## 🏁 Conclusion

The examples mostly work, which is great! But the unused imports and the clunky timestamp manipulation add unnecessary friction.
