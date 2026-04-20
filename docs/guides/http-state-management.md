# HTTP Server State Management

## Overview

This guide explains how `AppState` shares an `AletheiaDB` across HTTP handlers
in AletheiaDB's `autumn-web`-based HTTP server.

## Design Decision: `Arc<AletheiaDB>` vs `Arc<RwLock<AletheiaDB>>`

**Decision**: Use `Arc<AletheiaDB>` without an outer `RwLock`.

### Why Not `Arc<RwLock<AletheiaDB>>`?

AletheiaDB is already designed for concurrent access with fine-grained interior
mutability:

| Component          | Concurrency Mechanism | Performance Characteristic |
|--------------------|-----------------------|----------------------------|
| Current Storage    | `DashMap` (lock-free) | ~22–70ns lookups           |
| Historical Storage | `RwLock<HashMap>`     | Read-heavy optimised       |
| Vector Index       | `RwLock<HNSW>`        | Parallel reads             |
| WAL                | Striped locks         | High write throughput      |
| Indexes            | Lock-free / `RwLock`  | Concurrent updates         |

Adding an outer `RwLock<AletheiaDB>` would:

- Add global lock contention (all operations acquire the same lock)
- Reduce read parallelism (readers block other readers in write mode)
- Hurt write throughput (serialise all writes)
- Contradict the internal lock-free design

### Established Pattern

The MCP server (`src/mcp/server.rs`) uses `Arc<AletheiaDB>` directly:

```rust
pub struct AletheiaMcpServer {
    db: Arc<AletheiaDB>,
}
```

This pattern is proven in production use.

## AppState Structure

```rust
pub struct AppState {
    db: Arc<AletheiaDB>,
}

impl AppState {
    pub fn new(db: Arc<AletheiaDB>) -> Self { Self { db } }
    pub fn db(&self) -> &AletheiaDB { &self.db }
    pub fn db_arc(&self) -> Arc<AletheiaDB> { self.db.clone() }
}
```

`AppState: Clone` — a cheap `Arc` bump. It is installed once per process and
read on every request.

## Wiring

The tricky bit: `autumn-web` owns its own `AppState` type (carrying metrics,
probes, config props, etc.), and axum's `State<T>` extractor binds to *one*
state slot per router. We cannot install our `AppState` directly there.

The workaround is autumn's typed extension bag: every `autumn_web::AppState`
carries an `Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>` for
downstream crates to attach typed state. We install ours there and extract it
back out via a custom axum extractor.

### Production (`run_server`)

```rust
let our_state = AppState::new(db);
let hook_state = our_state.clone();

autumn_web::app()
    .on_startup(move |autumn_state| {
        let installed = hook_state.clone();
        async move {
            autumn_state.insert_extension(installed);
            Ok(())
        }
    })
    .merge(stateful_router)
    .run()
    .await;
```

`on_startup` fires once, before the first request — the extension is always
present by the time handlers run.

### Tests (`build_test_router`)

Tests don't boot autumn's lifecycle. They install the extension directly on a
fresh `AppState::detached()` and resolve it via `Router::with_state`:

```rust
let autumn_state = autumn_web::prelude::AppState::detached();
autumn_state.insert_extension(app_state);
let router = build_stateful_router(&config, /* with_rate_limit */ false)?
    .with_state(autumn_state);
let client = autumn_web::test::TestApp::from_router(router);
```

## The Custom Extractor

Handlers declare `AppState` as a normal extractor parameter:

```rust
#[post("/query")]
async fn handle_query(
    state: AppState,
    Json(req): Json<QueryRequest>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    let db = state.db_arc();
    // ...
}
```

This works because `AppState` implements `FromRequestParts<autumn::AppState>`:

```rust
impl FromRequestParts<autumn_web::prelude::AppState> for AppState {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &autumn_web::prelude::AppState,
    ) -> Result<Self, Self::Rejection> {
        state
            .extension::<AppState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)
    }
}
```

If the extension is missing — a boot-time invariant violation — it returns
`StateMissing`, which maps to HTTP 500 via the `IntoResponse` impl on
`AletheiaHttpError`. There are no `.unwrap()` calls on the happy or sad path.

## Thread Safety Verification

The test suite (`tests/http_server.rs::state_tests`) verifies thread safety:

1. **Concurrent reads** (`app_state_concurrent_reads`) — 10 tasks × 100 reads.
2. **Concurrent writes** (`app_state_concurrent_writes`) — 10 tasks × 10 writes; 100 nodes created.
3. **Mixed operations** (`app_state_mixed_concurrent_operations`) — 5 readers + 5 writers.
4. **Deadlock prevention** (`no_deadlock_under_load`) — 20 tasks with 10s timeout.

## Performance Characteristics

| Operation      | Latency                           | Concurrency                |
|----------------|-----------------------------------|----------------------------|
| Node lookup    | 22–70ns                           | Unlimited parallel reads   |
| Node creation  | ~100ns–1ms (depends on WAL mode)  | Lock-free append           |
| Edge traversal | <1µs                              | Parallel traversals        |
| Vector search  | <10ms (k=10, 1M vectors)          | Parallel searches          |

**AppState overhead**: negligible (single `Arc` clone per request plus one
`HashMap` lookup keyed by `TypeId` in the extractor).

## Common Pitfalls

### ❌ Don't wrap in `RwLock`

```rust
// BAD — adds unnecessary global lock contention
let db = Arc::new(RwLock::new(AletheiaDB::new()?));
```

### ✅ Use `Arc` directly

```rust
// GOOD — leverages internal lock-free structures
let db = Arc::new(AletheiaDB::new()?);
let app_state = AppState::new(db);
```

### ❌ Don't call `unwrap` on `state.db()` results

```rust
// BAD — panics on contention or invalid input
let node_id = state.db().create_node("Person", props).unwrap();
```

### ✅ Return `AletheiaHttpError` and let the framework map it

```rust
let node_id = state
    .db()
    .create_node("Person", props)
    .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
```

## Why Not `State<AppState>` Directly?

axum's native `State<T>` extractor requires the state type to be the *single*
state installed on the router via `.with_state(T)`. autumn already uses that
slot for its own `AppState`. Installing ours in the extension bag and
extracting it via a custom impl avoids clashing with autumn while keeping
handler ergonomics identical.

## Related Documentation

- `src/http/state.rs` — `AppState` and the extractor impl.
- `src/http/server.rs` — wiring in `run_server` and `build_test_router`.
- `src/mcp/server.rs` — the MCP server's established `Arc<AletheiaDB>` pattern.
- [ADR 0055](../adr/0055-migrate-http-server-to-autumn.md) — the
  actix → autumn migration decision.
- `docs/ARCHITECTURE.md` — overall concurrency design.
