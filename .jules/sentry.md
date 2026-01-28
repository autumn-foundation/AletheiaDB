# Sentry Journal

## 2024-05-23 - FFI Safety in Custom Metrics
**Learning:** External libraries (like `usearch`) often use raw pointers in callbacks. While typically safe, relying on implicit contracts is risky. Defensive null checks at the FFI boundary prevent Undefined Behavior if the external library misbehaves or changes its contract.
**Action:** Always wrap FFI callbacks in a safe Rust function that validates pointers (e.g., `is_null()`) before creating slices. Extract this wrapper logic into a standalone, testable function to verify the safety behavior without needing the full external library context.
