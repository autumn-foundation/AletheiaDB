# HTTP Server State Management

## Overview

This guide explains how `AppState` manages GallifreyDB state sharing across HTTP handlers in the actix-web server.

## Design Decision: Arc<GallifreyDB> vs Arc<RwLock<GallifreyDB>>

**Decision**: Use `Arc<GallifreyDB>` without an outer `RwLock`.

### Why Not Arc<RwLock<GallifreyDB>>?

GallifreyDB is already designed for concurrent access with fine-grained interior mutability:

| Component | Concurrency Mechanism | Performance Characteristic |
|-----------|----------------------|----------------------------|
| Current Storage | `DashMap` (lock-free) | ~22-70ns lookups |
| Historical Storage | `RwLock<HashMap>` | Read-heavy optimized |
| Vector Index | `RwLock<HNSW>` | Parallel reads |
| WAL | Striped locks | High write throughput |
| Indexes | Lock-free/RwLock | Concurrent updates |

Adding an outer `RwLock<GallifreyDB>` would:
- Add global lock contention (all operations acquire same lock)
- Reduce read parallelism (readers block other readers in write mode)
- Hurt write throughput (serialize all writes)
- Contradict internal lock-free design

### Established Pattern

The MCP server (see `src/mcp/server.rs`) successfully uses `Arc<GallifreyDB>` directly:

```rust
pub struct GallifreyMcpServer {
    db: Arc<GallifreyDB>,
}
```

This pattern is proven in production use.

## Thread Safety Verification

The test suite (`tests/http_server.rs`) verifies thread safety through:

1. **Concurrent Reads** (`test_app_state_concurrent_reads`)
   - 10 tasks × 100 reads each = 1000 concurrent reads
   - All succeed, no data races

2. **Concurrent Writes** (`test_app_state_concurrent_writes`)
   - 10 tasks × 10 writes each = 100 concurrent writes
   - All 100 nodes created correctly

3. **Mixed Operations** (`test_app_state_mixed_concurrent_operations`)
   - 5 readers + 5 writers running simultaneously
   - Readers never blocked by writers
   - All operations succeed

4. **Deadlock Prevention** (`test_no_deadlock_under_load`)
   - 20 tasks doing mixed operations with 10s timeout
   - No deadlocks detected

## Usage

### Basic Setup

```rust
use gallifreydb::{GallifreyDB, http::AppState};
use actix_web::{web, App, HttpServer};
use std::sync::Arc;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Create database
    let db = Arc::new(GallifreyDB::new().unwrap());
    let app_state = AppState::new(db);

    // Create server with shared state
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .configure(gallifreydb::http::configure_app)  // Use existing configure_app
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
```

### Handler Example

```rust
use actix_web::{web, HttpResponse};
use gallifreydb::http::AppState;

async fn get_node_count(state: web::Data<AppState>) -> HttpResponse {
    let count = state.db().node_count();
    HttpResponse::Ok().json(count)
}

async fn create_node(
    state: web::Data<AppState>,
    body: web::Json<CreateNodeRequest>
) -> HttpResponse {
    let request = body.into_inner();
    let node_id = state.db()
        .create_node(&request.label, request.properties)
        .unwrap();
    HttpResponse::Ok().json(node_id)
}
```

## API Reference

### AppState

```rust
pub struct AppState {
    db: Arc<GallifreyDB>,
}
```

**Methods:**

- `new(db: Arc<GallifreyDB>) -> Self` - Create new state
- `db(&self) -> &GallifreyDB` - Get database reference for method calls
- `db_arc(&self) -> Arc<GallifreyDB>` - Clone the Arc (rarely needed)

**Traits:**

- `Clone` - Required for actix-web worker sharing
- `From<Arc<GallifreyDB>>` - Ergonomic construction

### Example: Using db() vs db_arc()

```rust
// Most common: use db() for method calls
let count = state.db().node_count();

// Rare: use db_arc() when you need the Arc itself
let db_copy = state.db_arc();
let another_state = AppState::new(db_copy);
```

## Performance Characteristics

Based on GallifreyDB's internal architecture:

| Operation | Latency | Concurrency |
|-----------|---------|-------------|
| Node lookup | 22-70ns | Unlimited parallel reads |
| Node creation | ~100ns-1ms (depends on WAL mode) | Lock-free append |
| Edge traversal | <1µs | Parallel traversals |
| Vector search | <10ms (k=10, 1M vectors) | Parallel searches |

**AppState overhead**: Negligible (single Arc clone per request)

## Implementation Details

### Why Clone is Needed

Actix-web uses a multi-worker architecture. Each worker gets a clone of the App state:

```
Worker 1 ← AppState.clone()
Worker 2 ← AppState.clone()  →  Same Arc<GallifreyDB>
Worker 3 ← AppState.clone()
```

Cloning AppState is cheap (just clones the Arc, incrementing ref count).

### Memory Layout

```
HTTP Handler 1 ─┐
HTTP Handler 2 ─┼→ AppState → Arc<GallifreyDB> → GallifreyDB
HTTP Handler 3 ─┘                                      ↓
                                                  DashMap/RwLock/etc
```

All handlers share one GallifreyDB instance via reference counting.

## Testing

Run state management tests:

```bash
# All AppState tests
cargo test --test http_server --features http-server state_tests

# Specific concurrency test
cargo test --test http_server --features http-server test_app_state_concurrent_writes
```

## Common Pitfalls

### ❌ Don't: Expect you need &mut for mutations

```rust
async fn confused_handler(
    state: web::Data<AppState>,
    // properties: PropertyMap from request body
) -> HttpResponse {
    let db: &GallifreyDB = state.db();
    // GallifreyDB methods like `create_node` take `&self` (not `&mut self`)
    // and use interior mutability, so this compiles even though it mutates state.
    let node_id = db.create_node("Person", properties).unwrap();
    HttpResponse::Ok().json(node_id)
}
```

### ✅ Do: Use &self methods directly with proper error handling

```rust
async fn good_handler(
    state: web::Data<AppState>,
    // properties: PropertyMap from request body
) -> actix_web::Result<HttpResponse> {
    // All GallifreyDB methods take &self and use interior mutability
    let node_id = state.db().create_node("Person", properties)?;
    Ok(HttpResponse::Ok().json(node_id))
}
```

### ❌ Don't: Wrap in RwLock

```rust
// BAD - adds unnecessary global lock contention
let db = Arc::new(RwLock::new(GallifreyDB::new()?));
```

### ✅ Do: Use Arc directly

```rust
// GOOD - leverages internal lock-free structures
let db = Arc::new(GallifreyDB::new()?);
let app_state = AppState::new(db);
```

## Related Documentation

- **HTTP Server Setup**: `docs/guides/http-server-guide.md` (coming soon)
- **MCP Server**: `src/mcp/server.rs` (established Arc pattern)
- **Architecture**: `docs/ARCHITECTURE.md` (concurrency design)
- **Issue #466**: Original implementation issue

## Future Enhancements

Planned additions to AppState:

1. **Metrics Collection**
   ```rust
   pub struct AppState {
       db: Arc<GallifreyDB>,
       metrics: Arc<MetricsCollector>,  // Future
   }
   ```

2. **Configuration**
   ```rust
   pub struct AppState {
       db: Arc<GallifreyDB>,
       config: Arc<ServerConfig>,  // Future
   }
   ```

3. **Request Context**
   ```rust
   pub struct AppState {
       db: Arc<GallifreyDB>,
       request_id_generator: Arc<RequestIdGen>,  // Future
   }
   ```

The wrapper pattern makes these extensions easy without changing handler signatures.
