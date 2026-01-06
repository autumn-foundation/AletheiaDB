# Security: WAL Constructor Panics on Startup Errors

**Labels**: `security`, `automated-scan`, `high`, `P1`, `startup`
**Priority**: P1 - High priority

## Summary
Database constructor panics if WAL directory creation fails, preventing graceful error handling at startup. This can cause service unavailability and denial of service.

## Location
- **File**: `src/db.rs`
- **Line**: 59
- **Function**: `GallifreyDB::with_config()`

## Code
```rust
pub fn with_config(config: AnchorConfig) -> Self {
    // Create WAL with default config (can be made configurable later)
    let wal = WriteAheadLog::new(WalConfig::default())
        .expect("Failed to create WAL");
    //  ^-- PANICS instead of returning Err
    ...
}
```

## Severity
**HIGH**

## Impact
- **Service Unavailability**: Database won't start on permission errors, full disk, or filesystem issues
- **Poor Error Messages**: Stack trace instead of actionable error message
- **Container Restarts**: Orchestrators (K8s, Docker) repeatedly restart on panic → resource exhaustion
- **Startup DoS**: Attacker with low-privilege filesystem access can prevent service startup
- **Operations Burden**: No graceful degradation or recovery path

## Attack Scenario
1. Attacker gains limited filesystem access (e.g., shared volume, container escape)
2. Creates file named `gallifreydb/wal` (conflict with expected directory)
3. Database startup tries to create WAL directory
4. Filesystem error occurs (file vs directory conflict)
5. `expect()` panics → service won't start
6. Container orchestrator repeatedly restarts → CPU/log spam
7. Service remains unavailable until manual intervention

**Alternative**: Attacker fills disk → WAL creation fails → same panic path.

## Additional Examples

Similar panic paths exist in:
- Lock acquisitions: `self.current_timestamp.lock().unwrap()`
- Node/Edge retrieval: `db.get_node(id).unwrap()`
- Transaction operations

But constructor panic is **most critical** because:
1. Happens at startup (blocks service completely)
2. No recovery path (requires manual intervention)
3. Creates restart loops in orchestrated environments

## Expected Behavior
Constructor should return `Result` and allow caller to handle error:

```rust
match GallifreyDB::new() {
    Ok(db) => {
        info!("Database initialized successfully");
        db
    }
    Err(e) => {
        error!("Failed to initialize database: {}", e);
        error!("Please check:");
        error!("  - Directory permissions for WAL storage");
        error!("  - Available disk space");
        error!("  - No file/directory conflicts");
        std::process::exit(1);
    }
}
```

## Recommended Fix

### Step 1: Change Constructor Signature
```rust
// Before
impl GallifreyDB {
    pub fn new() -> Self { ... }
    pub fn with_config(config: AnchorConfig) -> Self { ... }
}

// After
impl GallifreyDB {
    pub fn new() -> Result<Self, Error> { ... }
    pub fn with_config(config: AnchorConfig) -> Result<Self, Error> { ... }
}
```

### Step 2: Implement Error Handling
```rust
pub fn with_config(config: AnchorConfig) -> Result<Self, Error> {
    let wal = WriteAheadLog::new(WalConfig::default())
        .map_err(|e| {
            Error::Storage(StorageError::Initialization(format!(
                "Failed to initialize WAL: {}. \
                 Check directory permissions and disk space at {:?}",
                e,
                WalConfig::default().wal_dir
            )))
        })?;

    let current_storage = CurrentStorage::new(config.clone())?;
    let historical_storage = HistoricalStorage::new(config)?;

    Ok(GallifreyDB {
        wal,
        current_storage: Arc::new(Mutex::new(current_storage)),
        historical_storage: Arc::new(Mutex::new(historical_storage)),
        // ... other fields
    })
}
```

### Step 3: Add New Error Variant
```rust
// In src/utils/error.rs
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    // ... existing variants

    /// Database initialization failed
    #[error("Initialization failed: {0}")]
    Initialization(String),
}
```

### Step 4: Update Call Sites

**Example 1: Library Usage**
```rust
// Before (in tests)
let db = GallifreyDB::new();

// After
let db = GallifreyDB::new().unwrap(); // Test code can still unwrap
```

**Example 2: Application Startup**
```rust
// In main.rs or application code
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = GallifreyDB::new()
        .map_err(|e| {
            eprintln!("FATAL: Database initialization failed");
            eprintln!("{}", e);
            e
        })?;

    // Continue with application logic
    Ok(())
}
```

## Testing Requirements

### Test 1: File Conflict
```rust
#[test]
fn test_wal_directory_conflict() {
    let temp_dir = tempfile::tempdir().unwrap();
    let wal_path = temp_dir.path().join("gallifreydb/wal");

    // Create file where directory should be
    std::fs::create_dir_all(temp_dir.path().join("gallifreydb")).unwrap();
    std::fs::write(&wal_path, "conflict").unwrap();

    // Should return error, not panic
    let result = GallifreyDB::with_wal_dir(&wal_path);
    assert!(result.is_err());
    assert!(format!("{:?}", result.unwrap_err()).contains("Initialization"));
}
```

### Test 2: Read-Only Filesystem
```rust
#[test]
fn test_readonly_filesystem() {
    // Create read-only directory
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path();

    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, Permissions::from_mode(0o444)).unwrap();
    }

    let result = GallifreyDB::with_wal_dir(path);
    assert!(result.is_err());
}
```

### Test 3: Full Disk Simulation
```rust
// Integration test with disk quota or mock filesystem
```

## Migration Guide for Users

### Breaking Change Notice
```markdown
## Breaking Change in v0.2.0

### Constructor Now Returns Result

**Before**:
```rust
let db = GallifreyDB::new();
```

**After**:
```rust
let db = GallifreyDB::new()?; // Propagate error
// or
let db = GallifreyDB::new().expect("Database initialization failed");
```

**Rationale**: Prevents panic on filesystem errors, provides better error messages.
```

## Related Issues
- #002 (excessive unwrap usage) - same pattern
- Similar pattern in other constructors?

## Priority
**P1 - High priority**

Should be fixed before any production deployment. Startup reliability is critical.

## Estimated Effort
- 2-3 days for implementation
- 1-2 days for testing and migration guide
- Breaking change - requires version bump
