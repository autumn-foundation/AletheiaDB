//! On-chain records: a per-transaction record and a head checkpoint (Issue #3351).
//!
//! Each type has a self-describing, length-framed binary codec used by
//! [`super::store`], and (behind the `serde` feature) a JSON representation with
//! digests rendered as lowercase hex for external export / anchoring.

use super::canonical::EntityKind;
use super::error::{ChainError, ChainResult};

/// A sealed record for one committed transaction.
///
/// `digest` is `chain_step(prev_digest, tx_digest(commit_ts, tx_id, leaves))`,
/// where `prev_digest` is the previous record's digest (or the genesis digest
/// for `seq == 1`). The record is self-contained: it carries the leaves and the
/// entity refs that produced them so a verifier can recompute everything.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChainTxRecord {
    /// Monotonic sequence number (genesis is `0`; first real tx is `1`).
    pub seq: u64,
    /// Commit timestamp (micros since epoch) — the primary ordering key.
    pub commit_ts: i64,
    /// Logical counter of the HLC commit timestamp — the tie-breaker so two
    /// transactions sharing a wallclock microsecond stay distinct and ordered
    /// (Issue #3351 finding 4). Bound into `tx_digest` alongside `commit_ts`.
    pub commit_ts_logical: u32,
    /// The committing transaction's id. Record metadata only — NOT bound into
    /// `tx_digest` (a post-crash rebuild cannot recover it; see
    /// [`tx_digest`](super::canonical::tx_digest)).
    pub tx_id: u64,
    /// The WAL LSN this record is anchored at (for truncation/recovery).
    pub anchor_lsn: u64,
    /// Per-version leaf hashes, in the same order as `entity_refs`.
    #[cfg_attr(feature = "serde", serde(with = "hex_vec"))]
    pub leaves: Vec<[u8; 32]>,
    /// `(kind, entity_id, version_id)` for each leaf, in leaf order.
    pub entity_refs: Vec<(EntityKind, u64, u64)>,
    /// This record's chained digest.
    #[cfg_attr(feature = "serde", serde(with = "hex_digest"))]
    pub digest: [u8; 32],
}

/// An exported/checkpointed head of the chain — the external anchor used to
/// prove append-only extension (rollback/fork detection).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChainHead {
    /// Sequence number of the head record (`0` for a bare genesis).
    pub seq: u64,
    /// The head record's chained digest.
    #[cfg_attr(feature = "serde", serde(with = "hex_digest"))]
    pub digest: [u8; 32],
    /// Commit timestamp of the head record (micros).
    pub commit_ts: i64,
    /// WAL LSN the head is anchored at.
    pub anchor_lsn: u64,
    /// The genesis digest this chain was seeded with.
    #[cfg_attr(feature = "serde", serde(with = "hex_digest"))]
    pub genesis_digest: [u8; 32],
}

impl ChainHead {
    /// Construct the genesis head (seq 0) for a chain enabled at
    /// `(enable_lsn, enable_ts)`. Its `digest` equals its `genesis_digest`, so it
    /// seeds `chain_step` for the first real transaction.
    #[must_use]
    pub fn genesis(enable_lsn: u64, enable_ts: i64) -> Self {
        let g = super::canonical::genesis_digest(enable_lsn, enable_ts);
        ChainHead {
            seq: 0,
            digest: g,
            commit_ts: enable_ts,
            anchor_lsn: enable_lsn,
            genesis_digest: g,
        }
    }
}

// ---------------------------------------------------------------------------
// Binary codecs (used by the append-only store; independent of serde).
// ---------------------------------------------------------------------------

/// A minimal big-endian byte reader with bounds checks.
pub(super) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> ChainResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| ChainError::BadFraming("length overflow".into()))?;
        if end > self.buf.len() {
            return Err(ChainError::BadFraming(format!(
                "unexpected end of buffer: need {n} bytes at offset {}",
                self.pos
            )));
        }
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u64(&mut self) -> ChainResult<u64> {
        let b = self.take(8)?;
        let mut arr = [0u8; 8];
        arr.copy_from_slice(b);
        Ok(u64::from_be_bytes(arr))
    }

    fn u32(&mut self) -> ChainResult<u32> {
        let b = self.take(4)?;
        let mut arr = [0u8; 4];
        arr.copy_from_slice(b);
        Ok(u32::from_be_bytes(arr))
    }

    fn i64(&mut self) -> ChainResult<i64> {
        Ok(self.u64()? as i64)
    }

    fn u8(&mut self) -> ChainResult<u8> {
        Ok(self.take(1)?[0])
    }

    fn digest(&mut self) -> ChainResult<[u8; 32]> {
        let b = self.take(32)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(b);
        Ok(arr)
    }

    /// A `u64`-prefixed count, guarded against absurd values so a corrupt length
    /// cannot trigger a huge pre-allocation.
    fn count(&mut self, remaining_min_per_item: usize) -> ChainResult<usize> {
        let n = self.u64()? as usize;
        // Each item consumes at least `remaining_min_per_item` bytes; reject a
        // count that cannot possibly fit in the remaining buffer.
        let remaining = self.buf.len().saturating_sub(self.pos);
        if remaining_min_per_item > 0 && n > remaining / remaining_min_per_item + 1 {
            return Err(ChainError::BadFraming(format!(
                "declared count {n} exceeds buffer capacity"
            )));
        }
        Ok(n)
    }
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

impl ChainTxRecord {
    /// Encode the record to its self-contained binary payload (no outer frame).
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.leaves.len() * 32 + self.entity_refs.len() * 17);
        put_u64(&mut out, self.seq);
        put_u64(&mut out, self.commit_ts as u64);
        put_u32(&mut out, self.commit_ts_logical);
        put_u64(&mut out, self.tx_id);
        put_u64(&mut out, self.anchor_lsn);
        put_u64(&mut out, self.leaves.len() as u64);
        for leaf in &self.leaves {
            out.extend_from_slice(leaf);
        }
        put_u64(&mut out, self.entity_refs.len() as u64);
        for (kind, id, vid) in &self.entity_refs {
            out.push(kind.tag());
            put_u64(&mut out, *id);
            put_u64(&mut out, *vid);
        }
        out.extend_from_slice(&self.digest);
        out
    }

    /// Decode a record from a binary payload (exactly one record; trailing bytes
    /// are an error).
    pub(super) fn decode(bytes: &[u8]) -> ChainResult<Self> {
        let mut r = Reader::new(bytes);
        let rec = Self::read_from(&mut r)?;
        if r.pos != bytes.len() {
            return Err(ChainError::BadFraming(
                "trailing bytes after record payload".into(),
            ));
        }
        Ok(rec)
    }

    fn read_from(r: &mut Reader<'_>) -> ChainResult<Self> {
        let seq = r.u64()?;
        let commit_ts = r.i64()?;
        let commit_ts_logical = r.u32()?;
        let tx_id = r.u64()?;
        let anchor_lsn = r.u64()?;
        let n_leaves = r.count(32)?;
        let mut leaves = Vec::with_capacity(n_leaves);
        for _ in 0..n_leaves {
            leaves.push(r.digest()?);
        }
        let n_refs = r.count(17)?;
        let mut entity_refs = Vec::with_capacity(n_refs);
        for _ in 0..n_refs {
            let tag = r.u8()?;
            let kind = EntityKind::from_tag(tag)
                .ok_or_else(|| ChainError::Corrupt(format!("invalid entity-kind tag {tag}")))?;
            let id = r.u64()?;
            let vid = r.u64()?;
            entity_refs.push((kind, id, vid));
        }
        let digest = r.digest()?;
        Ok(ChainTxRecord {
            seq,
            commit_ts,
            commit_ts_logical,
            tx_id,
            anchor_lsn,
            leaves,
            entity_refs,
            digest,
        })
    }
}

impl ChainHead {
    /// Encode the head to its self-contained binary payload (no outer frame).
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 32 + 8 + 8 + 32);
        put_u64(&mut out, self.seq);
        out.extend_from_slice(&self.digest);
        put_u64(&mut out, self.commit_ts as u64);
        put_u64(&mut out, self.anchor_lsn);
        out.extend_from_slice(&self.genesis_digest);
        out
    }

    /// Decode a head from a binary payload.
    pub(super) fn decode(bytes: &[u8]) -> ChainResult<Self> {
        let mut r = Reader::new(bytes);
        let seq = r.u64()?;
        let digest = r.digest()?;
        let commit_ts = r.i64()?;
        let anchor_lsn = r.u64()?;
        let genesis_digest = r.digest()?;
        Ok(ChainHead {
            seq,
            digest,
            commit_ts,
            anchor_lsn,
            genesis_digest,
        })
    }
}

// ---------------------------------------------------------------------------
// serde hex helpers.
// ---------------------------------------------------------------------------

#[cfg(feature = "serde")]
mod hex_digest {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::super::canonical::to_hex(d))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        super::super::canonical::from_hex_32(&s)
            .ok_or_else(|| serde::de::Error::custom("invalid 32-byte hex digest"))
    }
}

#[cfg(feature = "serde")]
mod hex_vec {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &[[u8; 32]], s: S) -> Result<S::Ok, S::Error> {
        let hexed: Vec<String> = v.iter().map(super::super::canonical::to_hex).collect();
        hexed.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<[u8; 32]>, D::Error> {
        let hexed = Vec::<String>::deserialize(d)?;
        hexed
            .iter()
            .map(|s| {
                super::super::canonical::from_hex_32(s)
                    .ok_or_else(|| serde::de::Error::custom("invalid 32-byte hex digest"))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(seq: u64) -> ChainTxRecord {
        ChainTxRecord {
            seq,
            commit_ts: 1_000 + seq as i64,
            commit_ts_logical: seq as u32,
            tx_id: 42 + seq,
            anchor_lsn: 7 + seq,
            leaves: vec![[seq as u8; 32], [(seq + 1) as u8; 32]],
            entity_refs: vec![(EntityKind::Node, 10, 1), (EntityKind::Edge, 20, 2)],
            digest: [(seq + 3) as u8; 32],
        }
    }

    #[test]
    fn record_binary_round_trip() {
        let rec = sample_record(5);
        let encoded = rec.encode();
        let decoded = ChainTxRecord::decode(&encoded).unwrap();
        assert_eq!(rec, decoded);
    }

    #[test]
    fn record_decode_rejects_trailing_bytes() {
        let mut encoded = sample_record(1).encode();
        encoded.push(0xAB);
        assert!(matches!(
            ChainTxRecord::decode(&encoded),
            Err(ChainError::BadFraming(_))
        ));
    }

    #[test]
    fn record_decode_rejects_truncation() {
        let encoded = sample_record(1).encode();
        assert!(ChainTxRecord::decode(&encoded[..encoded.len() - 4]).is_err());
    }

    #[test]
    fn head_binary_round_trip() {
        let head = ChainHead::genesis(99, 12345);
        let decoded = ChainHead::decode(&head.encode()).unwrap();
        assert_eq!(head, decoded);
        assert_eq!(head.seq, 0);
        assert_eq!(head.digest, head.genesis_digest);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn record_serde_uses_hex_digests() {
        let rec = sample_record(2);
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains(&super::super::canonical::to_hex(&rec.digest)));
        let back: ChainTxRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }
}
