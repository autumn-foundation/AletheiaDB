# Security: CRC32 Checksums May Be Insufficient for Integrity Protection

**Labels**: `security`, `automated-scan`, `medium`, `P2`, `cryptography`, `wal`
**Priority**: P2 - Medium priority

## Summary
WAL uses CRC32 checksums for corruption detection. While CRC32 is fast and detects accidental corruption well, it's **not cryptographically secure** and vulnerable to targeted attacks. An adversary with write access to WAL files can forge valid checksums.

## Location
- **File**: `src/storage/wal.rs`
- **Line**: 174-180 (checksum verification)
- **Library**: `crc32fast` crate (Cargo.toml:14)

## Code
```rust
pub fn verify_checksum(&self, serialized_data: &[u8]) -> bool {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&serialized_data[0..16]); // LSN + timestamp
    hasher.update(&serialized_data[20..]); // Operation data
    let computed = hasher.finalize();
    stored_checksum == computed
}
```

## Severity
**MEDIUM**

## Impact
- **Collision Attacks**: Attacker can craft data with same CRC32 (only 2^32 possible values)
- **Bitflip Tolerance**: Multiple bitflips can cancel out in CRC32 polynomial
- **Targeted Corruption**: Adversary can modify data to match existing checksum
- **Not Tamper-Proof**: No authentication, only error detection
- **Limited Protection**: Only guards against accidental corruption, not malicious tampering

## Threat Model

### Threat 1: Compromised Backup
**Scenario**:
1. Attacker gains access to database backups (S3 bucket misconfiguration, stolen laptop)
2. Modifies WAL files to inject malicious transactions
3. Computes new CRC32 that matches modified data (trivial - O(milliseconds))
4. Victim restores from "backup"
5. Database replays corrupted WAL → malicious transactions executed

**Impact**: Data integrity violation, unauthorized transactions

### Threat 2: Disk Corruption + Attacker
**Scenario**:
1. Natural disk corruption occurs (bad sectors, cosmic rays)
2. Attacker knows about corruption (monitoring system logs)
3. Modifies corrupted data to exploit vulnerability
4. Computes valid CRC32 for malicious payload
5. Database loads corrupted data, checksum passes

**Impact**: Exploit delivered via "natural" corruption

### Non-Threat: Physical Disk Errors
CRC32 **is sufficient** for:
- ✅ Random bit flips (cosmic rays, memory errors)
- ✅ Sector corruption (hardware failures)
- ✅ Incomplete writes (power loss)
- ✅ Transmission errors

## Attack Feasibility

### CRC32 Collision Complexity
- **Time**: O(milliseconds) on modern CPU
- **Skill**: Undergraduate computer science
- **Tools**: Widely available (Python, C libraries)
- **Example**:
  ```python
  import zlib

  original_data = b"legitimate transaction"
  malicious_data = b"malicious transaction "
  target_crc = zlib.crc32(original_data)

  # Find suffix that makes malicious_data have target_crc
  # Brute force over 2^32 space (trivial)
  ```

### Why CRC32 is Weak
1. **Not Cryptographic**: Designed for error detection, not security
2. **Small Space**: Only 2^32 possible checksums (4.3 billion)
3. **Linear**: CRC(A XOR B) = CRC(A) XOR CRC(B)
4. **Predictable**: No secret key, anyone can compute

## Recommended Fixes

### Option 1: Upgrade to BLAKE3 (Recommended for 1.0)
Fast cryptographic hash, tamper-evident:

```rust
use blake3;

pub fn compute_checksum(data: &[u8]) -> [u8; 32] {
    blake3::hash(data).into()
}

pub fn verify_checksum(&self, serialized_data: &[u8]) -> bool {
    let computed = blake3::hash(serialized_data);
    let stored = &serialized_data[16..48]; // 32-byte hash
    computed.as_bytes() == stored
}
```

**Pros**:
- ✅ Cryptographically secure (2^256 space)
- ✅ Extremely fast (~1 GB/s, competitive with CRC32)
- ✅ Tamper-evident (infeasible to forge)
- ✅ Drop-in replacement (no key management)

**Cons**:
- ⚠️ Slightly slower than CRC32 (~2-3x)
- ⚠️ Larger checksum (32 bytes vs 4 bytes)

**Dependencies**:
```toml
[dependencies]
blake3 = "1.5"
```

### Option 2: HMAC-SHA256 (Maximum Security)
Authenticated integrity with secret key:

```rust
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub fn compute_mac(data: &[u8], key: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn verify_mac(&self, data: &[u8], stored_mac: &[u8], key: &[u8]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(data);
    mac.verify_slice(stored_mac).is_ok()
}
```

**Pros**:
- ✅ Authenticated (proves data came from key holder)
- ✅ Cryptographically secure
- ✅ Standard algorithm (FIPS 198-1)

**Cons**:
- ⚠️ Requires key management
- ⚠️ Key rotation complexity
- ⚠️ Slower than BLAKE3

### Option 3: Document Limitations (Pre-1.0 - Minimum)
If keeping CRC32 for pre-1.0, **document clearly**:

```rust
/// Verify the checksum against serialized data.
///
/// # Security Note
///
/// ⚠️ **This uses CRC32 which is NOT cryptographically secure.**
///
/// CRC32 detects accidental corruption (hardware failures, bitflips) but
/// **NOT malicious tampering**. An attacker with write access to WAL files
/// can forge valid checksums.
///
/// **For production deployments requiring tamper detection**:
/// - Upgrade to BLAKE3 (see docs/SECURITY.md)
/// - Use HMAC-SHA256 with key management
/// - Store WAL files on tamper-evident storage (e.g., append-only S3)
///
/// **Acceptable for pre-1.0 because**:
/// - Focus is on development, not production
/// - Primary threat is accidental corruption, not adversarial tampering
/// - No encryption at rest (documented limitation)
///
/// See [Issue #004](link) for upgrade plan.
pub fn verify_checksum(&self, serialized_data: &[u8]) -> bool {
    // ... existing implementation
}
```

## Comparison Table

| Algorithm | Speed | Collision Resistance | Tamper Detection | Key Required | Size |
|-----------|-------|---------------------|------------------|--------------|------|
| **CRC32** | Fast | Poor (2^32) | ❌ No | No | 4 bytes |
| **BLAKE3** | Fast | Excellent (2^256) | ✅ Yes | No | 32 bytes |
| **SHA-256** | Good | Excellent (2^256) | ✅ Yes | No | 32 bytes |
| **HMAC-SHA256** | Good | Excellent (2^256) | ✅ Yes + Auth | Yes | 32 bytes |

### Performance Benchmark (Estimated)
| Algorithm | Throughput | Overhead vs CRC32 |
|-----------|------------|-------------------|
| CRC32 | ~3 GB/s | 1x (baseline) |
| BLAKE3 | ~1 GB/s | ~3x slower |
| SHA-256 | ~500 MB/s | ~6x slower |
| HMAC-SHA256 | ~450 MB/s | ~7x slower |

**Note**: WAL writes are not CPU-bound (disk I/O dominates), so 3x slowdown is acceptable.

## Recommended Approach

### Phase 1: Pre-1.0 (Current)
- ✅ Document CRC32 limitations (Option 3)
- ✅ Add to `SECURITY.md` known limitations
- ✅ Test with accidental corruption (fuzzing)

### Phase 2: 1.0 Release
- 🎯 Upgrade to BLAKE3 (Option 1)
- 🎯 WAL format version bump (v2 → v3)
- 🎯 Backward compatibility for reading v2
- 🎯 Migration tool for existing WAL files

### Phase 3: Enterprise (Post-1.0)
- 🔮 Add HMAC-SHA256 option (Option 2)
- 🔮 Key management integration
- 🔮 Encryption at rest

## Implementation Plan (Option 1 - BLAKE3)

### Step 1: Add Dependency
```toml
[dependencies]
blake3 = "1.5"
```

### Step 2: Update WAL Entry
```rust
pub struct WalEntry {
    pub lsn: LSN,
    pub timestamp: Timestamp,
    pub operation: WalOperation,
    pub checksum: [u8; 32], // Changed from u32 to [u8; 32]
}
```

### Step 3: Compute Checksum
```rust
impl WalEntry {
    pub fn new(lsn: LSN, operation: WalOperation) -> Self {
        let timestamp = time::now();
        // Serialize operation to compute checksum
        let mut buffer = Vec::new();
        serialize_wal_entry_without_checksum(&mut buffer, lsn, timestamp, &operation);
        let checksum = blake3::hash(&buffer).into();

        WalEntry {
            lsn,
            timestamp,
            operation,
            checksum,
        }
    }

    pub fn verify_checksum(&self, serialized_data: &[u8]) -> bool {
        let computed = blake3::hash(serialized_data);
        computed.as_bytes() == &self.checksum
    }
}
```

### Step 4: Update WAL Format Version
```rust
const WAL_VERSION: u8 = 3; // Was 2 (CRC32), now 3 (BLAKE3)
```

### Step 5: Backward Compatibility
```rust
pub fn read_entry(reader: &mut impl Read) -> Result<WalEntry> {
    let version = read_u8(reader)?;
    match version {
        2 => read_entry_v2(reader), // Old CRC32 format
        3 => read_entry_v3(reader), // New BLAKE3 format
        _ => Err(Error::UnsupportedWalVersion(version)),
    }
}
```

## Testing Requirements

### Test 1: Collision Resistance
```rust
#[test]
fn test_checksum_collision_resistance() {
    let data1 = b"transaction 1";
    let data2 = b"transaction 2";

    let hash1 = blake3::hash(data1);
    let hash2 = blake3::hash(data2);

    assert_ne!(hash1, hash2, "Hashes should be different");
}
```

### Test 2: Tamper Detection
```rust
#[test]
fn test_tamper_detection() {
    let original_data = b"original transaction";
    let original_hash = blake3::hash(original_data);

    // Modify single bit
    let mut tampered_data = original_data.to_vec();
    tampered_data[0] ^= 0x01;
    let tampered_hash = blake3::hash(&tampered_data);

    assert_ne!(original_hash, tampered_hash, "Tamper should be detected");
}
```

### Test 3: Backward Compatibility
```rust
#[test]
fn test_read_v2_wal_files() {
    let v2_wal_data = create_v2_wal_file();
    let entry = read_entry(&mut &v2_wal_data[..]).unwrap();
    assert!(matches!(entry.version, WalVersion::V2));
}
```

## References
- [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) - Fast cryptographic hash
- [CRC vs Cryptographic Hashes](https://security.stackexchange.com/questions/49850)
- [NIST FIPS 180-4](https://csrc.nist.gov/publications/detail/fips/180/4/final) - SHA-2
- [NIST FIPS 198-1](https://csrc.nist.gov/publications/detail/fips/198/1/final) - HMAC

## Related Issues
- #001 (WAL replay not implemented) - same file
- SECURITY.md - document known limitations

## Priority
**P2 - Medium priority**

- **Pre-1.0**: Document limitations (low effort, immediate)
- **1.0 Release**: Upgrade to BLAKE3 (medium effort, required for production)

## Estimated Effort
- Documentation: 1-2 hours
- BLAKE3 upgrade: 3-5 days (implementation + testing + migration)
- HMAC: 5-7 days (additional key management)
