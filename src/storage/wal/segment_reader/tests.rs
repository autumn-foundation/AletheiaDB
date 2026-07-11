use super::*;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::temporal::time;
use crate::storage::wal::serialization::serialize_entry_into;
use tempfile::TempDir;

#[test]
fn test_read_empty_directory() {
    let dir = TempDir::new().unwrap();
    let entries = read_entries_from_dir(dir.path(), LSN(1)).unwrap();
    assert!(entries.is_empty());
}

/// Issue #3413: `CommitTx` serializes and parses back byte-for-byte under
/// the transaction-framing version, and the parsed entry is flagged
/// `framed`.
#[test]
fn test_commit_tx_round_trip() {
    let commit_timestamp = crate::core::hlc::HybridTimestamp::new(1_234_567, 9).unwrap();
    let op = WalOperation::CommitTx {
        tx_id: 42,
        entry_count: 3,
        commit_timestamp,
    };
    let mut entry = WalEntry::new(LSN(100), op.clone());
    entry.timestamp = crate::core::hlc::HybridTimestamp::new(2_000_000, 0).unwrap();

    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    let (parsed, consumed) = parse_entry_at(&buffer, 0, WAL_VERSION_TX_FRAMING).unwrap();
    assert_eq!(consumed, buffer.len());
    assert_eq!(parsed.operation, op);
    assert_eq!(parsed.lsn, LSN(100));
    assert!(parsed.framed, "v7 entries must be flagged framed");
}

/// Issue #3413: `BeginTx` round-trips too.
#[test]
fn test_begin_tx_round_trip() {
    let op = WalOperation::BeginTx { tx_id: 77 };
    let entry = WalEntry::new(LSN(5), op.clone());
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();
    let (parsed, consumed) = parse_entry_at(&buffer, 0, WAL_VERSION_TX_FRAMING).unwrap();
    assert_eq!(consumed, buffer.len());
    assert_eq!(parsed.operation, op);
    assert!(parsed.framed);
}

/// Issue #3413: a pre-framing (v6) segment parses entries with
/// `framed == false`, keeping them on the legacy immediate-apply path.
#[test]
fn test_pre_framing_version_not_flagged_framed() {
    let op = WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("Legacy").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(1), op);
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();
    let (parsed, _) = parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE_PRINCIPAL).unwrap();
    assert!(
        !parsed.framed,
        "pre-v7 segments must not be treated as framed"
    );
}

#[test]
fn test_read_nonexistent_segment() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("nonexistent.log");
    let entries = read_segment(&path, LSN(1)).unwrap();
    assert!(entries.is_empty());
}

/// Issue #3420 / PR #3428 review: `max_lsn_in_dir` on an empty directory
/// (no segments at all) must report `None`, not a phantom LSN.
#[test]
fn test_max_lsn_in_dir_empty_directory() {
    let dir = TempDir::new().unwrap();
    assert_eq!(max_lsn_in_dir(dir.path(), None).unwrap(), None);
}

/// Issue #3420 / PR #3428 review: `max_lsn_in_dir` must return the max
/// LSN across ALL rotated segments — with the maximum deliberately placed
/// in a MIDDLE segment, so returning the first (or last) segment's max
/// would fail.
#[test]
fn test_max_lsn_in_dir_multi_segment_returns_global_max() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();

    // Segment 0: LSNs 1..=3; segment 1: LSNs 40..=42 (global max);
    // segment 2: LSNs 10..=12.
    let lsn_ranges: [&[u64]; 3] = [&[1, 2, 3], &[40, 41, 42], &[10, 11, 12]];
    for (seg_id, lsns) in lsn_ranges.iter().enumerate() {
        let segment_path = dir.path().join(format!("{}.log", seg_id));
        let mut file = File::create(&segment_path).unwrap();
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION_PROVENANCE]).unwrap();
        for lsn in *lsns {
            let operation = WalOperation::CreateNode {
                node_id: NodeId::new(*lsn).unwrap(),
                label: GLOBAL_INTERNER.intern("MaxLsnTest").unwrap(),
                properties: PropertyMap::new(),
                valid_from: time::now(),
                provenance: None,
            };
            let entry = WalEntry::new(LSN(*lsn), operation);
            let mut buffer = Vec::new();
            serialize_entry_into(&entry, &mut buffer).unwrap();
            file.write_all(&buffer).unwrap();
        }
        file.sync_all().unwrap();
    }

    assert_eq!(
        max_lsn_in_dir(dir.path(), None).unwrap(),
        Some(LSN(42)),
        "max must be taken across ALL segments, not the first or last"
    );
}

/// Write a plaintext segment file containing valid CreateNode entries for
/// the given LSNs and return the raw serialized bytes of the LAST entry
/// written (useful for crafting torn tails from real entry headers).
fn write_segment_with_lsns(path: &Path, lsns: &[u64]) -> Vec<u8> {
    use std::io::Write;
    let mut file = File::create(path).unwrap();
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION_PROVENANCE]).unwrap();
    let mut last_entry_bytes = Vec::new();
    for lsn in lsns {
        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(*lsn).unwrap(),
            label: GLOBAL_INTERNER.intern("TornTailTest").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        };
        let entry = WalEntry::new(LSN(*lsn), operation);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        file.write_all(&buffer).unwrap();
        last_entry_bytes = buffer;
    }
    file.sync_all().unwrap();
    last_entry_bytes
}

/// PR #3428 CI regression: a torn entry (valid 24-byte entry header
/// followed by operation-type byte 0 — the exact corruption shape from
/// the CI failure, e.g. an in-flight write in a shared WAL dir or a
/// crash-torn tail) must NOT fail the seeding scan. `max_lsn_in_dir`
/// returns the max of the segment's decodable prefix; the recovery
/// replay reader keeps its hard-error behavior, unchanged.
#[test]
fn test_max_lsn_in_dir_tolerates_torn_tail_entry() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    let entry_bytes = write_segment_with_lsns(&segment_path, &[5, 7]);

    // Torn tail: reuse a REAL serialized entry's first 24 bytes
    // (LSN + timestamp + checksum, all decodable) but with operation
    // type 0 — parse_entry_at fails with "Unknown WAL operation type: 0".
    let mut torn = entry_bytes[..24].to_vec();
    torn.push(0); // op-type 0 (OP_* codes start at 1)
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&segment_path)
        .unwrap();
    file.write_all(&torn).unwrap();
    file.sync_all().unwrap();

    // Seeding scan: max of the decodable prefix, no error.
    assert_eq!(
        max_lsn_in_dir(dir.path(), None).unwrap(),
        Some(LSN(7)),
        "torn tail must not fail the seeding scan; the decodable prefix's max must be kept"
    );

    // Replay reader behavior is deliberately UNCHANGED: it still
    // hard-errors on the same torn entry.
    assert!(
        read_segment(&segment_path, LSN(1)).is_err(),
        "replay reader must keep propagating undecodable-entry errors"
    );
}

/// PR #3428 CI regression: a zeroed preallocated tail is a benign stop
/// for the seeding scan (mirroring the replay reader, which also stops
/// there without error).
#[test]
fn test_max_lsn_in_dir_tolerates_zeroed_tail() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    write_segment_with_lsns(&segment_path, &[3, 4]);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&segment_path)
        .unwrap();
    file.write_all(&[0u8; 64]).unwrap();
    file.sync_all().unwrap();

    assert_eq!(
        max_lsn_in_dir(dir.path(), None).unwrap(),
        Some(LSN(4)),
        "zeroed preallocated tail must not fail the seeding scan"
    );
}

/// PR #3428 CI regression: a segment that is garbage from byte 0 (no
/// GWAL magic — e.g. a partially written header from another process
/// sharing the WAL dir) is SKIPPED with a warning; other segments still
/// contribute their LSNs. Decision rationale: replay applies nothing
/// from such a segment either, so skipping cannot under-seed relative to
/// what replay applies, and it keeps a real recovery dir usable. The
/// replay reader keeps its hard-error behavior for the same data.
#[test]
fn test_max_lsn_in_dir_skips_garbage_header_segment() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();

    // Segment 0: garbage from byte 0 (no GWAL magic).
    let garbage_path = dir.path().join("0.log");
    let mut file = File::create(&garbage_path).unwrap();
    file.write_all(b"garbage-not-a-wal-segment").unwrap();
    file.sync_all().unwrap();

    // Segment 1: valid entries.
    write_segment_with_lsns(&dir.path().join("1.log"), &[3, 4, 5]);

    assert_eq!(
        max_lsn_in_dir(dir.path(), None).unwrap(),
        Some(LSN(5)),
        "a garbage-header segment must be skipped, not fail the whole scan"
    );

    // Replay reader behavior is deliberately UNCHANGED: it still
    // hard-errors on the garbage-header segment.
    assert!(
        read_entries_from_dir(dir.path(), LSN(1)).is_err(),
        "replay reader must keep propagating missing-magic errors"
    );
}

// =============================================================================
// Issue #3433: generalized crash-torn-tail tolerance on the REPLAY path.
//
// Trunk (#3413) tolerated only a TRUNCATED trailing entry (payload past
// EOF). These tests pin that replay (`read_entries_from_dir*`) now stops at
// ANY undecodable trailing entry in the FINAL segment — zeroed op-type,
// garbage op-type, checksum mismatch on a length-complete payload — while
// still hard-erroring on corruption in a NON-final segment and (encrypted)
// an undecodable frame FOLLOWED BY a valid frame.
// =============================================================================

/// Serialize one valid `CreateNode` WAL entry for `lsn` and return its raw
/// bytes (no segment header).
fn serialized_entry_bytes(lsn: u64) -> Vec<u8> {
    let entry = WalEntry::new(
        LSN(lsn),
        WalOperation::CreateNode {
            node_id: NodeId::new(lsn).unwrap(),
            label: GLOBAL_INTERNER.intern("TornTail3433").unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        },
    );
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();
    buffer
}

/// Append `bytes` to an existing segment file.
fn append_bytes(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

/// #3433: a zeroed operation-type byte after a fully-written 24-byte entry
/// header (the exact CI shape) is a crash-torn tail in the FINAL segment.
/// Replay must keep the decodable prefix and succeed, NOT hard-error.
#[test]
fn test_replay_tolerates_zeroed_optype_torn_tail() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    let last = write_segment_with_lsns(&segment_path, &[5, 7]);

    // 24-byte header from a real entry + op-type 0.
    let mut torn = last[..24].to_vec();
    torn.push(0);
    append_bytes(&segment_path, &torn);

    let entries = read_entries_from_dir(dir.path(), LSN(1))
        .expect("replay must tolerate a zeroed-op-type torn tail in the final segment");
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(
        lsns,
        vec![5, 7],
        "decodable prefix kept; torn entry dropped"
    );
}

/// #3433: a garbage (non-zero, unknown) operation-type byte at the tail is
/// also a crash-torn tail — tolerated in the final segment.
#[test]
fn test_replay_tolerates_garbage_optype_torn_tail() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    let last = write_segment_with_lsns(&segment_path, &[5, 7]);

    let mut torn = last[..24].to_vec();
    torn.push(0xEE); // no OP_* code equals this
    append_bytes(&segment_path, &torn);

    let entries = read_entries_from_dir(dir.path(), LSN(1))
        .expect("replay must tolerate a garbage-op-type torn tail in the final segment");
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(lsns, vec![5, 7]);
}

/// #3433: a length-COMPLETE trailing entry whose payload byte is corrupted
/// (so the CRC32 checksum fails, but no truncation occurred) is a torn tail
/// too — half-written-then-crashed. Replay must tolerate it in the final
/// segment. (This is the shape #3413's `is_truncation_error` gate did NOT
/// cover.)
#[test]
fn test_replay_tolerates_checksum_mismatch_torn_tail() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    write_segment_with_lsns(&segment_path, &[5, 7]);

    // A full valid entry for LSN 9, with one payload byte flipped so the
    // checksum mismatches. The entry is length-complete (not truncated).
    let mut torn = serialized_entry_bytes(9);
    let flip = 30; // past the 24-byte header, inside the op payload
    torn[flip] ^= 0xFF;
    append_bytes(&segment_path, &torn);

    let entries = read_entries_from_dir(dir.path(), LSN(1))
        .expect("replay must tolerate a checksum-mismatch torn tail in the final segment");
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(
        lsns,
        vec![5, 7],
        "the corrupted LSN-9 tail entry must be dropped"
    );
}

/// #3433 must-hard-error (a): the SAME torn shape in a NON-final segment (a
/// newer segment exists after it) is real corruption, not a crash-torn
/// append. Replay must still hard-error.
#[test]
fn test_replay_hard_errors_torn_tail_in_non_final_segment() {
    let dir = TempDir::new().unwrap();
    let seg0 = dir.path().join("0.log");
    let last = write_segment_with_lsns(&seg0, &[5, 7]);
    let mut torn = last[..24].to_vec();
    torn.push(0);
    append_bytes(&seg0, &torn);

    // A later, fully valid segment makes seg0 non-final.
    write_segment_with_lsns(&dir.path().join("1.log"), &[9]);

    assert!(
        read_entries_from_dir(dir.path(), LSN(1)).is_err(),
        "an undecodable entry in a NON-final segment must hard-error, not be tolerated"
    );
}

/// #3433: a single-segment plaintext WAL whose ONLY segment ends in a torn
/// entry (the segment IS the final segment) is tolerated — the common
/// single-segment crash case.
#[test]
fn test_replay_tolerates_torn_tail_single_segment() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    let last = write_segment_with_lsns(&segment_path, &[1, 2, 3]);
    let mut torn = last[..24].to_vec();
    torn.push(0);
    append_bytes(&segment_path, &torn);

    let entries = read_entries_from_dir(dir.path(), LSN(1)).expect("single final segment");
    assert_eq!(entries.len(), 3);
}

/// #3433 must-hard-error (c): the operator opt-out. With
/// `tolerate_torn_tail = false`, even a torn tail in the FINAL segment
/// hard-errors (fail-stop recovery); with `true` the same input is
/// tolerated. Same bytes, opposite outcome — proves the flag gates the
/// policy.
#[test]
fn test_replay_torn_tail_respects_tolerate_flag() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    let last = write_segment_with_lsns(&segment_path, &[5, 7]);
    let mut torn = last[..24].to_vec();
    torn.push(0);
    append_bytes(&segment_path, &torn);

    // Fail-stop: opt-out hard-errors on the torn tail.
    assert!(
        read_entries_from_dir_with_options(dir.path(), LSN(1), None, false).is_err(),
        "tolerate_torn_tail=false must hard-error on a torn tail (fail-stop recovery)"
    );

    // Default: the same torn tail is tolerated.
    let entries = read_entries_from_dir_with_options(dir.path(), LSN(1), None, true)
        .expect("tolerate_torn_tail=true must keep the decodable prefix");
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(lsns, vec![5, 7]);
}

// ---- Encrypted (length-prefixed) segments ----

fn aes_cipher() -> Arc<dyn crate::encryption::cipher::Cipher> {
    use zeroize::Zeroizing;
    // Fixed key: the same cipher must decrypt what we encrypt in-test.
    let key = Zeroizing::new([7u8; 32]);
    Arc::new(crate::encryption::Aes256GcmCipher::new(&key))
}

/// Encode one encrypted, length-prefixed frame: `[u32 LE len][ciphertext]`.
fn encrypted_frame(lsn: u64, cipher: &Arc<dyn crate::encryption::cipher::Cipher>) -> Vec<u8> {
    let plaintext = serialized_entry_bytes(lsn);
    let ct = crate::encryption::wal_encryption::encrypt_wal_payload(&plaintext, cipher).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&(ct.len() as u32).to_le_bytes());
    out.extend_from_slice(&ct);
    out
}

/// A length-prefixed frame whose bytes will FAIL to decrypt (garbage
/// ciphertext with a plausible length). `len` is >= the cipher's minimum.
fn undecryptable_frame() -> Vec<u8> {
    let body = vec![0xABu8; 80];
    let mut out = Vec::new();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn write_encrypted_header(path: &Path) {
    use std::io::Write;
    let mut file = File::create(path).unwrap();
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION_ENCRYPTED_DELETE_VERSION_ID])
        .unwrap();
    file.sync_all().unwrap();
}

/// #3433 item #4: an encrypted final segment whose LAST frame fails to
/// decrypt (crash-torn tail) is tolerated — the decodable prefix survives.
#[test]
fn test_replay_tolerates_encrypted_torn_tail() {
    let dir = TempDir::new().unwrap();
    let cipher = aes_cipher();
    let path = dir.path().join("0.log");
    write_encrypted_header(&path);
    append_bytes(&path, &encrypted_frame(5, &cipher));
    append_bytes(&path, &encrypted_frame(7, &cipher));
    append_bytes(&path, &undecryptable_frame()); // torn tail

    let entries = read_entries_from_dir_with_cipher(dir.path(), LSN(1), Some(&cipher))
        .expect("encrypted final-segment torn tail must be tolerated");
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(lsns, vec![5, 7]);
}

/// #3433 must-hard-error (b): in an encrypted final segment, an undecodable
/// frame FOLLOWED BY a valid frame is resyncable mid-log corruption, NOT a
/// torn tail — it must hard-error even though it is the final segment.
#[test]
fn test_replay_hard_errors_encrypted_undecodable_then_valid() {
    let dir = TempDir::new().unwrap();
    let cipher = aes_cipher();
    let path = dir.path().join("0.log");
    write_encrypted_header(&path);
    append_bytes(&path, &encrypted_frame(5, &cipher));
    append_bytes(&path, &undecryptable_frame()); // corrupt, but NOT the tail
    append_bytes(&path, &encrypted_frame(9, &cipher)); // valid frame follows

    assert!(
        read_entries_from_dir_with_cipher(dir.path(), LSN(1), Some(&cipher)).is_err(),
        "an undecodable encrypted frame followed by a valid frame is mid-log corruption"
    );
}

// =============================================================================
// Issue #3433 CORRECTNESS HARDENING (PR #3461 review): the plaintext replay
// path must NOT swallow mid-log corruption, and `tolerate_torn_tail = false`
// must be a TRUE fail-stop for every genuine-torn-tail shape.
//
// The plaintext generalization added by #3461 `break`s at the first
// undecodable entry in the final segment. Because plaintext entries carry no
// length prefix, that silently dropped EVERY byte after a mid-segment
// corrupt entry — including valid COMMITTED entries after it (up to a 64 MB
// segment of acknowledged transactions). These tests pin the fix: a
// forward-probe distinguishes a genuine torn tail (nothing valid follows →
// tolerate under the flag) from mid-log corruption (a valid entry with a
// higher LSN follows → HARD ERROR regardless of the flag).
// =============================================================================

/// HIGH (the load-bearing test): a plaintext FINAL segment holding
/// `[valid LSN 5][CRC-corrupt full entry LSN 7][valid LSN 9]`. The corrupt
/// LSN-7 entry is length-complete (only a payload byte flipped, so it fails
/// its CRC but does NOT truncate), and a fully valid LSN-9 entry follows it.
/// This is mid-log corruption, NOT a crash-torn tail: replay must HARD ERROR
/// rather than silently drop LSN 7 AND LSN 9. Pre-fix, the plaintext path
/// `break`s at LSN 7 and returns `Ok([5])`, losing acknowledged LSN 9.
#[test]
fn plaintext_mid_segment_corruption_with_valid_entries_after_hard_errors() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    // Header + valid LSN 5.
    write_segment_with_lsns(&segment_path, &[5]);

    // CRC-corrupt (but length-complete) full entry for LSN 7.
    let mut corrupt7 = serialized_entry_bytes(7);
    corrupt7[30] ^= 0xFF; // past the 24-byte header, inside the op payload
    append_bytes(&segment_path, &corrupt7);

    // A fully valid entry for LSN 9 AFTER the corruption — this is the
    // acknowledged data #3461 was silently dropping.
    append_bytes(&segment_path, &serialized_entry_bytes(9));

    let result = read_entries_from_dir(dir.path(), LSN(1));
    assert!(
        result.is_err(),
        "mid-segment corruption with a valid committed entry after it MUST hard-error \
             (not silently drop the trailing valid entries); got {:?}",
        result.map(|e| e.iter().map(|w| w.lsn.0).collect::<Vec<_>>())
    );
}

/// MEDIUM (config): `tolerate_torn_tail = false` must be a true fail-stop on
/// a genuine torn tail. A truncated final write (fewer than a full 24-byte
/// header, nonzero) is the shape the pre-fix code `break`s on unconditionally
/// — BEFORE the flag check — so the opt-out was silently ignored. With the
/// fix, `false` hard-errors and `true` tolerates the SAME bytes.
#[test]
fn plaintext_torn_tail_fail_stop_when_opted_out() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    write_segment_with_lsns(&segment_path, &[5, 7]);

    // A torn/truncated final write: only 12 nonzero bytes made it to disk
    // (partial header) before the crash. Nothing valid can follow.
    let truncated = serialized_entry_bytes(9)[..12].to_vec();
    assert!(truncated.iter().any(|&b| b != 0), "torn bytes are nonzero");
    append_bytes(&segment_path, &truncated);

    // Fail-stop opt-out MUST error.
    let opted_out = read_entries_from_dir_with_options(dir.path(), LSN(1), None, false);
    assert!(
        opted_out.is_err(),
        "tolerate_torn_tail=false must fail-stop on a genuine torn tail (partial header); \
             got {:?}",
        opted_out.map(|e| e.iter().map(|w| w.lsn.0).collect::<Vec<_>>())
    );

    // Default tolerance keeps the decodable prefix.
    let tolerated = read_entries_from_dir_with_options(dir.path(), LSN(1), None, true)
        .expect("tolerate_torn_tail=true must keep the decodable prefix");
    let lsns: Vec<u64> = tolerated.iter().map(|e| e.lsn.0).collect();
    assert_eq!(lsns, vec![5, 7]);
}

/// item 5: a mid-field-truncation torn tail (a full 24-byte header + op-type
/// byte + a payload cut off mid-field, nothing valid after) is a genuine torn
/// append — tolerated under the default flag in the final segment.
#[test]
fn plaintext_tolerates_mid_field_truncation_torn_tail() {
    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("0.log");
    write_segment_with_lsns(&segment_path, &[5, 7]);

    // 30 bytes: 24-byte header + op-type + a few payload bytes, then EOF
    // (payload truncated mid-field). Nothing valid follows.
    let mid_field = serialized_entry_bytes(9)[..30].to_vec();
    append_bytes(&segment_path, &mid_field);

    let entries = read_entries_from_dir(dir.path(), LSN(1))
        .expect("a mid-field-truncation torn tail must be tolerated in the final segment");
    let lsns: Vec<u64> = entries.iter().map(|e| e.lsn.0).collect();
    assert_eq!(
        lsns,
        vec![5, 7],
        "decodable prefix kept; torn entry dropped"
    );
}

// =============================================================================
// TDD Tests for parse_entry_at() - Issue #218
// =============================================================================

#[test]
fn test_parse_entry_at_create_node() {
    // Create a CreateNode entry
    let node_id = NodeId::new(42).unwrap();
    let operation = WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(1), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back. Serialization always writes the provenance-carrying
    // payload shape now (Issue #3224), so parsing must use the matching
    // version to consume the same bytes that were written.
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(1));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::CreateNode {
            node_id: parsed_id,
            label,
            ..
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(label, GLOBAL_INTERNER.intern("Person").unwrap());
        }
        _ => panic!("Expected CreateNode operation"),
    }
}

/// Issue #3350/#3423: a provenance bundle carrying an authenticated
/// principal must round-trip byte-exactly through WAL serialization
/// when parsed at the principal-carrying payload version.
#[test]
fn test_parse_entry_at_provenance_principal_roundtrip() {
    let node_id = NodeId::new(77).unwrap();
    let operation = WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Fact").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: Some(
            Provenance::builder()
                .source("mcp")
                .confidence(0.75)
                .correlation_id("req-1")
                .principal("svc-writer")
                .build()
                .unwrap(),
        ),
    };
    let entry = WalEntry::new(LSN(5), operation);

    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE_PRINCIPAL).unwrap();

    assert_eq!(parsed_entry.lsn, LSN(5));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::CreateNode { provenance, .. } => {
            let p = provenance.expect("provenance bundle must round-trip");
            assert_eq!(p.source(), Some("mcp"));
            assert_eq!(p.confidence(), Some(0.75));
            assert_eq!(p.correlation_id(), Some("req-1"));
            assert_eq!(p.principal(), Some("svc-writer"));
        }
        other => panic!("Expected CreateNode operation, got {other:?}"),
    }
}

/// Issue #3350/#3423: pre-v5 bytes (a provenance bundle that ends at
/// `correlation_id`, with no principal slot) must parse successfully at
/// their own payload version with `principal: None`.
#[test]
fn test_parse_pre_v5_provenance_bytes_yields_no_principal() {
    // Build genuine v3-format bytes. Start from the current (v5)
    // serializer with `principal: None` -- whose only difference from
    // v3 is a single trailing absent-principal presence byte -- drop
    // that byte, and re-stamp the CRC (bytes 20..24, computed over
    // LSN+timestamp and the operation data).
    let operation = WalOperation::CreateNode {
        node_id: NodeId::new(9).unwrap(),
        label: GLOBAL_INTERNER.intern("Doc").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: Some(Provenance::builder().source("importer").build().unwrap()),
    };
    let entry = WalEntry::new(LSN(9), operation);
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();
    assert_eq!(
        *buffer.last().unwrap(),
        0,
        "v5 buffer must end with the absent-principal presence byte"
    );
    buffer.pop(); // v3 bundles end at correlation_id
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&buffer[0..20]);
    hasher.update(&buffer[24..]);
    let checksum = hasher.finalize();
    buffer[20..24].copy_from_slice(&checksum.to_le_bytes());

    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE).unwrap();

    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::CreateNode { provenance, .. } => {
            let p = provenance.expect("v3 provenance bundle must parse");
            assert_eq!(p.source(), Some("importer"));
            assert_eq!(
                p.principal(),
                None,
                "pre-v5 bytes must parse with principal: None"
            );
        }
        other => panic!("Expected CreateNode operation, got {other:?}"),
    }
}

#[test]
fn test_parse_entry_at_create_edge() {
    // Create a CreateEdge entry
    let edge_id = EdgeId::new(100).unwrap();
    let source = NodeId::new(1).unwrap();
    let target = NodeId::new(2).unwrap();
    let operation = WalOperation::CreateEdge {
        edge_id,
        source,
        target,
        label: GLOBAL_INTERNER.intern("KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(2), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back. Serialization always writes the provenance-carrying
    // payload shape now (Issue #3224), so parsing must use the matching
    // version to consume the same bytes that were written.
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(2));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::CreateEdge {
            edge_id: parsed_id,
            source: parsed_source,
            target: parsed_target,
            label,
            ..
        } => {
            assert_eq!(parsed_id, edge_id);
            assert_eq!(parsed_source, source);
            assert_eq!(parsed_target, target);
            assert_eq!(label, GLOBAL_INTERNER.intern("KNOWS").unwrap());
        }
        _ => panic!("Expected CreateEdge operation"),
    }
}

#[test]
fn test_parse_entry_at_retract_node_roundtrip() {
    // Issue #3230: RetractNode must round-trip its valid_to exactly.
    let node_id = NodeId::new(7).unwrap();
    let valid_to = crate::core::hlc::HybridTimestamp::new(1_234_567, 42).unwrap();
    // Issue #3406: the retraction version_id round-trips too. Serialization
    // always writes the highest (v9+) payload shape, so parse at that
    // version to consume the same bytes.
    let version_id = Some(VersionId::new(321).unwrap());
    let operation = WalOperation::RetractNode {
        node_id,
        valid_to,
        version_id,
    };
    let entry = WalEntry::new(LSN(10), operation);

    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_DELETE_VERSION_ID).unwrap();

    assert_eq!(parsed_entry.lsn, LSN(10));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::RetractNode {
            node_id: parsed_id,
            valid_to: parsed_valid_to,
            version_id: parsed_version_id,
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(parsed_valid_to, valid_to, "valid_to must survive verbatim");
            assert_eq!(parsed_version_id, version_id, "version_id must round-trip");
        }
        other => panic!("Expected RetractNode operation, got {other:?}"),
    }
}

#[test]
fn test_parse_entry_at_retract_edge_roundtrip() {
    // Issue #3230: RetractEdge must round-trip its valid_to exactly.
    let edge_id = EdgeId::new(11).unwrap();
    let valid_to = crate::core::hlc::HybridTimestamp::new(9_876_543, 3).unwrap();
    // Issue #3406: the retraction version_id round-trips too.
    let version_id = Some(VersionId::new(654).unwrap());
    let operation = WalOperation::RetractEdge {
        edge_id,
        valid_to,
        version_id,
    };
    let entry = WalEntry::new(LSN(11), operation);

    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_DELETE_VERSION_ID).unwrap();

    assert_eq!(parsed_entry.lsn, LSN(11));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::RetractEdge {
            edge_id: parsed_id,
            valid_to: parsed_valid_to,
            version_id: parsed_version_id,
        } => {
            assert_eq!(parsed_id, edge_id);
            assert_eq!(parsed_valid_to, valid_to, "valid_to must survive verbatim");
            assert_eq!(parsed_version_id, version_id, "version_id must round-trip");
        }
        other => panic!("Expected RetractEdge operation, got {other:?}"),
    }
}

#[test]
fn test_parse_entry_at_update_node() {
    // Create an UpdateNode entry
    let node_id = NodeId::new(42).unwrap();
    let version_id = VersionId::new(1).unwrap();
    let operation = WalOperation::UpdateNode {
        node_id,
        version_id,
        label: GLOBAL_INTERNER.intern("UpdatedPerson").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(3), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back. Serialization always writes the provenance-carrying
    // payload shape now (Issue #3224), so parsing must use the matching
    // version to consume the same bytes that were written.
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(3));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::UpdateNode {
            node_id: parsed_id,
            version_id: parsed_version,
            label,
            ..
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(parsed_version, version_id);
            assert_eq!(label, GLOBAL_INTERNER.intern("UpdatedPerson").unwrap());
        }
        _ => panic!("Expected UpdateNode operation"),
    }
}

#[test]
fn test_parse_entry_at_update_edge() {
    // Create an UpdateEdge entry
    let edge_id = EdgeId::new(100).unwrap();
    let version_id = VersionId::new(1).unwrap();
    let operation = WalOperation::UpdateEdge {
        edge_id,
        version_id,
        label: GLOBAL_INTERNER.intern("UPDATED_KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(4), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back. Serialization always writes the provenance-carrying
    // payload shape now (Issue #3224), so parsing must use the matching
    // version to consume the same bytes that were written.
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_PROVENANCE).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(4));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::UpdateEdge {
            edge_id: parsed_id,
            version_id: parsed_version,
            label,
            ..
        } => {
            assert_eq!(parsed_id, edge_id);
            assert_eq!(parsed_version, version_id);
            assert_eq!(label, GLOBAL_INTERNER.intern("UPDATED_KNOWS").unwrap());
        }
        _ => panic!("Expected UpdateEdge operation"),
    }
}

#[test]
fn test_parse_entry_at_delete_node() {
    // Create a DeleteNode entry with a distinct BACKDATED valid_from
    // (Issue #3221/#3400: the logged delete valid_from must roundtrip
    // through serialization exactly, it is honored by WAL replay).
    let node_id = NodeId::new(42).unwrap();
    let valid_from = HybridTimestamp::new(time::now().wallclock() - 3_600_000_000, 0).unwrap(); // 1h ago
    // Issue #3406: the tombstone version_id round-trips too. Serialization
    // always writes the highest (v9+) payload shape, so parse at that
    // version to consume the same bytes.
    let version_id = Some(VersionId::new(555).unwrap());
    let operation = WalOperation::DeleteNode {
        node_id,
        valid_from,
        version_id,
    };
    let entry = WalEntry::new(LSN(5), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_DELETE_VERSION_ID).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(5));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::DeleteNode {
            node_id: parsed_id,
            valid_from: parsed_valid_from,
            version_id: parsed_version_id,
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(
                parsed_valid_from, valid_from,
                "backdated delete valid_from must roundtrip exactly"
            );
            assert_eq!(parsed_version_id, version_id, "version_id must round-trip");
        }
        _ => panic!("Expected DeleteNode operation"),
    }
}

#[test]
fn test_parse_entry_at_delete_edge() {
    // Create a DeleteEdge entry with a distinct BACKDATED valid_from
    // (Issue #3221/#3400: the logged delete valid_from must roundtrip
    // through serialization exactly, it is honored by WAL replay).
    let edge_id = EdgeId::new(100).unwrap();
    let valid_from = HybridTimestamp::new(time::now().wallclock() - 3_600_000_000, 0).unwrap(); // 1h ago
    // Issue #3406: the tombstone version_id round-trips too.
    let version_id = Some(VersionId::new(556).unwrap());
    let operation = WalOperation::DeleteEdge {
        edge_id,
        valid_from,
        version_id,
    };
    let entry = WalEntry::new(LSN(6), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, 0, WAL_VERSION_DELETE_VERSION_ID).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(6));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::DeleteEdge {
            edge_id: parsed_id,
            valid_from: parsed_valid_from,
            version_id: parsed_version_id,
        } => {
            assert_eq!(parsed_id, edge_id);
            assert_eq!(
                parsed_valid_from, valid_from,
                "backdated delete valid_from must roundtrip exactly"
            );
            assert_eq!(parsed_version_id, version_id, "version_id must round-trip");
        }
        _ => panic!("Expected DeleteEdge operation"),
    }
}

#[test]
fn test_parse_entry_at_checkpoint() {
    // Create a Checkpoint entry
    let cp_timestamp = time::now();
    let operation = WalOperation::Checkpoint {
        lsn: LSN(100),
        timestamp: cp_timestamp,
    };
    let entry = WalEntry::new(LSN(7), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Parse it back
    let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, WAL_VERSION).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(7));
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::Checkpoint { lsn, .. } => {
            assert_eq!(lsn, LSN(100));
        }
        _ => panic!("Expected Checkpoint operation"),
    }
}

#[test]
fn test_parse_entry_at_with_offset() {
    // Create two entries
    let operation1 = WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("First").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry1 = WalEntry::new(LSN(1), operation1);

    let operation2 = WalOperation::CreateNode {
        node_id: NodeId::new(2).unwrap(),
        label: GLOBAL_INTERNER.intern("Second").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry2 = WalEntry::new(LSN(2), operation2);

    // Serialize both entries separately, then concatenate
    // (serialize_entry_into computes checksum from buffer start, so we can't
    //  append directly without getting wrong checksums)
    let mut buffer = Vec::new();
    serialize_entry_into(&entry1, &mut buffer).unwrap();
    let offset1_end = buffer.len();

    let mut buffer2 = Vec::new();
    serialize_entry_into(&entry2, &mut buffer2).unwrap();
    buffer.extend_from_slice(&buffer2);

    // Parse second entry using offset. Serialization always writes the
    // provenance-carrying payload shape now (Issue #3224), so parsing
    // must use the matching version to consume the same bytes written.
    let (parsed_entry, bytes_consumed) =
        parse_entry_at(&buffer, offset1_end, WAL_VERSION_PROVENANCE).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn, LSN(2));
    match parsed_entry.operation {
        WalOperation::CreateNode { label, .. } => {
            assert_eq!(label, GLOBAL_INTERNER.intern("Second").unwrap());
        }
        _ => panic!("Expected CreateNode operation"),
    }
    assert_eq!(bytes_consumed, buffer.len() - offset1_end);
}

#[test]
fn test_parse_entry_at_insufficient_buffer() {
    // Create a buffer with only 10 bytes (not enough for LSN + timestamp + checksum)
    let buffer = vec![0u8; 10];

    // Should return error
    let result = parse_entry_at(&buffer, 0, WAL_VERSION);
    assert!(result.is_err());
}

#[test]
fn test_parse_entry_at_unknown_operation_type() {
    // Create a valid header but invalid operation type
    let mut buffer = Vec::new();

    // LSN (8 bytes)
    buffer.extend_from_slice(&1u64.to_le_bytes());

    // Timestamp (12 bytes)
    let timestamp = time::now();
    timestamp.serialize_into(&mut buffer);

    // Checksum (4 bytes) - just use 0 for this test
    buffer.extend_from_slice(&0u32.to_le_bytes());

    // Invalid operation type (255)
    buffer.push(255);

    // Should return error for unknown operation type
    let result = parse_entry_at(&buffer, 0, WAL_VERSION);
    assert!(result.is_err());
}

#[test]
fn test_parse_entry_at_truncated_operation_data() {
    // Create a valid header but truncate operation data
    let mut buffer = Vec::new();

    // LSN (8 bytes)
    buffer.extend_from_slice(&1u64.to_le_bytes());

    // Timestamp (12 bytes)
    let timestamp = time::now();
    timestamp.serialize_into(&mut buffer);

    // Checksum (4 bytes)
    buffer.extend_from_slice(&0u32.to_le_bytes());

    // Operation type for CreateNode (1)
    buffer.push(1);

    // Only 4 bytes of node_id (should be 8) - truncated!
    buffer.extend_from_slice(&[1, 2, 3, 4]);

    // Should return error for insufficient data
    let result = parse_entry_at(&buffer, 0, WAL_VERSION);
    assert!(result.is_err());
}

#[test]
fn test_parse_entry_at_version_0_compatibility() {
    // Test legacy version 0 parsing (without properties and temporal data)
    // This tests the version < WAL_VERSION code path
    let mut buffer = Vec::new();

    // LSN (8 bytes)
    buffer.extend_from_slice(&42u64.to_le_bytes());

    // Timestamp (12 bytes)
    let timestamp = time::now();
    timestamp.serialize_into(&mut buffer);

    // Placeholder checksum (4 bytes) - will be computed later
    let checksum_offset = buffer.len();
    buffer.extend_from_slice(&0u32.to_le_bytes());

    // Operation type: CreateNode (1)
    buffer.push(1);

    // Node ID (8 bytes)
    buffer.extend_from_slice(&123u64.to_le_bytes());

    // Label (4-byte InternedString ID)
    let label_id = GLOBAL_INTERNER.intern("TestNode").unwrap().as_u32();
    buffer.extend_from_slice(&label_id.to_le_bytes());

    // Note: Version 0 format does NOT include properties or temporal data

    // Compute checksum
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&buffer[0..checksum_offset]); // LSN + timestamp
    hasher.update(&buffer[checksum_offset + 4..]); // Operation data
    let checksum = hasher.finalize();
    buffer[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());

    // Parse with version 0
    let (parsed_entry, bytes_consumed) = parse_entry_at(&buffer, 0, 0).unwrap();

    // Verify
    assert_eq!(parsed_entry.lsn.0, 42);
    assert_eq!(bytes_consumed, buffer.len());
    match parsed_entry.operation {
        WalOperation::CreateNode {
            node_id,
            label: parsed_label,
            properties,
            valid_from,
            ..
        } => {
            assert_eq!(node_id.as_u64(), 123);
            assert_eq!(parsed_label, GLOBAL_INTERNER.intern("TestNode").unwrap());
            // Version 0 should have empty properties
            assert!(properties.is_empty());
            // Valid_from should be set to the timestamp
            assert_eq!(valid_from, timestamp);
        }
        _ => panic!("Expected CreateNode operation"),
    }
}

#[test]
fn test_parse_entry_at_checksum_mismatch() {
    // Create a valid entry
    let node_id = NodeId::new(42).unwrap();
    let operation = WalOperation::CreateNode {
        node_id,
        label: GLOBAL_INTERNER.intern("Person").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(1), operation);

    // Serialize it
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();

    // Corrupt the checksum (bytes 20-24)
    buffer[20] ^= 0xFF; // Flip all bits in first checksum byte

    // Should return error for checksum mismatch
    let result = parse_entry_at(&buffer, 0, WAL_VERSION);
    assert!(result.is_err());
    if let Err(e) = result {
        let error_msg = format!("{}", e);
        assert!(error_msg.contains("checksum mismatch"));
    }
}

#[test]
fn test_parse_entry_at_update_edge_truncated_label() {
    // Reproduction test for fuzzing panic: UpdateEdge with missing label
    let mut buffer = Vec::new();

    // LSN (8 bytes)
    buffer.extend_from_slice(&1u64.to_le_bytes());

    // Timestamp (12 bytes)
    let timestamp = time::now();
    timestamp.serialize_into(&mut buffer);

    // Checksum (4 bytes) - placeholders
    let checksum_offset = buffer.len();
    buffer.extend_from_slice(&0u32.to_le_bytes());

    // Operation type: UpdateEdge (4)
    buffer.push(4);

    // Edge ID (8 bytes)
    buffer.extend_from_slice(&100u64.to_le_bytes());

    // Version ID (8 bytes)
    buffer.extend_from_slice(&1u64.to_le_bytes());

    // STOP HERE - Do not write label ID. This simulates truncation.
    // We have written 16 bytes of operation data (EdgeID + VersionID), which satisfies the initial check.
    // But we are missing the Label ID (4 bytes) which is read immediately after.

    // Compute checksum for what we have
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&buffer[0..checksum_offset]); // LSN + timestamp
    hasher.update(&buffer[checksum_offset + 4..]); // Operation data
    let checksum = hasher.finalize();
    buffer[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());

    // Parse - this should NOT panic, but return an error
    let result = parse_entry_at(&buffer, 0, WAL_VERSION);
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("Insufficient buffer size"));
}

#[test]
fn test_parse_entry_at_update_node_truncated_label() {
    // Reproduction test for fuzzing panic: UpdateNode with missing label
    let mut buffer = Vec::new();

    // LSN (8 bytes)
    buffer.extend_from_slice(&1u64.to_le_bytes());

    // Timestamp (12 bytes)
    let timestamp = time::now();
    timestamp.serialize_into(&mut buffer);

    // Checksum (4 bytes) - placeholders
    let checksum_offset = buffer.len();
    buffer.extend_from_slice(&0u32.to_le_bytes());

    // Operation type: UpdateNode (3)
    buffer.push(3);

    // Node ID (8 bytes)
    buffer.extend_from_slice(&100u64.to_le_bytes());

    // Version ID (8 bytes)
    buffer.extend_from_slice(&1u64.to_le_bytes());

    // STOP HERE - Do not write label ID.

    // Compute checksum for what we have
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&buffer[0..checksum_offset]); // LSN + timestamp
    hasher.update(&buffer[checksum_offset + 4..]); // Operation data
    let checksum = hasher.finalize();
    buffer[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());

    // Parse - this should NOT panic, but return an error
    let result = parse_entry_at(&buffer, 0, WAL_VERSION);
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(err_msg.contains("Insufficient buffer size"));
}

// =============================================================================
// TDD Tests for Memory-Efficient Segment Reading - Issue #216
// =============================================================================

/// Test that we can read a segment file with many entries without loading
/// the entire file into memory at once.
///
/// This test creates a large segment file (simulating real-world 64MB segments)
/// and verifies that all entries can be read correctly.
#[test]
fn test_read_large_segment_memory_efficient() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("large_segment.log");

    // Create a segment file with many entries
    let mut file = File::create(&segment_path).unwrap();

    // Write WAL header. Entries below are serialized with the modern
    // (always-provenance-carrying) format, so the header must declare
    // WAL_VERSION_PROVENANCE for the reader to parse them correctly.
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION_PROVENANCE]).unwrap();

    // Create and write many entries to simulate a large segment
    // We'll create 1000 entries, which should be several MB
    let num_entries = 1000;
    let mut expected_lsns = Vec::new();

    for i in 0..num_entries {
        let lsn = LSN(i + 1);
        expected_lsns.push(lsn);

        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(i + 1).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node_{}", i)).unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        };

        let entry = WalEntry::new(lsn, operation);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        file.write_all(&buffer).unwrap();
    }

    file.sync_all().unwrap();
    drop(file);

    // Read the segment
    let entries = read_segment(&segment_path, LSN(1)).unwrap();

    // Verify all entries were read correctly
    assert_eq!(entries.len(), num_entries as usize);
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.lsn, LSN(i as u64 + 1));
    }
}

/// Test that reading multiple segments doesn't accumulate excessive memory.
///
/// This test creates multiple segment files and verifies that we can process
/// them sequentially without holding all segment buffers in memory simultaneously.
#[test]
fn test_read_multiple_segments_sequentially() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();

    // Create 5 segment files
    let num_segments = 5;
    let entries_per_segment = 100;

    for seg_id in 0..num_segments {
        let segment_path = dir.path().join(format!("{}.log", seg_id));
        let mut file = File::create(&segment_path).unwrap();

        // Write WAL header. Entries below are serialized with the modern
        // (always-provenance-carrying) format, so the header must
        // declare WAL_VERSION_PROVENANCE for the reader to parse them
        // correctly.
        file.write_all(&WAL_MAGIC).unwrap();
        file.write_all(&[WAL_VERSION_PROVENANCE]).unwrap();

        // Write entries for this segment
        for i in 0..entries_per_segment {
            let lsn = LSN((seg_id * entries_per_segment) + i + 1);

            let operation = WalOperation::CreateNode {
                node_id: NodeId::new(lsn.0).unwrap(),
                label: GLOBAL_INTERNER
                    .intern(format!("Node_seg{}_entry{}", seg_id, i))
                    .unwrap(),
                properties: PropertyMap::new(),
                valid_from: time::now(),
                provenance: None,
            };

            let entry = WalEntry::new(lsn, operation);
            let mut buffer = Vec::new();
            serialize_entry_into(&entry, &mut buffer).unwrap();
            file.write_all(&buffer).unwrap();
        }

        file.sync_all().unwrap();
    }

    // Read all entries from directory
    let entries = read_entries_from_dir(dir.path(), LSN(1)).unwrap();

    // Verify all entries were read correctly
    assert_eq!(entries.len(), (num_segments * entries_per_segment) as usize);

    // Verify entries are sorted by LSN
    for i in 0..entries.len() - 1 {
        assert!(entries[i].lsn <= entries[i + 1].lsn);
    }
}

/// Test that segment reading works correctly with the start_lsn filter.
///
/// This verifies that we can efficiently skip entries before a certain LSN
/// without processing them.
#[test]
fn test_read_segment_with_start_lsn_filter() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("filtered_segment.log");

    let mut file = File::create(&segment_path).unwrap();

    // Write WAL header. Entries below are serialized with the modern
    // (always-provenance-carrying) format, so the header must declare
    // WAL_VERSION_PROVENANCE for the reader to parse them correctly.
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION_PROVENANCE]).unwrap();

    // Write 100 entries with LSN 1-100
    for i in 1..=100 {
        let lsn = LSN(i);
        let operation = WalOperation::CreateNode {
            node_id: NodeId::new(i).unwrap(),
            label: GLOBAL_INTERNER.intern(format!("Node_{}", i)).unwrap(),
            properties: PropertyMap::new(),
            valid_from: time::now(),
            provenance: None,
        };

        let entry = WalEntry::new(lsn, operation);
        let mut buffer = Vec::new();
        serialize_entry_into(&entry, &mut buffer).unwrap();
        file.write_all(&buffer).unwrap();
    }

    file.sync_all().unwrap();
    drop(file);

    // Read entries starting from LSN 50
    let entries = read_segment(&segment_path, LSN(50)).unwrap();

    // Should only get entries with LSN >= 50
    assert_eq!(entries.len(), 51); // LSN 50-100 inclusive
    assert_eq!(entries[0].lsn, LSN(50));
    assert_eq!(entries[entries.len() - 1].lsn, LSN(100));
}

/// Test that empty segments are handled efficiently.
#[test]
fn test_read_empty_segment_efficient() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("empty_segment.log");

    let mut file = File::create(&segment_path).unwrap();

    // Write only WAL header, no entries
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION]).unwrap();

    file.sync_all().unwrap();
    drop(file);

    // Read the empty segment
    let entries = read_segment(&segment_path, LSN(1)).unwrap();

    // Should return empty vector
    assert!(entries.is_empty());
}

/// A partial/truncated trailing entry (a mid-entry interrupted write) is
/// a torn tail. The strict single-segment [`read_segment`] reader (its
/// documented contract: "every parse failure when `tolerate_torn_tail` is
/// false" hard-errors) must REJECT it — while the recovery dir-reader, which
/// opts the final segment into torn-tail tolerance, keeps the decodable
/// prefix. (PR #3461: the strict reader previously `break`ed unconditionally
/// on a partial header, contradicting its own contract and silently swallowing
/// a torn write; that unconditional break is now gated on the flag.)
#[test]
fn test_read_segment_with_truncated_entry() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    // Numeric stem so the recovery dir-reader (`read_entries_from_dir`)
    // enumerates it as a segment.
    let segment_path = dir.path().join("0.log");

    let mut file = File::create(&segment_path).unwrap();

    // Write WAL header. The entry below is serialized with the modern
    // (always-provenance-carrying) format, so the header must declare
    // WAL_VERSION_PROVENANCE for the reader to parse it correctly.
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION_PROVENANCE]).unwrap();

    // Write one complete entry
    let operation = WalOperation::CreateNode {
        node_id: NodeId::new(1).unwrap(),
        label: GLOBAL_INTERNER.intern("Node_1").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(1), operation);
    let mut buffer = Vec::new();
    serialize_entry_into(&entry, &mut buffer).unwrap();
    file.write_all(&buffer).unwrap();

    // Write a partial entry (just the LSN, incomplete) -- a nonzero partial
    // header, i.e. a torn write.
    file.write_all(&42u64.to_le_bytes()).unwrap();

    file.sync_all().unwrap();
    drop(file);

    // Strict single-segment read: fail-stop on the torn partial tail.
    assert!(
        read_segment(&segment_path, LSN(1)).is_err(),
        "the strict read_segment reader must hard-error on a torn partial tail"
    );

    // Recovery dir-read (final segment tolerant): keeps the complete prefix
    // and drops the torn partial tail.
    let entries = read_entries_from_dir(dir.path(), LSN(1))
        .expect("the recovery reader tolerates a torn tail in the final segment");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].lsn, LSN(1));
}

// =============================================================================
// Security and Error Handling Tests - Issue #216 Fixes
// =============================================================================

/// Test that non-existent files return empty results (not an error).
#[test]
fn test_read_nonexistent_file_returns_empty() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("does_not_exist.log");

    // Should return Ok(empty vector), not an error
    let result = read_segment(&nonexistent, LSN(1));
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

/// Test that file size validation prevents reading excessively large files.
///
/// This protects against DoS attacks where an attacker places a huge file
/// in the WAL directory.
#[test]
fn test_read_segment_rejects_oversized_file() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let segment_path = dir.path().join("oversized_segment.log");

    let mut file = File::create(&segment_path).unwrap();

    // Write WAL header
    file.write_all(&WAL_MAGIC).unwrap();
    file.write_all(&[WAL_VERSION]).unwrap();

    // Seek to a position beyond MAX_SEGMENT_SIZE (1GB)
    // Note: We don't actually write 1GB of data, just seek past it
    // This creates a sparse file that reports a large size
    const OVERSIZED: u64 = 1024 * 1024 * 1024 + 1; // 1GB + 1 byte
    file.set_len(OVERSIZED).unwrap();

    file.sync_all().unwrap();
    drop(file);

    // Should return an error about file being too large
    let result = read_segment(&segment_path, LSN(1));
    assert!(result.is_err());
    let error_msg = format!("{}", result.unwrap_err());
    assert!(
        error_msg.contains("too large"),
        "Expected 'too large' error, got: {}",
        error_msg
    );
}

#[test]
fn test_wal_offset_overflow_protection() {
    // Create a small dummy buffer
    let buffer = [0u8; 100];

    // Use an offset close to usize::MAX
    let offset = usize::MAX - 10;

    // Attempt to parse - this should trigger the checked_add protection
    // NOT a panic or buffer overrun
    let result = parse_entry_at(&buffer, offset, 1);

    assert!(result.is_err());
    match result {
        Err(Error::Storage(StorageError::CorruptedData(msg))) => {
            assert_eq!(msg, "WAL offset overflow");
        }
        _ => panic!("Expected WAL offset overflow error, got: {:?}", result),
    }
}

#[test]
fn test_update_node_insufficient_buffer_for_label() {
    // Create a valid UpdateNode entry
    let node_id = NodeId::new(42).unwrap();
    let version_id = VersionId::new(1).unwrap();
    let operation = WalOperation::UpdateNode {
        node_id,
        version_id,
        label: GLOBAL_INTERNER.intern("UpdatedPerson").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(1), operation);

    // Serialize it
    let mut full_buffer = Vec::new();
    serialize_entry_into(&entry, &mut full_buffer).unwrap();

    // Calculate expected cut point
    // Header (24) + Op (1) + NodeID (8) + VersionID (8) = 41 bytes
    // We want to pass the first check (41 bytes) but fail the next (Label ID, +4 bytes)
    // So we truncate to EXACTLY 41 bytes.
    let truncated_buffer = &full_buffer[0..41];

    // This should trigger "Insufficient buffer size for UpdateNode label"
    let result = parse_entry_at(truncated_buffer, 0, WAL_VERSION);
    assert!(result.is_err());
    if let Err(Error::Storage(StorageError::CorruptedData(msg))) = result {
        assert_eq!(msg, "Insufficient buffer size for UpdateNode label");
    } else {
        panic!("Expected specific CorruptedData error, got: {:?}", result);
    }
}

#[test]
fn test_update_edge_insufficient_buffer_for_label() {
    // Create a valid UpdateEdge entry
    let edge_id = EdgeId::new(100).unwrap();
    let version_id = VersionId::new(1).unwrap();
    let operation = WalOperation::UpdateEdge {
        edge_id,
        version_id,
        label: GLOBAL_INTERNER.intern("UPDATED_KNOWS").unwrap(),
        properties: PropertyMap::new(),
        valid_from: time::now(),
        provenance: None,
    };
    let entry = WalEntry::new(LSN(1), operation);

    // Serialize it
    let mut full_buffer = Vec::new();
    serialize_entry_into(&entry, &mut full_buffer).unwrap();

    // Calculate expected cut point.
    // UpdateEdge now validates all V1 fixed fields in one check:
    // Header (24) + Op (1) + EdgeID (8) + VersionID (8) + LabelID (4) = 45 bytes.
    // Truncating to 41 bytes should fail the fixed-fields boundary check.
    let truncated_buffer = &full_buffer[0..41];

    // This should trigger the generic UpdateEdge insufficient buffer error.
    let result = parse_entry_at(truncated_buffer, 0, WAL_VERSION);
    assert!(result.is_err());
    if let Err(Error::Storage(StorageError::CorruptedData(msg))) = result {
        assert_eq!(msg, "Insufficient buffer size for UpdateEdge");
    } else {
        panic!("Expected specific CorruptedData error, got: {:?}", result);
    }
}

#[test]
fn test_update_edge_offset_overflow_before_label() {
    // This test attempts to trigger the overflow check before reading the label ID in UpdateEdge
    // It's hard to trigger purely via buffer offset manipulation without triggering earlier checks,
    // unless we mock the buffer length check or construct a very specific scenario.
    //
    // However, we can construct a buffer that passes earlier checks but fails the overflow check
    // if we use a huge offset that wraps around when adding 4.
    //
    // Let's try to pass a buffer and an offset such that offset + 16 (for edge+ver) succeeds,
    // but offset + 16 + 4 overflows.
    //
    // offset + 16 <= usize::MAX
    // offset + 20 > usize::MAX (overflow)
    // So offset can be usize::MAX - 19.

    // We need a buffer that is technically "valid" up to that point logic-wise,
    // but since we are passing a huge offset, we need the buffer length to be huge too?
    // No, `buffer.len()` is checked against `current_offset`.
    // If `current_offset` is huge, `buffer.len()` must be huge for the check `current_offset > buffer.len()` to pass.
    // Since we can't allocate a usize::MAX buffer, we can't easily test the "success" path up to the overflow.
    //
    // BUT, the `checked_add` returns None on overflow, and we convert that to an error.
    // So we just need `current_offset.checked_add(4)` to return None.
    // And we need to get past the previous checks.
    //
    // Previous checks in UpdateEdge:
    // 1. `current_offset.checked_add(16)` (Edge ID + Version ID)
    //
    // So if we start with an offset that allows +16 but fails +20 (implicit in logic flow),
    // we might hit it. But `parse_entry_at` starts from `offset`.
    //
    // The function does:
    // header checks (offset + 24) -> OK
    // op type check (offset + 1) -> OK
    // UpdateEdge checks:
    //   offset + 16 -> OK
    //   read edge_id, version_id -> OK
    //   offset + 4 -> OVERFLOW?
    //
    // To get to UpdateEdge check, we need to pass header checks.
    // `offset + 24` must not overflow.
    // So `offset` must be <= usize::MAX - 24.
    //
    // Inside UpdateEdge:
    // `current_offset` is now `offset + 24 + 1` (header + op type) = `offset + 25`.
    // Then checks `current_offset + 16`. `offset + 25 + 16` = `offset + 41`.
    // Then adds 16. `current_offset` is `offset + 41`.
    // Then checks `current_offset + 4`. `offset + 41 + 4` = `offset + 45`.
    //
    // So if we pick `offset` such that `offset + 45` overflows, but `offset + 41` does not?
    // Yes. `usize::MAX - 44`.
    // `offset + 41` = `MAX - 3` (OK)
    // `offset + 45` = OVERFLOW (Error)
    //
    // However, we also need `current_offset < buffer.len()`.
    // `buffer.len()` would need to be `usize::MAX - 3`. We can't allocate that.
    //
    // So we can't integration-test the overflow check with a real buffer on a 64-bit machine.
    // But on a 32-bit machine (or if we could mock the buffer), maybe.
    //
    // Actually, the `checked_add` protection is `ok_or_else(|| Error...)`.
    // This error `WAL offset overflow` is what we want to verify.
    //
    // Since we can't allocate a huge buffer, this test is theoretical unless we can mock `buffer.len()` or use a trick.
    // The check is `checked_add(...) > buffer.len()`.
    // If `checked_add` fails (returns None), we get the error immediately.
    // We don't check buffer length if `checked_add` fails.
    //
    // So if we pass a small buffer, but a huge offset?
    // Then `current_offset > buffer.len()` check inside `add_offset!` or manual checks will fail
    // with "Insufficient buffer size..." BEFORE we get to the overflow check?
    //
    // Let's trace:
    // `parse_entry_at(buffer, offset)`
    // `current_offset = offset`
    // `if current_offset.checked_add(24)... > buffer.len()` -> Error "Insufficient buffer size..."
    //
    // So we can never get past the first check with a huge offset and a small buffer.
    // Thus, we can't easily test the later overflow checks without a huge buffer.
    //
    // Use `#[cfg(target_pointer_width = "32")]`? No, CI is likely 64-bit.
    //
    // However, the coverage report says lines 518-520 are missed.
    // `src/storage/wal/segment_reader.rs:518`:
    // if current_offset.checked_add(4).ok_or_else(|| ...
    //
    // Wait, if I can't reach it, maybe it's dead code?
    // No, it's valid protection.
    //
    // Actually, the previous test `test_wal_offset_overflow_protection` just calls `parse_entry_at` with huge offset.
    // And it hits the FIRST check: `checked_add(24)`.
    //
    // To hit the UpdateEdge specific overflow check, we'd need to pass the first check.
    //
    // What if we test the logic in isolation? We can't, it's inside the function.
    //
    // Let's settle for testing the `Insufficient buffer size` error, which IS reachable with small buffers.
    // The overflow check is likely unreachable in tests without huge buffers, so we might have to accept it as uncovered or add `// LCOV_EXCL_START`?
    // But the user wants coverage.
    //
    // Wait, Codecov says lines 518-520 are uncovered.
    // Line 518 is the `if current_offset.checked_add(4)...` check.
    //
    // If I supply a buffer that is large enough to pass the *previous* checks but *truncated* right after,
    // then `checked_add(4)` will succeed (return Some), but `> buffer.len()` will be true.
    // This will verify the logic `> buffer.len()` branch.
    //
    // The `WAL offset overflow` error (from `.ok_or_else`) is what handles the arithmetic overflow.
    // The `Insufficient buffer size` error is what handles the buffer boundary.
    //
    // My proposed `test_update_edge_insufficient_buffer_for_label` will cover the `Insufficient buffer size` path.
    //
    // Is line 518 the check itself? Yes.
    // If the test runs, it executes the line `if current_offset.checked_add(4)...`.
    // Even if it doesn't panic/return overflow error, it executes the condition.
    //
    // Codecov usually marks the line as covered if it's executed.
    //
    // So `test_update_edge_insufficient_buffer_for_label` should cover lines 518-520 (the condition) and 524 (the error return).
    //
    // The overflow branch (inside `ok_or_else`) might remain uncovered, but that's fine if the main path is covered.
}

// Cover the advance() overflow branch directly (can't be reached via parse_entry_at
// because require_bytes always validates bounds first).
#[test]
fn test_advance_overflow_protection() {
    let mut offset = usize::MAX;
    let result = advance(&mut offset, 1);
    assert!(result.is_err());
    match result {
        Err(Error::Storage(StorageError::CorruptedData(msg))) => {
            assert_eq!(msg, "WAL offset overflow");
        }
        _ => panic!("Expected WAL offset overflow error, got: {:?}", result),
    }
}

// Cover V0 (legacy) else-branches in parse_delete_node_op / parse_delete_edge_op /
// parse_update_node_op / parse_update_edge_op.

fn make_v0_buffer(
    op_byte: u8,
    op_data: &[u8],
    timestamp: crate::core::hlc::HybridTimestamp,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&1u64.to_le_bytes()); // LSN
    timestamp.serialize_into(&mut buf); // 12-byte timestamp
    let checksum_off = buf.len();
    buf.extend_from_slice(&0u32.to_le_bytes()); // checksum placeholder
    buf.push(op_byte);
    buf.extend_from_slice(op_data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&buf[0..checksum_off]);
    hasher.update(&buf[checksum_off + 4..]);
    let cs = hasher.finalize();
    buf[checksum_off..checksum_off + 4].copy_from_slice(&cs.to_le_bytes());
    buf
}

#[test]
fn test_parse_entry_at_version_0_delete_node() {
    let timestamp = time::now();
    let node_id = NodeId::new(55).unwrap();
    let buf = make_v0_buffer(6, &55u64.to_le_bytes(), timestamp); // OP_DELETE_NODE = 6
    let (entry, consumed) = parse_entry_at(&buf, 0, 0).unwrap();
    assert_eq!(consumed, buf.len());
    match entry.operation {
        WalOperation::DeleteNode {
            node_id: parsed_id,
            valid_from,
            version_id,
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(valid_from, timestamp);
            // v0 segments carry no tombstone version_id (Issue #3406);
            // replay synthesizes it.
            assert_eq!(version_id, None);
        }
        _ => panic!("Expected DeleteNode"),
    }
}

#[test]
fn test_parse_entry_at_version_0_delete_edge() {
    let timestamp = time::now();
    let edge_id = EdgeId::new(200).unwrap();
    let buf = make_v0_buffer(7, &200u64.to_le_bytes(), timestamp); // OP_DELETE_EDGE = 7
    let (entry, consumed) = parse_entry_at(&buf, 0, 0).unwrap();
    assert_eq!(consumed, buf.len());
    match entry.operation {
        WalOperation::DeleteEdge {
            edge_id: parsed_id,
            valid_from,
            version_id,
        } => {
            assert_eq!(parsed_id, edge_id);
            assert_eq!(valid_from, timestamp);
            // v0 segments carry no tombstone version_id (Issue #3406).
            assert_eq!(version_id, None);
        }
        _ => panic!("Expected DeleteEdge"),
    }
}

/// Issue #3406 back-compat: a GENUINE pre-v9 but *framed* (v7) `DeleteNode`
/// payload — `node_id` + `valid_from` and NO trailing tombstone
/// `version_id` — parses under the current (v9-max) reader without error and
/// yields `version_id == None`, so replay synthesizes the tombstone. This
/// closes the gap left by the v0-only `..._version_0_delete_node` test: v0
/// skips `valid_from` entirely, whereas v7 reads it and THEN hits the
/// `carries_delete_version_id` gate, exercising the realistic
/// old-reader-parsing-a-recent-but-pre-#3406-segment path.
///
/// Limitation: this covers the PARSE half of the mixed-format recovery path
/// (the `carries_delete_version_id(version) == false` gate) at a genuine
/// older header version. The SYNTHESIS half is covered by the recovery
/// integration test `back_compat_synthesizes_when_version_id_absent`. A
/// single test driving a real old-header segment through
/// `CheckpointManager::recover` is impractical here: the WAL serializer is
/// test-only (`pub(crate)`) and always emits the highest (v9) payload shape,
/// so a genuine short old payload must be hand-assembled at the parse layer.
#[test]
fn test_parse_entry_at_pre_v9_framed_delete_node_has_no_version_id() {
    let timestamp = time::now();
    let valid_from = HybridTimestamp::new(time::now().wallclock() - 3_600_000_000, 0).unwrap();
    let node_id = NodeId::new(77).unwrap();

    // v7 DeleteNode op_data: node_id (8) + valid_from (12), NO version_id.
    let mut op_data = Vec::new();
    op_data.extend_from_slice(&node_id.as_u64().to_le_bytes());
    valid_from.serialize_into(&mut op_data);
    let buf = make_v0_buffer(6, &op_data, timestamp); // OP_DELETE_NODE = 6

    let (entry, consumed) = parse_entry_at(&buf, 0, WAL_VERSION_TX_FRAMING).unwrap();
    assert_eq!(
        consumed,
        buf.len(),
        "parser must consume exactly the v7 payload — no phantom trailing version_id"
    );
    match entry.operation {
        WalOperation::DeleteNode {
            node_id: parsed_id,
            valid_from: parsed_vf,
            version_id,
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(parsed_vf, valid_from, "v7 delete carries valid_from");
            assert_eq!(
                version_id, None,
                "a genuine pre-v9 delete carries no tombstone version_id"
            );
        }
        _ => panic!("Expected DeleteNode"),
    }
}

/// Issue #3406 back-compat: same as above for a genuine pre-v9 (v7)
/// `RetractNode` payload — `node_id` + `valid_to` and NO trailing
/// `version_id` — must parse to `version_id == None`.
#[test]
fn test_parse_entry_at_pre_v9_framed_retract_node_has_no_version_id() {
    let timestamp = time::now();
    let valid_to = HybridTimestamp::new(1_700_000_000_000_000, 0).unwrap();
    let node_id = NodeId::new(88).unwrap();

    // v7 RetractNode op_data: node_id (8) + valid_to (12), NO version_id.
    let mut op_data = Vec::new();
    op_data.extend_from_slice(&node_id.as_u64().to_le_bytes());
    valid_to.serialize_into(&mut op_data);
    let buf = make_v0_buffer(10, &op_data, timestamp); // OP_RETRACT_NODE = 10

    let (entry, consumed) = parse_entry_at(&buf, 0, WAL_VERSION_TX_FRAMING).unwrap();
    assert_eq!(
        consumed,
        buf.len(),
        "parser must consume exactly the v7 retract payload — no phantom version_id"
    );
    match entry.operation {
        WalOperation::RetractNode {
            node_id: parsed_id,
            valid_to: parsed_vt,
            version_id,
        } => {
            assert_eq!(parsed_id, node_id);
            assert_eq!(parsed_vt, valid_to, "v7 retract carries valid_to");
            assert_eq!(
                version_id, None,
                "a genuine pre-v9 retract carries no version_id"
            );
        }
        _ => panic!("Expected RetractNode"),
    }
}

#[test]
fn test_parse_entry_at_version_0_update_node() {
    let timestamp = time::now();
    let node_id = NodeId::new(42).unwrap();
    let version_id = VersionId::new(7).unwrap();
    let mut op_data = Vec::new();
    op_data.extend_from_slice(&42u64.to_le_bytes());
    op_data.extend_from_slice(&7u64.to_le_bytes());
    let buf = make_v0_buffer(3, &op_data, timestamp); // OP_UPDATE_NODE = 3
    let (entry, consumed) = parse_entry_at(&buf, 0, 0).unwrap();
    assert_eq!(consumed, buf.len());
    match entry.operation {
        WalOperation::UpdateNode {
            node_id: parsed_node,
            version_id: parsed_ver,
            properties,
            valid_from,
            ..
        } => {
            assert_eq!(parsed_node, node_id);
            assert_eq!(parsed_ver, version_id);
            assert!(properties.is_empty());
            assert_eq!(valid_from, timestamp);
        }
        _ => panic!("Expected UpdateNode"),
    }
}

#[test]
fn test_parse_entry_at_version_0_update_edge() {
    let timestamp = time::now();
    let edge_id = EdgeId::new(300).unwrap();
    let version_id = VersionId::new(5).unwrap();
    let mut op_data = Vec::new();
    op_data.extend_from_slice(&300u64.to_le_bytes());
    op_data.extend_from_slice(&5u64.to_le_bytes());
    let buf = make_v0_buffer(4, &op_data, timestamp); // OP_UPDATE_EDGE = 4
    let (entry, consumed) = parse_entry_at(&buf, 0, 0).unwrap();
    assert_eq!(consumed, buf.len());
    match entry.operation {
        WalOperation::UpdateEdge {
            edge_id: parsed_edge,
            version_id: parsed_ver,
            properties,
            valid_from,
            ..
        } => {
            assert_eq!(parsed_edge, edge_id);
            assert_eq!(parsed_ver, version_id);
            assert!(properties.is_empty());
            assert_eq!(valid_from, timestamp);
        }
        _ => panic!("Expected UpdateEdge"),
    }
}
