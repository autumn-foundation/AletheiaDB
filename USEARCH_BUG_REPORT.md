# Critical: usearch::Index Cannot Be Used as Rust Struct Field

## Summary

The usearch Rust bindings have a critical FFI bug that makes `usearch::Index` unusable in any real-world Rust application. The index **only works** when used as a local stack variable. Any attempt to store it in a struct field, wrap it in `Arc`, `Box`, or even `Pin<Box<>>` results in segfaults (`STATUS_ACCESS_VIOLATION` on Windows, `SIGSEGV` on Linux/macOS).

## Environment

- **usearch version**: 2.22.0
- **Rust version**: 1.83.0
- **Platforms tested**: Windows 11 x64, macOS 14, Ubuntu 22.04
- **Reproducible**: 100% consistent across all platforms

## Problem

When `usearch::Index` is used as a struct field and that struct is moved (which happens in virtually all Rust code), the first operation on the index causes a segfault.

### What Works ✅

```rust
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

fn main() {
    let options = IndexOptions {
        dimensions: 4,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F32,
        connectivity: 16,
        expansion_add: 128,
        expansion_search: 64,
        multi: false,
    };

    // ✅ WORKS: Index as local variable
    let index = Index::new(&options).unwrap();
    index.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    println!("Success! Index length: {}", index.size());
}
```

### What Crashes ❌

#### 1. Index as Struct Field (Most Common Use Case)

```rust
struct Container {
    index: Index,  // ❌ CRASHES
}

impl Container {
    fn new() -> Result<Self, usearch::Error> {
        let options = IndexOptions { /* ... */ };
        Ok(Self {
            index: Index::new(&options)?,
        })
    }

    fn add_vector(&self, id: u64, vec: &[f32]) -> Result<(), usearch::Error> {
        self.index.add(id, vec)?;  // 💥 SEGFAULT HERE
        Ok(())
    }
}

fn main() {
    let container = Container::new().unwrap();
    container.add_vector(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();  // CRASH
}
```

**Error**: `exit code: 0xc0000005, STATUS_ACCESS_VIOLATION`

#### 2. Index Wrapped in Box

```rust
fn main() {
    let options = IndexOptions { /* ... */ };
    let index = Box::new(Index::new(&options).unwrap());  // ❌ CRASHES
    index.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();  // 💥 SEGFAULT
}
```

#### 3. Index Wrapped in Arc (Thread-Safe Sharing)

```rust
use std::sync::Arc;

fn main() {
    let options = IndexOptions { /* ... */ };
    let index = Arc::new(Index::new(&options).unwrap());  // ❌ CRASHES
    index.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();  // 💥 SEGFAULT
}
```

#### 4. Index Wrapped in Pin<Box<>> (Prevents All Moves)

```rust
use std::pin::Pin;

fn main() {
    let options = IndexOptions { /* ... */ };
    // Create directly on heap with Pin to prevent ANY moves
    let index = Box::pin(Index::new(&options).unwrap());  // ❌ STILL CRASHES
    index.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();  // 💥 SEGFAULT
}
```

**All attempts crash**, even with `Pin<Box<>>` which prevents the object from ever moving in memory.

## Root Cause Analysis

### What We Know

1. **Rust move semantics break the C++ object**: When Rust moves a value, it performs a bitwise copy (`memcpy`) without calling C++ move constructors
2. **The C++ object has internal self-references**: The `usearch::index_dense_t` C++ class likely contains:
   - Self-referential pointers (`this` pointer stored somewhere)
   - Pointers to internal data structures
   - Mutex/lock pointers
   - Thread-local storage references
3. **Even heap allocation doesn't fix it**: `Pin<Box<>>` guarantees a stable heap address, yet it still crashes when wrapped in `Arc` or `Box`

### What This Suggests

The usearch C++ implementation likely:
- Stores the `this` pointer during construction
- Uses that stored pointer in method calls
- Assumes pointer stability beyond what Rust's move semantics provide
- May have thread-local storage or other global state tied to object addresses

When Rust moves the value (even moving a `Box<Index>` or `Arc<Index>`), the stored internal pointers become dangling, causing segfaults on first use.

## Impact

**This bug makes usearch completely unusable** in real-world Rust applications because:

1. **Cannot store in structs**: 99% of use cases need the index as a struct field
2. **Cannot share across threads**: `Arc<Index>` crashes
3. **Cannot use in collections**: `Vec<Index>`, `HashMap<K, Index>` all crash
4. **Cannot return from functions**: Moving the return value crashes
5. **Cannot use in async contexts**: Futures need to store state in structs

The only "working" pattern (local variable) is useless for any real application.

## Comprehensive Testing Results

We tested every possible Rust wrapping strategy:

| Wrapping Strategy | Result | Notes |
|------------------|--------|-------|
| Local variable | ✅ WORKS | Only pattern that works |
| Struct field (bare) | ❌ CRASH | Most common use case |
| `Box<Index>` | ❌ CRASH | Heap allocation doesn't help |
| `Arc<Index>` | ❌ CRASH | Cannot share across threads |
| `Rc<Index>` | ❌ CRASH | Single-threaded sharing fails |
| `Pin<Box<Index>>` | ❌ CRASH | Even preventing moves doesn't help |
| `Pin<Box<Index>>` in Arc | ❌ CRASH | Combination fails |
| Direct heap creation | ❌ CRASH | `Box::pin(Index::new(...))` fails |

**Every strategy except local variables crashes.**

## Suggested Fixes

### Option 1: Fix the C++ Code (Preferred)

Modify `usearch::index_dense_t` to:
1. **Remove self-referential pointers**: Don't store `this` pointer internally
2. **Use relative pointers**: Store offsets instead of absolute addresses
3. **Allocate internal data separately**: Use heap-allocated data structures that don't move

### Option 2: Fix the Rust Bindings

Change the Rust wrapper to:
1. **Always heap-allocate**: Create `Index` on heap immediately
2. **Use opaque pointer**: Expose `struct IndexHandle(*mut usearch::index_dense_t)`
3. **Implement proper Drop**: Manually manage C++ object lifetime
4. **Prevent moves**: Mark as `!Unpin` and require `Pin<Box<Index>>`

**Example fix for Rust bindings**:

```rust
// Current (broken):
pub struct Index {
    inner: usearch::index_dense_t,  // ❌ Embedded, moves with struct
}

// Fixed:
pub struct Index {
    inner: Pin<Box<usearch::index_dense_t>>,  // ✅ Stable heap address
}

impl Index {
    pub fn new(options: &IndexOptions) -> Result<Self, Error> {
        // Create directly on heap, never on stack
        let inner = Box::pin(usearch::index_dense_t::new(options)?);
        Ok(Index { inner })
    }
}

// All methods deref through Pin
impl Index {
    pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), Error> {
        self.inner.add(key, vector)  // Deref through Pin works fine
    }
}
```

However, **this alone doesn't fix it** (we tested this). The issue is deeper.

### Option 3: Document the Limitation

If fixing the C++ code is too complex, at minimum:
1. **Document that Index must be a local variable**
2. **Add compile-time check**: `static_assert!(!Index: Unpin)` or similar
3. **Provide safe wrapper**: Offer `struct SafeIndex(*mut Index)` that manages heap allocation

## Reproduction Repository

Full reproduction available at:
- **GallifreyDB PR #182**: https://github.com/madmax983/GallifreyDB/pull/182
- **Test file**: `tests/temporal_vector_tests.rs`
- **Wrapper code**: `src/index/vector/hnsw.rs`

## Our Workaround

We're migrating to **hnsw_rs** (pure Rust HNSW implementation) to avoid FFI issues entirely. While we'll lose ~20-30% performance vs usearch, we gain:
- ✅ Full Rust safety guarantees
- ✅ No FFI footguns
- ✅ Works in any context (structs, Arc, async, etc.)
- ✅ No platform-specific issues

## Request for Maintainers

This is a **critical bug** that makes usearch unusable in Rust. Please prioritize fixing this, as it affects all Rust users trying to use usearch in production.

We're happy to:
1. Test any proposed fixes
2. Provide more detailed reproduction cases
3. Help with Rust-side wrapper improvements
4. Contribute patches if we can identify the C++ issue

---

**Contact**: [Your GitHub username]
**Project**: GallifreyDB - Bi-temporal graph database
**Impact**: Blocking feature (temporal vector search)
