## 2026-10-17 - SIMD Safety & Coverage
**Learning:** `unsafe` SIMD functions in `src/core/vector.rs` lacked explicit buffer bounds checks (`debug_assert_eq!`) and direct coverage testing. They relied on implicit constraints from public APIs and runtime feature detection for coverage.
**Action:** Added `debug_assert_eq!(a.len(), b.len())` to SIMD functions to catch length mismatches in debug builds. Added explicit tests that use `is_x86_feature_detected!` to call these unsafe functions directly, ensuring coverage of specific instruction sets when available.
