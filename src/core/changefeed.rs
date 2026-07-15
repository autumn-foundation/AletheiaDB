//! Graph-wide temporal changefeed types (Issue #3216).
//!
//! These types back [`crate::db::AletheiaDB::list_changes`], a read-only API that
//! enumerates the entities (nodes **and** edges) whose versions were committed within a
//! transaction-time window `[t1, t2)`. This is the *discovery* half of the bi-temporal
//! story: callers can ask "what changed between T1 and T2?" without already knowing any
//! entity IDs, then drill into each result with the existing per-entity history/diff APIs.
//!
//! The feed is **bounded** (a `limit` plus a stable, opaque continuation cursor) and
//! **deterministically ordered** (transaction-time ascending, then entity kind, id, and
//! version) so that paginated reads are stable and replayable.

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

    /// Inverse of [`EntityKind::ord`] for values produced by this crate (0 = node, else edge).
    #[inline]
    pub(crate) const fn from_ord(ord: u8) -> EntityKind {
        if ord == 0 {
            EntityKind::Node
        } else {
            EntityKind::Edge
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
    /// Version id of the specific version this row describes (use with the per-entity
    /// history/diff APIs to drill in).
    pub version_id: u64,
    /// Whether this is a node or an edge.
    pub kind: EntityKind,
    /// How the entity changed at this version.
    pub change_type: ChangeType,
    /// Resolved node label / edge type.
    pub label: String,
    /// Full transaction-time range of the version.
    pub transaction_time_range: TimeRange,
    /// Full valid-time range of the version (an empty range denotes a deletion).
    pub valid_time_range: TimeRange,
}

impl ChangeRecord {
    /// Commit timestamp of this version (== `transaction_time_range.start()`).
    ///
    /// Derived rather than stored so it can never disagree with the range it comes from.
    #[inline]
    pub fn transaction_time(&self) -> Timestamp {
        self.transaction_time_range.start()
    }

    /// The stable total-order [`ChangeCursor`] identifying this record.
    ///
    /// This is the dedup / resume key for the push changefeed (Issue #3375): two
    /// records are the same committed fact iff their cursors are equal, and a
    /// consumer resumes a pull with `list_changes(cursor = last_delivered.cursor())`.
    /// Derived from the record's own fields so it always agrees with what the
    /// #3216 scan would have produced for the same version.
    #[inline]
    pub(crate) fn cursor(&self) -> ChangeCursor {
        let start = self.transaction_time_range.start();
        ChangeCursor {
            tx_wallclock: start.wallclock(),
            tx_logical: start.logical(),
            kind_ord: self.kind.ord(),
            entity_id: self.entity_id,
            version_id: self.version_id,
        }
    }
}

/// Total-order sort key used for deterministic ordering and cursor pagination.
///
/// Ordering is by `(tx_wallclock, tx_logical, kind_ord, entity_id, version_id)` ascending.
/// `version_id` is included as the final component so the key is **unique per emitted version**
/// even when one transaction commits two versions of the same entity (which share a commit
/// timestamp): without it, two such rows would collide and one would be silently dropped at a
/// page boundary by the strict `> cursor` resume rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ChangeCursor {
    pub tx_wallclock: i64,
    pub tx_logical: u32,
    pub kind_ord: u8,
    pub entity_id: u64,
    pub version_id: u64,
}

impl ChangeCursor {
    /// Encode this key as an opaque, URL-safe continuation token.
    ///
    /// The underlying form is a delimited string hex-encoded so clients treat it as
    /// opaque and never attempt to parse or construct it by hand.
    pub(crate) fn encode(&self) -> String {
        let raw = format!(
            "{}:{}:{}:{}:{}",
            self.tx_wallclock, self.tx_logical, self.kind_ord, self.entity_id, self.version_id
        );
        crate::core::hex::encode(raw.as_bytes())
    }

    /// Decode an opaque continuation token produced by [`ChangeCursor::encode`].
    ///
    /// Returns a `QueryError::InvalidParameter` (never panics) when the token is malformed.
    pub(crate) fn decode(token: &str) -> Result<Self, Error> {
        let bytes =
            crate::core::hex::decode(token).ok_or_else(|| invalid_cursor("not valid hex"))?;
        let raw = std::str::from_utf8(&bytes).map_err(|_| invalid_cursor("not valid utf-8"))?;
        let mut parts = raw.split(':');
        let tx_wallclock = parse_field(parts.next())?;
        let tx_logical = parse_field(parts.next())?;
        let kind_ord = parse_field(parts.next())?;
        let entity_id = parse_field(parts.next())?;
        let version_id = parse_field(parts.next())?;
        if parts.next().is_some() {
            return Err(invalid_cursor("too many fields"));
        }
        Ok(ChangeCursor {
            tx_wallclock,
            tx_logical,
            kind_ord,
            entity_id,
            version_id,
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

/// A changefeed row in its cheap, pre-materialization form.
///
/// This is what the storage scan produces: it carries the precomputed sort [`ChangeCursor`] and
/// the label as an [`InternedString`] (no owned `String`). The expensive label resolution is
/// deferred to [`RawChange::into_record`], which the query layer calls only for the rows that
/// actually survive cursor filtering and the page `limit` — so a wide window over a large store
/// never allocates a label per matching version just to discard it.
#[derive(Debug, Clone)]
pub(crate) struct RawChange {
    pub cursor: ChangeCursor,
    pub change_type: ChangeType,
    pub label_id: InternedString,
    pub transaction_time_range: TimeRange,
    pub valid_time_range: TimeRange,
}

impl RawChange {
    /// Resolve the label and materialize a public [`ChangeRecord`].
    pub(crate) fn into_record(self) -> ChangeRecord {
        let label = GLOBAL_INTERNER
            .resolve_with(self.label_id, |s| s.to_string())
            .unwrap_or_default();
        ChangeRecord {
            entity_id: self.cursor.entity_id,
            version_id: self.cursor.version_id,
            kind: EntityKind::from_ord(self.cursor.kind_ord),
            change_type: self.change_type,
            label,
            transaction_time_range: self.transaction_time_range,
            valid_time_range: self.valid_time_range,
        }
    }
}

/// Build a [`RawChange`] for a single committed version if it passes all filters.
///
/// Shared by the node and edge scans (hot and cold tiers) — they differ only in id accessor
/// and [`EntityKind`]. Returns `None` when the version falls outside the transaction-time
/// window, the optional valid-time window, or the optional label filter.
///
/// Valid-time filtering treats a version's *valid extent* and the window as sets and includes
/// the version iff they intersect. A live version has a non-empty extent, so this is a range
/// overlap. A deletion (tombstone) has an empty valid range `[v, v)` — a degenerate point at
/// the deletion instant `v` — so it intersects the half-open window iff the window contains
/// that single instant. The two predicates are the same intersection rule applied to a range
/// vs. a point.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_raw_change(
    version_id: u64,
    entity_id: u64,
    kind: EntityKind,
    temporal: &BiTemporalInterval,
    label_id: InternedString,
    prev_is_none: bool,
    tx_window: &TimeRange,
    valid_window: Option<&TimeRange>,
    label_filter: Option<&str>,
) -> Option<RawChange> {
    let tx_range = temporal.transaction_time();
    // Transaction-time window is half-open [t1, t2).
    if !tx_window.contains(tx_range.start()) {
        return None;
    }

    let valid_range = temporal.valid_time();
    let is_deletion = valid_range.is_empty();

    if let Some(vw) = valid_window {
        // Intersection of the version's valid extent with the window (tombstone = point).
        let intersects = if is_deletion {
            vw.contains(valid_range.start())
        } else {
            valid_range.overlaps(vw)
        };
        if !intersects {
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

    Some(RawChange {
        cursor: ChangeCursor {
            tx_wallclock: tx_range.start().wallclock(),
            tx_logical: tx_range.start().logical(),
            kind_ord: kind.ord(),
            entity_id,
            version_id,
        },
        change_type,
        label_id,
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
    /// Maximum number of rows to return in this page. A value of `0` is treated as `1`
    /// (a page must be able to carry a continuation cursor, so it cannot be empty when more
    /// rows exist).
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

    fn cur(tx: i64, logical: u32, kind_ord: u8, entity_id: u64, version_id: u64) -> ChangeCursor {
        ChangeCursor {
            tx_wallclock: tx,
            tx_logical: logical,
            kind_ord,
            entity_id,
            version_id,
        }
    }

    #[test]
    fn entity_kind_ord_and_str() {
        assert_eq!(EntityKind::Node.ord(), 0);
        assert_eq!(EntityKind::Edge.ord(), 1);
        assert!(EntityKind::Node.ord() < EntityKind::Edge.ord());
        assert_eq!(EntityKind::Node.as_str(), "node");
        assert_eq!(EntityKind::Edge.as_str(), "edge");
        assert_eq!(EntityKind::from_ord(0), EntityKind::Node);
        assert_eq!(EntityKind::from_ord(1), EntityKind::Edge);
    }

    #[test]
    fn change_type_str() {
        assert_eq!(ChangeType::Created.as_str(), "created");
        assert_eq!(ChangeType::Modified.as_str(), "modified");
        assert_eq!(ChangeType::Deleted.as_str(), "deleted");
    }

    #[test]
    fn cursor_round_trips() {
        let c = cur(1_700_000_000_000_000, 7, 1, 42, 99);
        let token = c.encode();
        let decoded = ChangeCursor::decode(&token).expect("round-trip should succeed");
        assert_eq!(c, decoded);
    }

    #[test]
    fn cursor_token_is_opaque_hex() {
        let token = cur(10, 0, 0, 1, 1).encode();
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn cursor_ordering_is_total_and_correct() {
        let a = cur(10, 0, 0, 5, 1);
        // Later tx-time dominates everything else.
        let b = cur(11, 0, 0, 1, 1);
        // Same tx-time, edge (kind_ord 1) after node (kind_ord 0).
        let c = cur(10, 0, 1, 1, 1);
        // Same tx-time + kind, higher id is later.
        let d = cur(10, 0, 0, 6, 1);
        // Same tx-time + kind + entity, higher version_id is later (the collision tiebreak).
        let e = cur(10, 0, 0, 5, 2);
        assert!(a < b);
        assert!(a < c);
        assert!(a < d);
        assert!(d < c);
        assert!(a < e);
        assert!(e < d);
    }

    #[test]
    fn decode_rejects_malformed_tokens() {
        // Not hex.
        assert!(ChangeCursor::decode("zzzz").is_err());
        // Odd length.
        assert!(ChangeCursor::decode("abc").is_err());
        // Valid hex but too few fields ("1:2:0:3" -> 4 fields, need 5).
        assert!(ChangeCursor::decode(&crate::core::hex::encode(b"1:2:0:3")).is_err());
        // Valid hex, right field count, but a non-numeric field.
        assert!(ChangeCursor::decode(&crate::core::hex::encode(b"1:2:0:3:x")).is_err());
    }
}
