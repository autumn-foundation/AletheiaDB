//! Graph-wide temporal changefeed types (Issue #3216).
//!
//! These types back [`crate::db::AletheiaDB::list_changes`], a read-only API that
//! enumerates the entities (nodes **and** edges) whose versions were committed within a
//! transaction-time window `[t1, t2)`. This is the *discovery* half of the bi-temporal
//! story: callers can ask "what changed between T1 and T2?" without already knowing any
//! entity IDs, then drill into each result with the existing per-entity history/diff APIs.
//!
//! The feed is **bounded** (a `limit` plus a stable, opaque continuation cursor) and
//! **deterministically ordered** (transaction-time ascending, then entity kind, then id)
//! so that paginated reads are stable and replayable.

use crate::core::error::{Error, QueryError};
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::temporal::{BiTemporalInterval, TimeRange, Timestamp};

/// Whether a change record refers to a node or an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    /// The change refers to a node.
    Node,
    /// The change refers to an edge.
    Edge,
}

impl EntityKind {
    /// Stable ordinal used for deterministic ordering and cursor encoding.
    ///
    /// Defined explicitly (not via enum discriminant) so ordering is unaffected by
    /// future reordering of the enum variants.
    #[inline]
    pub const fn ord(self) -> u8 {
        match self {
            EntityKind::Node => 0,
            EntityKind::Edge => 1,
        }
    }

    /// Lowercase string form used in MCP/JSON responses.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            EntityKind::Node => "node",
            EntityKind::Edge => "edge",
        }
    }
}

/// Classification of a single committed version relative to the entity's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeType {
    /// The first version of the entity (no previous version).
    Created,
    /// A subsequent, non-deleting version of an existing entity.
    Modified,
    /// A tombstone version: the entity was deleted (valid-time closed to an empty range).
    Deleted,
}

impl ChangeType {
    /// Lowercase string form used in MCP/JSON responses.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            ChangeType::Created => "created",
            ChangeType::Modified => "modified",
            ChangeType::Deleted => "deleted",
        }
    }
}

/// One row of the changefeed: a single entity version committed within the query window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRecord {
    /// Entity id (`NodeId`/`EdgeId` as `u64`).
    pub entity_id: u64,
    /// Whether this is a node or an edge.
    pub kind: EntityKind,
    /// How the entity changed at this version.
    pub change_type: ChangeType,
    /// Resolved node label / edge type.
    pub label: String,
    /// Commit timestamp (== `transaction_time_range.start()`).
    pub transaction_time: Timestamp,
    /// Full transaction-time range of the version.
    pub transaction_time_range: TimeRange,
    /// Full valid-time range of the version (an empty range denotes a deletion).
    pub valid_time_range: TimeRange,
}

impl ChangeRecord {
    /// The total-order sort key for this record (tx-time asc, then kind, then id).
    #[inline]
    pub(crate) fn cursor(&self) -> ChangeCursor {
        ChangeCursor {
            tx_wallclock: self.transaction_time.wallclock(),
            tx_logical: self.transaction_time.logical(),
            kind_ord: self.kind.ord(),
            entity_id: self.entity_id,
        }
    }
}

/// Total-order sort key used for deterministic ordering and cursor pagination.
///
/// Ordering is by `(tx_wallclock, tx_logical, kind_ord, entity_id)` ascending. This tuple is
/// unique per emitted row because a single entity cannot have two versions at the exact same
/// hybrid transaction time, so "resume strictly after this key" is unambiguous and replayable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ChangeCursor {
    pub tx_wallclock: i64,
    pub tx_logical: u32,
    pub kind_ord: u8,
    pub entity_id: u64,
}

impl ChangeCursor {
    /// Encode this key as an opaque, URL-safe continuation token.
    ///
    /// The underlying form is a delimited string hex-encoded so clients treat it as
    /// opaque and never attempt to parse or construct it by hand.
    pub(crate) fn encode(&self) -> String {
        let raw = format!(
            "{}:{}:{}:{}",
            self.tx_wallclock, self.tx_logical, self.kind_ord, self.entity_id
        );
        hex_encode(raw.as_bytes())
    }

    /// Decode an opaque continuation token produced by [`ChangeCursor::encode`].
    ///
    /// Returns a `QueryError::InvalidParameter` (never panics) when the token is malformed.
    pub(crate) fn decode(token: &str) -> Result<Self, Error> {
        let bytes = hex_decode(token).ok_or_else(|| invalid_cursor("not valid hex"))?;
        let raw = std::str::from_utf8(&bytes).map_err(|_| invalid_cursor("not valid utf-8"))?;
        let mut parts = raw.split(':');
        let tx_wallclock = parse_field(parts.next())?;
        let tx_logical = parse_field(parts.next())?;
        let kind_ord = parse_field(parts.next())?;
        let entity_id = parse_field(parts.next())?;
        if parts.next().is_some() {
            return Err(invalid_cursor("too many fields"));
        }
        Ok(ChangeCursor {
            tx_wallclock,
            tx_logical,
            kind_ord,
            entity_id,
        })
    }
}

fn parse_field<T: std::str::FromStr>(field: Option<&str>) -> Result<T, Error> {
    field
        .ok_or_else(|| invalid_cursor("missing field"))?
        .parse::<T>()
        .map_err(|_| invalid_cursor("unparseable field"))
}

fn invalid_cursor(reason: &str) -> Error {
    Error::Query(QueryError::InvalidParameter {
        parameter: "cursor".to_string(),
        reason: format!("malformed continuation token: {reason}"),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Build a [`ChangeRecord`] for a single committed version if it passes all filters.
///
/// Shared by the node and edge scans — they differ only in id accessor and [`EntityKind`].
/// Returns `None` when the version falls outside the transaction-time window, the optional
/// valid-time window, or the optional label filter.
///
/// Tombstone (deletion) handling under a valid-time filter: a deletion's valid-time is an
/// empty range `[v, v)`, which never *overlaps* any window. We therefore treat the deletion
/// instant as a point and include the tombstone iff that instant lies within the valid window.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_change_record(
    entity_id: u64,
    kind: EntityKind,
    temporal: &BiTemporalInterval,
    label_id: InternedString,
    prev_is_none: bool,
    tx_window: &TimeRange,
    valid_window: Option<&TimeRange>,
    label_filter: Option<&str>,
) -> Option<ChangeRecord> {
    let tx_range = temporal.transaction_time();
    // Transaction-time window is half-open [t1, t2).
    if !tx_window.contains(tx_range.start()) {
        return None;
    }

    let valid_range = temporal.valid_time();
    let is_deletion = valid_range.is_empty();

    if let Some(vw) = valid_window {
        let matches = if is_deletion {
            // Treat the deletion instant as a point within the window.
            vw.contains(valid_range.start())
        } else {
            valid_range.overlaps(vw)
        };
        if !matches {
            return None;
        }
    }

    if let Some(filter) = label_filter
        && GLOBAL_INTERNER.resolve_with(label_id, |s| s == filter) != Some(true)
    {
        return None;
    }

    let change_type = if is_deletion {
        ChangeType::Deleted
    } else if prev_is_none {
        ChangeType::Created
    } else {
        ChangeType::Modified
    };

    let label = GLOBAL_INTERNER
        .resolve_with(label_id, |s| s.to_string())
        .unwrap_or_default();

    Some(ChangeRecord {
        entity_id,
        kind,
        change_type,
        label,
        transaction_time: tx_range.start(),
        transaction_time_range: tx_range,
        valid_time_range: valid_range,
    })
}

/// Query options for [`crate::db::AletheiaDB::list_changes`].
///
/// The transaction-time window is required; everything else is optional. An empty window
/// (`tx_from == tx_to`) is valid and yields an empty page (it is not an error). A window with
/// `tx_from > tx_to` is rejected with `TemporalError::InvalidTimeRange`.
#[derive(Debug, Clone)]
pub struct ChangeFeedQuery {
    /// Inclusive lower bound of the transaction-time window.
    pub tx_from: Timestamp,
    /// Exclusive upper bound of the transaction-time window.
    pub tx_to: Timestamp,
    /// Optional inclusive lower bound of a valid-time constraint. Must be paired with
    /// [`ChangeFeedQuery::valid_to`].
    pub valid_from: Option<Timestamp>,
    /// Optional exclusive upper bound of a valid-time constraint. Must be paired with
    /// [`ChangeFeedQuery::valid_from`].
    pub valid_to: Option<Timestamp>,
    /// Optional node-label / edge-type filter (exact string match).
    pub label: Option<String>,
    /// Maximum number of rows to return in this page.
    pub limit: usize,
    /// Opaque continuation token from a previous page's `next_cursor` (`None` = first page).
    pub cursor: Option<String>,
}

impl ChangeFeedQuery {
    /// Construct a query over a transaction-time window with default options
    /// (no valid-time or label filter, given `limit`, first page).
    pub fn new(tx_from: Timestamp, tx_to: Timestamp, limit: usize) -> Self {
        ChangeFeedQuery {
            tx_from,
            tx_to,
            valid_from: None,
            valid_to: None,
            label: None,
            limit,
            cursor: None,
        }
    }
}

/// A bounded page of changefeed results.
#[derive(Debug, Clone)]
pub struct ChangeFeedPage {
    /// The change rows for this page, in deterministic ascending order.
    pub changes: Vec<ChangeRecord>,
    /// Continuation token for the next page, or `None` when this is the last page.
    pub next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_kind_ord_and_str() {
        assert_eq!(EntityKind::Node.ord(), 0);
        assert_eq!(EntityKind::Edge.ord(), 1);
        assert!(EntityKind::Node.ord() < EntityKind::Edge.ord());
        assert_eq!(EntityKind::Node.as_str(), "node");
        assert_eq!(EntityKind::Edge.as_str(), "edge");
    }

    #[test]
    fn change_type_str() {
        assert_eq!(ChangeType::Created.as_str(), "created");
        assert_eq!(ChangeType::Modified.as_str(), "modified");
        assert_eq!(ChangeType::Deleted.as_str(), "deleted");
    }

    #[test]
    fn cursor_round_trips() {
        let c = ChangeCursor {
            tx_wallclock: 1_700_000_000_000_000,
            tx_logical: 7,
            kind_ord: 1,
            entity_id: 42,
        };
        let token = c.encode();
        let decoded = ChangeCursor::decode(&token).expect("round-trip should succeed");
        assert_eq!(c, decoded);
    }

    #[test]
    fn cursor_token_is_opaque_hex() {
        let c = ChangeCursor {
            tx_wallclock: 10,
            tx_logical: 0,
            kind_ord: 0,
            entity_id: 1,
        };
        let token = c.encode();
        // Hex-only alphabet: no ':' or digits-with-separators leaking through.
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn cursor_ordering_is_total_and_correct() {
        let a = ChangeCursor {
            tx_wallclock: 10,
            tx_logical: 0,
            kind_ord: 0,
            entity_id: 5,
        };
        // Later tx-time dominates everything else.
        let b = ChangeCursor {
            tx_wallclock: 11,
            tx_logical: 0,
            kind_ord: 0,
            entity_id: 1,
        };
        // Same tx-time, edge (kind_ord 1) after node (kind_ord 0).
        let c = ChangeCursor {
            tx_wallclock: 10,
            tx_logical: 0,
            kind_ord: 1,
            entity_id: 1,
        };
        // Same tx-time + kind, higher id is later.
        let d = ChangeCursor {
            tx_wallclock: 10,
            tx_logical: 0,
            kind_ord: 0,
            entity_id: 6,
        };
        assert!(a < b);
        assert!(a < c);
        assert!(a < d);
        assert!(d < c);
    }

    #[test]
    fn decode_rejects_malformed_tokens() {
        // Not hex.
        assert!(ChangeCursor::decode("zzzz").is_err());
        // Odd length.
        assert!(ChangeCursor::decode("abc").is_err());
        // Valid hex but wrong field count ("1:2" -> "31 3a 32").
        assert!(ChangeCursor::decode(&hex_encode(b"1:2")).is_err());
        // Valid hex, right field count, but a non-numeric field.
        assert!(ChangeCursor::decode(&hex_encode(b"1:2:0:x")).is_err());
    }
}
