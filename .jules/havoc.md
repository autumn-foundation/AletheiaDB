**[Chaos Findings]
**Module:** HTTP Server / JSON Limits
**Summary:** JSON deserialization relies on actix-web's defaults, meaning a malicious client can upload a huge JSON payload and cause the server to OOM.
**Diagnosis:** `web::JsonConfig` is not explicitly configured on the app, allowing `actix-web` defaults. Wait, default limit in `actix-web` is 2MB. I sent a 10MB payload and got `413 Payload Too Large`. The server correctly restricted it! No OOM possible here.
**Kill Shot:** None found.

**Module:** HNSW Index (usearch FFI)
**Summary:** Passing null pointers or unaligned pointers from Rust to usearch C++ layer through the `create_metric_wrapper` can cause Undefined Behavior.
**Diagnosis:** The wrapper validates that `a` and `b` are not null and are properly aligned. It also wraps the execution in `catch_unwind` to prevent Rust panics from propagating into C++.
**Kill Shot:** None found.

**Module:** SIMD Vector Math
**Summary:** The functions using `unsafe` such as `dot_and_magnitudes_avx2`, `scale_in_place_avx2` perform their own alignment or len checks (`assert_eq!(a.len(), b.len());`) and correctly handle remainder arrays.
**Diagnosis:** Strongly protected.

**Module:** Memory mapped index (Graph)
**Summary:** `load_graph_index_mmap` explicitly validates the file size up to `MAX_MMAP_FILE_SIZE` and verifies `file.metadata()?.len() > 0`. Wait, it actually verifies `mmap.len() < 4`. `Mmap::map` can crash or error on empty files, but this is handled by returning `Err`.
**Diagnosis:** Zero-byte files or corrupted checksums are correctly rejected as `CorruptedData`.

**Module:** CompressedCommitLog (RwLock)
**Summary:** There is a potential for thread safety issues inside `TxVisibilityManager`? `CompressedCommitLog` handles concurrent reads perfectly.

Is there an out of bounds array access somewhere?
