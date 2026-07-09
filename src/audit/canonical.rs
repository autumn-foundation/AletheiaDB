//! Deterministic canonical encoding + SHA-256 hash chain (Issue #3358).
//!
//! The signature is computed over a canonical *binary* encoding of the typed
//! [`super::model`] values — NOT the JSON text — so that reformatting the JSON
//! (whitespace, key order, float rendering) never invalidates a signature, while
//! any change to signed content (a value, a timestamp, a provenance field, the
//! order or count of versions) always changes a leaf hash and therefore the root.
//!
//! # Canonical encoding (independently implementable)
//!
//! Primitive framing, all integers big-endian:
//! - `u8` tag, `u32`/`u64`/`i64` fixed-width, `f64` as its IEEE-754 `to_bits()` u64,
//! - `bool` as one byte (`0`/`1`),
//! - length-prefixed byte strings: `u64` length then raw UTF-8/bytes,
//! - `Option<T>`: a `0` byte for `None`, or a `1` byte then `T`.
//!
//! A version leaf is `SHA256("aletheiadb.audit.v1.leaf\0" || canon(version))`.
//! The root folds metadata then every leaf in flattened entity→version order:
//! `acc = SHA256("aletheiadb.audit.v1.root\0" || canon(metadata))`, then for each
//! leaf `acc = SHA256(acc || leaf)`; the final `acc` is the root the signature
//! covers. The fold is order- and count-dependent, so reordering or truncating
//! versions changes the root.

use crate::audit::model::{
    ExportMetadata, ExportedEntity, ExportedProperty, ExportedProvenance, ExportedValue,
    ExportedVersion,
};
use crate::core::hex;
use sha2::{Digest, Sha256};

const LEAF_DOMAIN: &[u8] = b"aletheiadb.audit.v1.leaf\0";
const ROOT_DOMAIN: &[u8] = b"aletheiadb.audit.v1.root\0";
const HEADER_DOMAIN: &[u8] = b"aletheiadb.audit.v1.header\0";

/// A tiny append-only canonical byte writer.
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
    fn f64(&mut self, v: f64) {
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
    fn opt_i64(&mut self, v: Option<i64>) {
        match v {
            None => self.buf.push(0),
            Some(x) => {
                self.buf.push(1);
                self.i64(x);
            }
        }
    }
    fn opt_u32(&mut self, v: Option<u32>) {
        match v {
            None => self.buf.push(0),
            Some(x) => {
                self.buf.push(1);
                self.u32(x);
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
                self.f64(x);
            }
        }
    }

    fn value(&mut self, v: &ExportedValue) {
        match v {
            ExportedValue::Null => self.tag(0),
            ExportedValue::Bool { value } => {
                self.tag(1);
                self.bool(*value);
            }
            ExportedValue::Int { value } => {
                self.tag(2);
                self.i64(*value);
            }
            ExportedValue::Float { value } => {
                self.tag(3);
                self.f64(*value);
            }
            ExportedValue::String { value } => {
                self.tag(4);
                self.str(value);
            }
            ExportedValue::Bytes { hex } => {
                self.tag(5);
                // Encode the decoded bytes so an equivalent-but-recased hex still
                // canonicalizes identically; fall back to raw string on garbage.
                match hex::decode(hex) {
                    Some(bytes) => self.bytes(&bytes),
                    None => self.str(hex),
                }
            }
            ExportedValue::Array { values } => {
                self.tag(6);
                self.u64(values.len() as u64);
                for e in values {
                    self.value(e);
                }
            }
            ExportedValue::Vector { values } => {
                self.tag(7);
                self.u64(values.len() as u64);
                for s in values {
                    // Canonicalize via the parsed float bits.
                    let f = crate::audit::model::token_to_f64(s).unwrap_or(f64::NAN);
                    self.f64(f);
                }
            }
            ExportedValue::SparseVector {
                dimension,
                indices,
                values,
            } => {
                self.tag(8);
                self.u64(*dimension as u64);
                self.u64(indices.len() as u64);
                for i in indices {
                    self.u32(*i);
                }
                self.u64(values.len() as u64);
                for s in values {
                    let f = crate::audit::model::token_to_f64(s).unwrap_or(f64::NAN);
                    self.f64(f);
                }
            }
        }
    }

    fn provenance(&mut self, p: &Option<ExportedProvenance>) {
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

    fn property(&mut self, p: &ExportedProperty) {
        self.str(&p.key);
        self.bool(p.redacted);
        match &p.value {
            None => self.buf.push(0),
            Some(v) => {
                self.buf.push(1);
                self.value(v);
            }
        }
    }
}

/// Canonical bytes for a single version leaf (bound to its entity identity).
fn version_canonical(entity_kind: &str, entity_id: u64, v: &ExportedVersion) -> Vec<u8> {
    let mut c = Canon::default();
    c.str(entity_kind);
    c.u64(entity_id);
    c.u64(v.version_number);
    c.u64(v.version_id);
    c.str(&v.label);
    c.i64(v.valid_from_micros);
    c.u32(v.valid_from_logical);
    c.opt_i64(v.valid_to_micros);
    c.opt_u32(v.valid_to_logical);
    c.i64(v.transaction_from_micros);
    c.u32(v.transaction_from_logical);
    c.opt_i64(v.transaction_to_micros);
    c.opt_u32(v.transaction_to_logical);
    c.bool(v.is_current);
    c.provenance(&v.provenance);

    // Sort properties by key so property-array ordering is not signed content.
    let mut props: Vec<&ExportedProperty> = v.properties.iter().collect();
    props.sort_by(|a, b| a.key.cmp(&b.key));
    c.u64(props.len() as u64);
    for p in props {
        c.property(p);
    }
    c.buf
}

/// Canonical bytes for the export metadata (folded first into the root).
fn metadata_canonical(m: &ExportMetadata) -> Vec<u8> {
    let mut c = Canon::default();
    c.str(&m.database_id);
    c.str(&m.exported_at);
    c.str(&m.tool_version);
    c.str(&m.scope_description);
    c.str(&m.scope.kind);
    // requested + included + not_included (order as recorded, they are stable).
    c.u64(m.scope.requested.len() as u64);
    for e in &m.scope.requested {
        c.str(&e.kind);
        c.u64(e.id);
    }
    c.u64(m.scope.included.len() as u64);
    for e in &m.scope.included {
        c.str(&e.kind);
        c.u64(e.id);
    }
    c.u64(m.scope.not_included.len() as u64);
    for e in &m.scope.not_included {
        c.str(&e.kind);
        c.u64(e.id);
        c.str(&e.reason);
    }
    match &m.scope.coordinate {
        None => c.buf.push(0),
        Some(co) => {
            c.buf.push(1);
            c.opt_str(co.valid_time.as_deref());
            c.opt_str(co.transaction_time.as_deref());
        }
    }
    c.opt_u32(m.scope.hops);
    c.u64(m.chain_anchor.source_lsn);
    c.str(&m.chain_anchor.description);
    // Redacted keys folded sorted so their order is not signed content.
    let mut rk: Vec<&String> = m.redacted_keys.iter().collect();
    rk.sort();
    c.u64(rk.len() as u64);
    for k in rk {
        c.str(k);
    }
    c.u64(m.entity_count as u64);
    c.u64(m.version_count as u64);
    c.str(&m.hash_algorithm);
    c.str(&m.signature_algorithm);
    c.buf
}

/// Canonical bytes for an entity header (binds label / edge endpoints).
fn entity_header_canonical(entity: &ExportedEntity) -> Vec<u8> {
    let mut c = Canon::default();
    c.str(&entity.kind);
    c.u64(entity.id);
    c.opt_str(entity.label.as_deref());
    match entity.source {
        None => c.buf.push(0),
        Some(s) => {
            c.buf.push(1);
            c.u64(s);
        }
    }
    match entity.target {
        None => c.buf.push(0),
        Some(t) => {
            c.buf.push(1);
            c.u64(t);
        }
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

/// Compute the per-version leaf hashes AND the folded chain root in one pass.
///
/// Returns `(leaves, root)` where `leaves` are the per-version hashes in
/// flattened entity→version order (stored in the artifact) and `root` is the
/// 32-byte value the signature covers. The fold binds, in order: the metadata,
/// then for each entity its header followed by each of its version leaves — so
/// tampering an endpoint, reordering, or truncating any of them changes `root`.
pub(crate) fn compute_chain(
    metadata: &ExportMetadata,
    entities: &[ExportedEntity],
) -> (Vec<[u8; 32]>, [u8; 32]) {
    let mut acc = hash_domain(ROOT_DOMAIN, &metadata_canonical(metadata));
    let mut leaves = Vec::new();
    for entity in entities {
        let header = hash_domain(HEADER_DOMAIN, &entity_header_canonical(entity));
        acc = fold(acc, &header);
        for v in &entity.versions {
            let leaf = hash_domain(LEAF_DOMAIN, &version_canonical(&entity.kind, entity.id, v));
            leaves.push(leaf);
            acc = fold(acc, &leaf);
        }
    }
    (leaves, acc)
}

/// `SHA256(acc || next)`.
fn fold(acc: [u8; 32], next: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(acc);
    h.update(next);
    h.finalize().into()
}

/// Hex-encode a 32-byte hash.
pub(crate) fn to_hex(bytes: &[u8; 32]) -> String {
    hex::encode(bytes)
}

/// Decode a 32-byte hex hash.
pub(crate) fn hex_to_32(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s)?;
    if v.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    Some(out)
}
