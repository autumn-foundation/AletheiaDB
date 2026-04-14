# Miri - Undefined Behavior Detection for AletheiaDB

## Overview

[Miri](https://github.com/rust-lang/miri) is an interpreter for Rust's mid-level intermediate representation (MIR). It can detect certain classes of undefined behavior (UB) in unsafe Rust code, including:

- Out-of-bounds memory accesses
- Use-after-free
- Invalid pointer arithmetic
- Data races in concurrent code
- Violations of Rust's aliasing rules (Stacked Borrows / Tree Borrows)
- Uninitialized memory reads

**Status**: AletheiaDB uses Miri to validate unsafe code in SIMD operations, WAL ring buffer, and lock-free data structures.

## Quick Start

### Installation

```bash
# Install miri component (requires nightly Rust)
just miri-setup

# Or manually:
rustup +nightly component add miri
cargo +nightly miri setup
```

### Running Miri

```bash
# Run miri on all tests
just miri

# Run specific test
just miri-test test_name

# Verbose output for debugging
just miri-verbose

# Run library tests only (faster)
just miri-lib
```

## Configuration

Miri configuration is stored in `.mirirc` at the project root:

```text
# Strict provenance tracking
-Zmiri-strict-provenance

# Track raw pointer tags
-Zmiri-tag-raw-pointers

# Weak memory emulation (detects data races)
-Zmiri-preemption-rate=0.01

# Allow system interactions (env vars, file I/O)
-Zmiri-disable-isolation

# Better diagnostics
-Zmiri-backtrace=full
```

### Key Flags Explained

| Flag | Purpose | Impact |
|------|---------|--------|
| `-Zmiri-strict-provenance` | Enforces strict pointer-integer casts | Catches invalid pointer operations |
| `-Zmiri-tag-raw-pointers` | Tracks pointer aliasing violations | Detects use-after-free, aliasing bugs |
| `-Zmiri-preemption-rate=N` | Thread interleaving frequency | Detects data races (0.01 = 1% chance per operation) |
| `-Zmiri-disable-isolation` | Allows env/FS access | Required for integration tests |
| `-Zmiri-backtrace=full` | Full stack traces on errors | Easier debugging |

## Known Limitations

### 1. SIMD Intrinsics

**Issue**: Miri cannot execute SIMD instructions (AVX2, SSE2).

**Solution**: Use `cfg(not(miri))` to conditionally disable SIMD:

```rust
#[cfg(not(miri))]
use std::arch::x86_64::*;

pub fn vector_distance(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(not(miri))]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe { dot_product_avx2(a, b) }
        } else {
            scalar_fallback(a, b)
        }
    }

    #[cfg(miri)]
    {
        scalar_fallback(a, b)
    }
}
```

**Status**: SIMD code in `src/core/vector/simd.rs` is automatically skipped under Miri.

### 2. Foreign Function Interface (FFI)

**Issue**: Miri cannot execute external C/C++ code (e.g., usearch).

**Solution**: Skip FFI-heavy tests with `#[cfg_attr(miri, ignore)]`:

```rust
#[test]
#[cfg_attr(miri, ignore = "usearch FFI not supported by Miri")]
fn test_usearch_integration() {
    // Test that calls usearch
}
```

**Affected**: Vector search tests using usearch are automatically skipped.

### 3. Async Runtime Limitations

**Issue**: Miri has limited support for advanced tokio features.

**Solution**: Basic async/await works, but complex runtime interactions may fail.

```rust
#[tokio::test]
#[cfg_attr(miri, ignore = "complex tokio features")]
async fn test_complex_async() {
    // Test with tokio::spawn, channels, etc.
}
```

### 4. Performance

**Issue**: Miri is ~1000x slower than native execution.

**Solution**:
- Run selectively: `just miri-test specific_test`
- Use `just miri-lib` to test library code only (faster than full suite)
- Focus on unsafe code modules

## What Miri Validates in AletheiaDB

### ✅ Currently Validated

| Module | Unsafe Operations | Validation |
|--------|------------------|------------|
| `src/core/vector/simd.rs` | SIMD pointer operations | Scalar fallback under Miri |
| `src/storage/wal/ring_buffer.rs` | Lock-free ring buffer | Concurrency checks |
| `src/storage/wal/segment_reader.rs` | Memory-mapped I/O | Provenance tracking |
| `src/index/vector/hnsw.rs` | Concurrent graph updates | Data race detection |
| `src/core/property.rs` | Transmute, raw pointers | Strict provenance |

### ⚠️ Skipped (FFI/SIMD)

- `tests/vector_api_tests.rs` - usearch integration
- `benches/current_state.rs` - SIMD benchmarks
- Some vector search tests - usearch FFI

## Interpreting Miri Errors

### Common Error Patterns

#### 1. Use-After-Free

```
error: Undefined Behavior: pointer to alloc12345 was dereferenced after this allocation got freed
```

**Cause**: Accessing memory after it's been deallocated.

**Fix**: Check lifetimes, ensure no dangling pointers.

#### 2. Data Race

```
error: Undefined Behavior: Data race detected between Write of size 8 at 0x10003e5b0 and Read of size 8
```

**Cause**: Concurrent access without proper synchronization.

**Fix**: Add atomic operations, mutexes, or memory barriers.

#### 3. Strict Provenance Violation

```
error: Undefined Behavior: out-of-bounds pointer arithmetic
```

**Cause**: Invalid pointer-integer cast or arithmetic.

**Fix**: Use `.cast()`, `.addr()`, `.with_addr()` for provenance-preserving operations.

#### 4. Aliasing Violation (Stacked Borrows)

```
error: Undefined Behavior: attempting a read access using <tag> at alloc12345, but that tag does not exist in the borrow stack
```

**Cause**: Conflicting mutable/immutable borrows in unsafe code.

**Fix**: Review pointer aliasing, ensure no overlapping mutable access.

## Development Workflow

### Pre-Commit Checks

Miri is **not** part of pre-commit hooks (too slow). Run manually for unsafe code changes:

```bash
# After modifying unsafe code
just miri-test module_name

# Or for specific unsafe function
just miri-test test_unsafe_function
```

### CI Integration

Miri runs nightly in `.github/workflows/nightly.yml`:

- **Frequency**: Daily at 00:00 UTC + on-demand
- **Mode**: `continue-on-error: true` (advisory)
- **Target**: Make strict in future versions

### When to Run Miri

**Always run Miri when**:
- Writing or modifying `unsafe` blocks
- Implementing lock-free data structures
- Using raw pointers or transmute
- Working with memory-mapped I/O
- Implementing SIMD (test scalar fallback)

**Not needed for**:
- Pure safe Rust code
- Simple property changes
- Documentation updates
- Test additions (unless testing unsafe code)

## Advanced Usage

### Tree Borrows (Experimental)

Alternative aliasing model to Stacked Borrows:

```bash
just miri-tree-borrows
```

**Pros**: More permissive, fewer false positives
**Cons**: Experimental, may miss some UB

### Custom Flags

Override `.mirirc` with environment variables:

```bash
# Increase preemption rate for more thorough race detection
MIRIFLAGS="-Zmiri-preemption-rate=0.1" cargo +nightly miri test

# Disable strict provenance for legacy code
MIRIFLAGS="-Zmiri-permissive-provenance" cargo +nightly miri test

# Combine multiple flags
MIRIFLAGS="-Zmiri-tree-borrows -Zmiri-backtrace=full" cargo +nightly miri test
```

### Debugging with Miri

```bash
# Run single test with full output
RUST_BACKTRACE=full cargo +nightly miri test test_name -- --nocapture

# Show intermediate MIR
cargo +nightly miri run --emit=mir

# Check specific file
cargo +nightly miri run --bin binary_name
```

## Best Practices

### 1. Annotate Safety Invariants

```rust
// SAFETY: Pointer is valid for the lifetime of the allocation.
// The slice length matches the allocation size (checked above).
unsafe {
    std::slice::from_raw_parts(ptr, len)
}
```

### 2. Provide Scalar Fallbacks

```rust
#[cfg(not(miri))]
pub fn fast_simd_operation() { /* ... */ }

#[cfg(miri)]
pub fn fast_simd_operation() {
    scalar_fallback()
}
```

### 3. Use `cfg_attr` for Test Isolation

```rust
#[test]
#[cfg_attr(miri, ignore = "reason")]
fn test_ffi_heavy() { /* ... */ }
```

### 4. Test Concurrency Thoroughly

```rust
#[test]
fn test_concurrent_access() {
    // Spawn multiple threads
    let handles: Vec<_> = (0..10)
        .map(|_| std::thread::spawn(|| {
            // Concurrent operations
        }))
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}
```

## Troubleshooting

### Problem: "miri: command not found"

**Solution**:
```bash
rustup +nightly component add miri
cargo +nightly miri setup
```

### Problem: "unsupported operation: can't call foreign function"

**Solution**: Mark test with `#[cfg_attr(miri, ignore)]` or provide pure-Rust alternative.

### Problem: Tests timeout under Miri

**Solution**: Reduce test dataset size or use `--test-threads=1` to avoid thread explosion.

### Problem: False positive in library code

**Solution**: Check Miri version, consider `-Zmiri-permissive-provenance`, report upstream.

## References

- [Miri GitHub Repository](https://github.com/rust-lang/miri)
- [Strict Provenance Experiment](https://rust-lang.github.io/rfcs/3559-rust-has-provenance.html)
- [Stacked Borrows Explained](https://github.com/rust-lang/unsafe-code-guidelines/blob/master/wip/stacked-borrows.md)
- [Tree Borrows (experimental)](https://perso.crans.org/vanille/treebor/)

## Future Work

- [ ] Gradually tighten Miri in CI (remove `continue-on-error`)
- [ ] Add Miri to pre-merge checks for unsafe code PRs
- [ ] Create Miri-specific test suite for concurrency edge cases
- [ ] Investigate Tree Borrows for production use
- [ ] Add custom shims for usearch if possible
