//! Injective, deterministic, domain-separated SHA-256 canonical encoder for a
//! version, plus the folding primitives that build a transaction digest and
//! chain step (Issue #3351).
//!
//! # Why a normalized [`VersionHashInput`]
//!
//! Both the seal path and the verify path hash the **same** normalized form so a
//! digest is reproducible from reconstructed history alone. [`From`] conversions
//! turn a live [`NodeVersion`]/[`EdgeVersion`] into a [`VersionHashInput`], and a
//! [`VersionSource`](super::verify::VersionSource) reconstructs one at verify
//! time; the encoder never sees the storage structs directly.
//!
//! # Injectivity
//!
//! The encoder is designed so that two *distinct logical values* can never
//! produce identical bytes:
//! - every value carries a 1-byte **type tag**,
//! - every variable-length field is **length-prefixed** with a big-endian `u64`,
//! - `Option`s emit a `0`/`1` presence byte,
//! - properties are **sorted by key** before encoding so map iteration order does
//!   not change the digest,
//! - `Bytes` (tag 5) and `String` (tag 4) differ by tag, so the same underlying
//!   bytes as a byte string vs. a UTF-8 string hash **differently** (the boundary
//!   the audit encoder's `Bytes` fallback previously violated — see the sibling
//!   fix in `src/audit/canonical.rs`).

use crate::core::hex;
use crate::core::property::PropertyValue;
use crate::core::version::{EdgeVersion, NodeVersion, VersionData};
use sha2::{Digest, Sha256};

/// Domain tag for a per-version leaf hash.
pub const CHAIN_LEAF_DOMAIN: &[u8] = b"aletheiadb.chain.v1.leaf\0";
/// Domain tag for a per-transaction digest.
pub const CHAIN_TX_DOMAIN: &[u8] = b"aletheiadb.chain.v1.tx\0";
/// Domain tag for a chain step (fold of previous digest + tx digest).
pub const CHAIN_NODE_DOMAIN: &[u8] = b"aletheiadb.chain.v1.node\0";
/// Domain tag for the genesis digest.
pub const CHAIN_GENESIS_DOMAIN: &[u8] = b"aletheiadb.chain.v1.genesis\0";

/// Whether a version belongs to a node or an edge. The tag byte is part of the
/// canonical encoding so a node and an edge that happen to share an id and
/// content still hash differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum EntityKind {
    /// A graph node.
    Node,
    /// A graph edge.
    Edge,
}

impl EntityKind {
    /// The 1-byte discriminator used in the canonical encoding and on disk.
    #[must_use]
    pub const fn tag(self) -> u8 {
        match self {
            EntityKind::Node => 0,
            EntityKind::Edge => 1,
        }
    }

    /// Reconstruct an [`EntityKind`] from its on-disk tag byte.
    #[must_use]
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(EntityKind::Node),
            1 => Some(EntityKind::Edge),
            _ => None,
        }
    }
}

/// Normalized, hashable form of a version's write-time provenance bundle.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProvenanceHashInput {
    /// Source system/identifier that produced the write.
    pub source: Option<String>,
    /// Writer confidence in `[0.0, 1.0]`.
    pub confidence: Option<f64>,
    /// Free-text note.
    pub note: Option<String>,
    /// Correlation id grouping writes of one logical operation.
    pub correlation_id: Option<String>,
    /// Authenticated principal that made the write.
    pub principal: Option<String>,
}

impl From<&crate::core::provenance::Provenance> for ProvenanceHashInput {
    fn from(p: &crate::core::provenance::Provenance) -> Self {
        ProvenanceHashInput {
            source: p.source().map(str::to_owned),
            confidence: p.confidence(),
            note: p.note().map(str::to_owned),
            correlation_id: p.correlation_id().map(str::to_owned),
            principal: p.principal().map(str::to_owned),
        }
    }
}

/// Normalized, hashable form of a single version.
///
/// This is the ONLY structure the canonical encoder reads, so seal-time and
/// verify-time necessarily hash identical bytes. Timestamps are microseconds
/// since the Unix epoch; a `None` end means an open (still-current) interval.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionHashInput {
    /// Whether this is a node or edge version.
    pub entity_kind: EntityKind,
    /// The owning entity's numeric id.
    pub entity_id: u64,
    /// This version's id.
    pub version_id: u64,
    /// The previous version's id in the chain, if any.
    pub prev_version_id: Option<u64>,
    /// Valid-time start (micros); `None` only for a degenerate open start.
    pub valid_from: Option<i64>,
    /// Valid-time end (micros); `None` == open.
    pub valid_to: Option<i64>,
    /// Transaction-time start (micros).
    pub transaction_from: Option<i64>,
    /// Transaction-time end (micros); `None` == open.
    pub transaction_to: Option<i64>,
    /// Whether this version is current in both temporal dimensions.
    pub is_current: bool,
    /// Write-time provenance, if any.
    pub provenance: Option<ProvenanceHashInput>,
    /// Materialized properties `(key, value)`. Encoding sorts by key, so the
    /// caller may supply them in any order.
    pub properties: Vec<(String, PropertyValue)>,
}

/// Extract `(key, value)` property pairs from a version's [`VersionData`].
///
/// For an anchor this is the full property snapshot. For a delta only the
/// changed (non-removed) properties are available from the version in isolation;
/// callers that need full-state leaves (the seal path) should reconstruct the
/// state first and build the [`VersionHashInput`] from that. This helper keeps
/// the [`From`] conversions total and lossless for anchors.
fn properties_from_data(data: &VersionData) -> Vec<(String, PropertyValue)> {
    use crate::core::interning::GLOBAL_INTERNER;
    let mut out: Vec<(String, PropertyValue)> = Vec::new();
    match data {
        VersionData::Anchor { properties, .. } => {
            for (key, value) in properties.iter() {
                if let Some(name) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) {
                    out.push((name, value.clone()));
                }
            }
        }
        VersionData::Delta { delta } => {
            for (key, value) in &delta.changed {
                if let Some(name) = GLOBAL_INTERNER.resolve_with(*key, |s| s.to_string()) {
                    out.push((name, value.clone()));
                }
            }
        }
    }
    out
}

impl From<&NodeVersion> for VersionHashInput {
    fn from(v: &NodeVersion) -> Self {
        let (vf, vt, tf, tt) = temporal_micros(&v.temporal);
        VersionHashInput {
            entity_kind: EntityKind::Node,
            entity_id: v.node_id.as_u64(),
            version_id: v.id.as_u64(),
            prev_version_id: v.prev_version.map(|p| p.as_u64()),
            valid_from: vf,
            valid_to: vt,
            transaction_from: tf,
            transaction_to: tt,
            is_current: v.temporal.is_current(),
            provenance: v.provenance.as_deref().map(ProvenanceHashInput::from),
            properties: properties_from_data(&v.data),
        }
    }
}

impl From<&EdgeVersion> for VersionHashInput {
    fn from(v: &EdgeVersion) -> Self {
        let (vf, vt, tf, tt) = temporal_micros(&v.temporal);
        VersionHashInput {
            entity_kind: EntityKind::Edge,
            entity_id: v.edge_id.as_u64(),
            version_id: v.id.as_u64(),
            prev_version_id: v.prev_version.map(|p| p.as_u64()),
            valid_from: vf,
            valid_to: vt,
            transaction_from: tf,
            transaction_to: tt,
            is_current: v.temporal.is_current(),
            provenance: v.provenance.as_deref().map(ProvenanceHashInput::from),
            properties: properties_from_data(&v.data),
        }
    }
}

/// Decompose a bi-temporal interval into `(valid_from, valid_to, tx_from,
/// tx_to)` micros, mapping the open-interval sentinel (`TIMESTAMP_MAX`) to
/// `None`.
fn temporal_micros(
    interval: &crate::core::temporal::BiTemporalInterval,
) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
    use crate::core::temporal::TIMESTAMP_MAX;
    let end_micros = |ts: crate::core::temporal::Timestamp| -> Option<i64> {
        if ts == TIMESTAMP_MAX {
            None
        } else {
            Some(ts.wallclock())
        }
    };
    let vt = interval.valid_time();
    let tt = interval.transaction_time();
    (
        Some(vt.start().wallclock()),
        end_micros(vt.end()),
        Some(tt.start().wallclock()),
        end_micros(tt.end()),
    )
}

/// A tiny append-only canonical byte writer (mirrors `audit::canonical::Canon`).
#[derive(Default)]
struct Canon {
    buf: Vec<u8>,
}

impl Canon {
    fn tag(&mut self, t: u8) {
        self.buf.push(t);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_be_bytes());
    }
    fn f32bits(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    fn f64bits(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.buf.extend_from_slice(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn opt_u64(&mut self, v: Option<u64>) {
        match v {
            None => self.buf.push(0),
            Some(x) => {
                self.buf.push(1);
                self.u64(x);
            }
        }
    }
    fn opt_i64(&mut self, v: Option<i64>) {
        match v {
            None => self.buf.push(0),
            Some(x) => {
                self.buf.push(1);
                self.i64(x);
            }
        }
    }
    fn opt_str(&mut self, v: Option<&str>) {
        match v {
            None => self.buf.push(0),
            Some(s) => {
                self.buf.push(1);
                self.str(s);
            }
        }
    }
    fn opt_f64(&mut self, v: Option<f64>) {
        match v {
            None => self.buf.push(0),
            Some(x) => {
                self.buf.push(1);
                self.f64bits(x);
            }
        }
    }

    /// Encode a property value with a per-variant type tag. Every variable-length
    /// payload is length-prefixed, so the encoding is injective across variants.
    fn value(&mut self, v: &PropertyValue) {
        match v {
            PropertyValue::Null => self.tag(0),
            PropertyValue::Bool(b) => {
                self.tag(1);
                self.bool(*b);
            }
            PropertyValue::Int(i) => {
                self.tag(2);
                self.i64(*i);
            }
            PropertyValue::Float(f) => {
                self.tag(3);
                self.f64bits(*f);
            }
            PropertyValue::String(s) => {
                self.tag(4);
                self.str(s);
            }
            PropertyValue::Bytes(b) => {
                // Distinct tag from String: identical underlying bytes as a byte
                // string vs. a UTF-8 string hash differently.
                self.tag(5);
                self.bytes(b);
            }
            PropertyValue::Array(items) => {
                self.tag(6);
                self.u64(items.len() as u64);
                for item in items.iter() {
                    self.value(item);
                }
            }
            PropertyValue::Vector(vec) => {
                self.tag(7);
                self.u64(vec.len() as u64);
                for f in vec.iter() {
                    self.f32bits(*f);
                }
            }
            PropertyValue::SparseVector(sparse) => {
                self.tag(8);
                self.u64(sparse.dimension() as u64);
                let indices = sparse.indices();
                self.u64(indices.len() as u64);
                for i in indices {
                    self.u32(*i);
                }
                let values = sparse.values();
                self.u64(values.len() as u64);
                for f in values {
                    self.f32bits(*f);
                }
            }
        }
    }

    fn provenance(&mut self, p: &Option<ProvenanceHashInput>) {
        match p {
            None => self.buf.push(0),
            Some(pr) => {
                self.buf.push(1);
                self.opt_str(pr.source.as_deref());
                self.opt_f64(pr.confidence);
                self.opt_str(pr.note.as_deref());
                self.opt_str(pr.correlation_id.as_deref());
                self.opt_str(pr.principal.as_deref());
            }
        }
    }
}

/// Deterministic canonical bytes for a version (independent of property order).
fn version_canonical(input: &VersionHashInput) -> Vec<u8> {
    let mut c = Canon::default();
    c.tag(input.entity_kind.tag());
    c.u64(input.entity_id);
    c.u64(input.version_id);
    c.opt_u64(input.prev_version_id);
    c.opt_i64(input.valid_from);
    c.opt_i64(input.valid_to);
    c.opt_i64(input.transaction_from);
    c.opt_i64(input.transaction_to);
    c.bool(input.is_current);
    c.provenance(&input.provenance);

    // Sort properties by key so map iteration order is not signed content.
    let mut props: Vec<&(String, PropertyValue)> = input.properties.iter().collect();
    props.sort_by(|a, b| a.0.cmp(&b.0));
    c.u64(props.len() as u64);
    for (key, value) in props {
        c.str(key);
        c.value(value);
    }
    c.buf
}

/// SHA-256 of `domain || payload`.
fn hash_domain(domain: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(domain);
    h.update(payload);
    h.finalize().into()
}

/// The per-version leaf hash: `SHA256(LEAF_DOMAIN || canon(version))`.
#[must_use]
pub fn version_leaf(input: &VersionHashInput) -> [u8; 32] {
    hash_domain(CHAIN_LEAF_DOMAIN, &version_canonical(input))
}

/// The per-transaction digest, order- and count-dependent over its leaves:
/// `SHA256(TX_DOMAIN || commit_ts || tx_id || len || leaf_0 || .. || leaf_n)`.
#[must_use]
pub fn tx_digest(commit_ts_micros: i64, tx_id: u64, leaves: &[[u8; 32]]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(CHAIN_TX_DOMAIN);
    h.update(commit_ts_micros.to_be_bytes());
    h.update(tx_id.to_be_bytes());
    h.update((leaves.len() as u64).to_be_bytes());
    for leaf in leaves {
        h.update(leaf);
    }
    h.finalize().into()
}

/// One chain step: `SHA256(NODE_DOMAIN || prev || tx_digest)`.
#[must_use]
pub fn chain_step(prev: &[u8; 32], tx_digest: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(CHAIN_NODE_DOMAIN);
    h.update(prev);
    h.update(tx_digest);
    h.finalize().into()
}

/// The genesis digest seeding a chain: `SHA256(GENESIS_DOMAIN || enable_lsn ||
/// enable_ts)`.
#[must_use]
pub fn genesis_digest(enable_lsn: u64, enable_ts: i64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(CHAIN_GENESIS_DOMAIN);
    h.update(enable_lsn.to_be_bytes());
    h.update(enable_ts.to_be_bytes());
    h.finalize().into()
}

/// Lowercase-hex encode a 32-byte digest.
#[must_use]
pub fn to_hex(bytes: &[u8; 32]) -> String {
    hex::encode(bytes)
}

/// Decode a 64-char lowercase-hex string into a 32-byte digest.
#[must_use]
pub fn from_hex_32(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s)?;
    if v.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn base() -> VersionHashInput {
        VersionHashInput {
            entity_kind: EntityKind::Node,
            entity_id: 1,
            version_id: 1,
            prev_version_id: None,
            valid_from: Some(100),
            valid_to: None,
            transaction_from: Some(100),
            transaction_to: None,
            is_current: true,
            provenance: None,
            properties: vec![("name".to_string(), PropertyValue::string("Alice"))],
        }
    }

    #[test]
    fn distinct_property_values_produce_distinct_leaves() {
        let a = base();
        let mut b = base();
        b.properties = vec![("name".to_string(), PropertyValue::string("Bob"))];
        assert_ne!(version_leaf(&a), version_leaf(&b));
    }

    #[test]
    fn distinct_provenance_produces_distinct_leaves() {
        let a = base();
        let mut b = base();
        b.provenance = Some(ProvenanceHashInput {
            source: Some("csv".to_string()),
            ..Default::default()
        });
        assert_ne!(version_leaf(&a), version_leaf(&b));
    }

    #[test]
    fn distinct_bitemporal_coords_produce_distinct_leaves() {
        let a = base();
        let mut b = base();
        b.valid_to = Some(999); // was open (None)
        assert_ne!(version_leaf(&a), version_leaf(&b));

        let mut c = base();
        c.transaction_from = Some(101);
        assert_ne!(version_leaf(&a), version_leaf(&c));
    }

    #[test]
    fn property_order_independence() {
        let mut a = base();
        a.properties = vec![
            ("name".to_string(), PropertyValue::string("Alice")),
            ("age".to_string(), PropertyValue::Int(30)),
            ("city".to_string(), PropertyValue::string("NYC")),
        ];
        let mut b = base();
        b.properties = vec![
            ("city".to_string(), PropertyValue::string("NYC")),
            ("name".to_string(), PropertyValue::string("Alice")),
            ("age".to_string(), PropertyValue::Int(30)),
        ];
        assert_eq!(
            version_leaf(&a),
            version_leaf(&b),
            "reordering the same property map must not change the leaf"
        );
    }

    #[test]
    fn bytes_and_string_with_same_underlying_bytes_differ() {
        // The exact boundary the audit Bytes fallback violated: a byte string and
        // a UTF-8 string over identical bytes must hash differently.
        let mut a = base();
        a.properties = vec![(
            "x".to_string(),
            PropertyValue::Bytes(Arc::from(&b"hello"[..])),
        )];
        let mut b = base();
        b.properties = vec![("x".to_string(), PropertyValue::string("hello"))];
        assert_ne!(version_leaf(&a), version_leaf(&b));
    }

    #[test]
    fn tx_digest_is_order_and_count_dependent() {
        let l1 = version_leaf(&base());
        let mut other = base();
        other.version_id = 2;
        let l2 = version_leaf(&other);

        let d_12 = tx_digest(500, 7, &[l1, l2]);
        let d_21 = tx_digest(500, 7, &[l2, l1]);
        let d_1 = tx_digest(500, 7, &[l1]);
        assert_ne!(d_12, d_21, "leaf order must matter");
        assert_ne!(d_12, d_1, "leaf count must matter");

        // commit_ts and tx_id are bound into the digest.
        assert_ne!(d_12, tx_digest(501, 7, &[l1, l2]));
        assert_ne!(d_12, tx_digest(500, 8, &[l1, l2]));
    }

    #[test]
    fn genesis_digest_is_deterministic_and_input_sensitive() {
        assert_eq!(genesis_digest(10, 1000), genesis_digest(10, 1000));
        assert_ne!(genesis_digest(10, 1000), genesis_digest(11, 1000));
        assert_ne!(genesis_digest(10, 1000), genesis_digest(10, 1001));
    }

    #[test]
    fn hex_round_trip() {
        let d = version_leaf(&base());
        let s = to_hex(&d);
        assert_eq!(s.len(), 64);
        assert_eq!(from_hex_32(&s), Some(d));
        assert_eq!(from_hex_32("nothex"), None);
        assert_eq!(from_hex_32("ab"), None); // too short
    }

    #[test]
    fn entity_kind_tag_round_trip() {
        assert_eq!(
            EntityKind::from_tag(EntityKind::Node.tag()),
            Some(EntityKind::Node)
        );
        assert_eq!(
            EntityKind::from_tag(EntityKind::Edge.tag()),
            Some(EntityKind::Edge)
        );
        assert_eq!(EntityKind::from_tag(9), None);
    }

    #[test]
    fn node_and_edge_with_same_ids_hash_differently() {
        let mut node = base();
        node.entity_kind = EntityKind::Node;
        let mut edge = base();
        edge.entity_kind = EntityKind::Edge;
        assert_ne!(version_leaf(&node), version_leaf(&edge));
    }

    #[test]
    fn from_node_version_hashes() {
        use crate::core::id::{NodeId, VersionId};
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMapBuilder;
        use crate::core::temporal::BiTemporalInterval;

        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let temporal = BiTemporalInterval::current(1000.into());
        let v = NodeVersion::new_anchor(
            VersionId::new(1).unwrap(),
            NodeId::new(10).unwrap(),
            temporal,
            GLOBAL_INTERNER.intern("Person").unwrap(),
            props,
        );
        let input = VersionHashInput::from(&v);
        assert_eq!(input.entity_kind, EntityKind::Node);
        assert_eq!(input.entity_id, 10);
        assert_eq!(input.properties.len(), 1);
        // Leaf is stable across repeated conversions.
        assert_eq!(
            version_leaf(&input),
            version_leaf(&VersionHashInput::from(&v))
        );
    }
}
