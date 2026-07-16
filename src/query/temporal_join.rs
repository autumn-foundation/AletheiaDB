//! Pure interval math for AQL temporal joins (Issue #3379).
//!
//! This module holds the *storage-free* semantic core behind AQL temporal
//! joins: given each participating entity's reconstructed **valid-time**
//! timeline (as believed at a pinned transaction time), it aligns them at
//! matching valid-time coordinates over a range, in one of two modes:
//!
//! * **event-aligned** ([`AlignMode::Events`]) — the SQL `ASOF JOIN` analog.
//!   For each version-change instant of a *driver* entity within the range,
//!   every participant is sampled at "its most recent state at or before that
//!   instant". One output row per driver event.
//! * **interval-overlap** ([`AlignMode::Overlap`]) — the piecewise maximal
//!   sub-intervals over the range during which *all* participants' states (and
//!   any gating edge) co-held constant, i.e. the intersection of the
//!   participating valid intervals. One output row per non-empty sub-interval.
//!
//! Keeping this logic separate from the executor iterator lets the subtle
//! half-open interval-boundary rules be exhaustively unit-tested without a
//! database — the CI correctness fixture (Issue #3379 AC6) lives in this
//! module's tests.
//!
//! # Boundary contract (falsifiable)
//!
//! All ranges and presence intervals are **half-open** `[from, to)`, matching
//! the storage model and the #3363 temporal-window convention. A change-point
//! or edge interval whose `valid_from` equals an alignment instant *starts*
//! that instant and is therefore **included** at it (a version does not
//! contribute to an instant strictly before its `valid_from`). A version at
//! exactly `to` is excluded from the range. No output row ever pairs states
//! that did not co-hold under this rule.

use crate::core::property::PropertyMap;

/// Hard cap on the number of alignment instants (event mode) or sub-interval
/// boundaries (overlap mode) a single join may generate, guarding against a
/// caller pairing a huge range with highly volatile entities.
pub const MAX_ALIGN_INSTANTS: usize = 100_000;

/// The temporal-join alignment mode (Issue #3379).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignMode {
    /// Event-aligned (ASOF): sample participants at each driver change-point.
    Events,
    /// Interval-overlap: piecewise sub-intervals where all states co-held.
    Overlap,
}

/// One believed change-point in an entity's valid-time timeline: the entity's
/// reconstructed state (`properties`) holds from `valid_from` until the next
/// change-point (or forever, for the last one).
#[derive(Debug, Clone)]
pub struct ChangePoint {
    /// Valid-time interval start, microseconds since epoch.
    pub valid_from: i64,
    /// The version's reconstructed properties at this change-point.
    pub properties: PropertyMap,
}

/// A believed valid-time timeline for one participant entity: its change-points
/// sorted ascending and deduplicated by `valid_from` (latest belief wins — the
/// caller resolves same-instant supersession before constructing this).
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    /// Ascending, `valid_from`-deduplicated change-points.
    pub changes: Vec<ChangePoint>,
}

impl Timeline {
    /// Build a timeline from unsorted change-points, sorting ascending by
    /// `valid_from`. Callers must have already resolved same-`valid_from`
    /// supersession (see the executor's believed-version reconstruction).
    #[must_use]
    pub fn from_changes(mut changes: Vec<ChangePoint>) -> Self {
        changes.sort_by_key(|c| c.valid_from);
        Timeline { changes }
    }

    /// Index of the most recent change-point at or before `instant`
    /// (`valid_from <= instant`), or `None` if the entity did not yet exist.
    #[must_use]
    pub fn sample_index_at(&self, instant: i64) -> Option<usize> {
        // changes is ascending; find the last one with valid_from <= instant.
        self.changes.iter().rposition(|c| c.valid_from <= instant)
    }
}

/// A set of believed half-open valid intervals `[from, to)` during which a
/// gating edge is present (`to == i64::MAX` for an open interval). Used to gate
/// a participant that is reached via a relationship whose own validity changes
/// over the range (Issue #3379 AC2: edge validity honored at each instant).
#[derive(Debug, Clone, Default)]
pub struct PresenceIntervals {
    /// Half-open `[from, to)` intervals; may overlap/abut (union semantics).
    pub intervals: Vec<(i64, i64)>,
}

impl PresenceIntervals {
    /// Whether the gate is open at `instant` (some interval contains it).
    #[must_use]
    pub fn present_at(&self, instant: i64) -> bool {
        self.intervals
            .iter()
            .any(|(s, e)| instant >= *s && instant < *e)
    }
}

/// A participant in a temporal join: its own state timeline plus an optional
/// connecting-edge presence gate. The participant is considered *present* at an
/// instant iff it has a state sample there **and** its gate (if any) is open.
#[derive(Debug, Clone)]
pub struct Participant {
    /// The participant's believed valid-time state timeline.
    pub timeline: Timeline,
    /// Optional connecting-edge validity gate. `None` means ungated (e.g. the
    /// driver itself, or a participant bound by explicit id).
    pub gate: Option<PresenceIntervals>,
}

impl Participant {
    /// Construct an ungated participant from a timeline.
    #[must_use]
    pub fn ungated(timeline: Timeline) -> Self {
        Participant {
            timeline,
            gate: None,
        }
    }

    /// Whether the gate (if any) is open at `instant`.
    fn gate_open_at(&self, instant: i64) -> bool {
        self.gate.as_ref().is_none_or(|g| g.present_at(instant))
    }

    /// The sampled change-point index at `instant`, honoring the gate: `None`
    /// if the entity has no state at/before `instant` or the gate is closed.
    #[must_use]
    pub fn sample_index_at(&self, instant: i64) -> Option<usize> {
        if self.gate_open_at(instant) {
            self.timeline.sample_index_at(instant)
        } else {
            None
        }
    }

    /// Whether the participant is present (state + gate) at `instant`.
    #[must_use]
    pub fn present_at(&self, instant: i64) -> bool {
        self.sample_index_at(instant).is_some()
    }
}

/// The alignment coordinate stamped on each output row (Issue #3379 AC4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignCoordinate {
    /// Event-aligned: the single valid-time instant (microseconds since epoch).
    Instant(i64),
    /// Interval-overlap: the half-open `[from, to)` sub-interval it holds over.
    Interval {
        /// Inclusive start (microseconds since epoch).
        from: i64,
        /// Exclusive end (microseconds since epoch).
        to: i64,
    },
}

/// One aligned output row: the alignment coordinate plus, for each participant
/// (in participant order), the index of its sampled change-point, or `None`
/// when the participant was absent at that coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlignedRow {
    /// The valid-time coordinate this row corresponds to.
    pub coordinate: AlignCoordinate,
    /// Per-participant sampled change-point index (`None` == absent).
    pub sample_index: Vec<Option<usize>>,
}

/// Errors from alignment. Mapped by the converter/executor to structured
/// [`crate::core::error::QueryError`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlignError {
    /// `to <= from`.
    EmptyRange,
    /// No participants were supplied.
    NoParticipants,
    /// The driver index is out of range for event-aligned mode.
    DriverOutOfRange,
    /// The range/volatility pair would generate more than
    /// [`MAX_ALIGN_INSTANTS`] instants or boundaries.
    TooManyInstants(usize),
}

/// Event-aligned join (Issue #3379, [`AlignMode::Events`]).
///
/// Emits one row per distinct driver change-point instant `T` with
/// `from <= T < to`, sampling every participant at its most recent state at or
/// before `T` (`None` == absent). The driver is always present at its own
/// event. Rows are in ascending instant order.
pub fn align_events(
    participants: &[Participant],
    driver: usize,
    from: i64,
    to: i64,
) -> Result<Vec<AlignedRow>, AlignError> {
    if participants.is_empty() {
        return Err(AlignError::NoParticipants);
    }
    if driver >= participants.len() {
        return Err(AlignError::DriverOutOfRange);
    }
    if to <= from {
        return Err(AlignError::EmptyRange);
    }

    // Distinct driver change-point instants in [from, to), ascending. The
    // driver timeline is already sorted+deduped, so a linear pass suffices.
    let mut instants: Vec<i64> = Vec::new();
    for cp in &participants[driver].timeline.changes {
        if cp.valid_from >= from && cp.valid_from < to {
            if instants.last() != Some(&cp.valid_from) {
                instants.push(cp.valid_from);
            }
            if instants.len() > MAX_ALIGN_INSTANTS {
                return Err(AlignError::TooManyInstants(instants.len()));
            }
        }
    }

    let rows = instants
        .into_iter()
        .map(|t| AlignedRow {
            coordinate: AlignCoordinate::Instant(t),
            sample_index: participants.iter().map(|p| p.sample_index_at(t)).collect(),
        })
        .collect();
    Ok(rows)
}

/// Interval-overlap join (Issue #3379, [`AlignMode::Overlap`]).
///
/// Computes the piecewise maximal sub-intervals tiling `[from, to)` at the
/// union of all participants' change-points and gating-edge boundaries within
/// the range, and emits one row per sub-interval **over which every participant
/// (state + gate) co-held** — sub-intervals where any participant is absent are
/// the empty-overlap gaps and are dropped. Each participant is sampled at the
/// sub-interval's left endpoint (constant across it by construction). Rows are
/// in ascending interval order.
pub fn align_overlap(
    participants: &[Participant],
    from: i64,
    to: i64,
) -> Result<Vec<AlignedRow>, AlignError> {
    if participants.is_empty() {
        return Err(AlignError::NoParticipants);
    }
    if to <= from {
        return Err(AlignError::EmptyRange);
    }

    // Boundary set: `from`, every participant change-point in [from, to), and
    // every gating-edge interval boundary in (from, to). A BTreeSet keeps them
    // sorted and unique.
    let mut boundaries: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    boundaries.insert(from);
    for p in participants {
        for cp in &p.timeline.changes {
            if cp.valid_from >= from && cp.valid_from < to {
                boundaries.insert(cp.valid_from);
            }
        }
        if let Some(gate) = &p.gate {
            for (s, e) in &gate.intervals {
                if *s > from && *s < to {
                    boundaries.insert(*s);
                }
                if *e > from && *e < to {
                    boundaries.insert(*e);
                }
            }
        }
        if boundaries.len() > MAX_ALIGN_INSTANTS {
            return Err(AlignError::TooManyInstants(boundaries.len()));
        }
    }

    // Consecutive boundaries form sub-intervals [b_j, b_{j+1}); the final
    // sub-interval's end is `to`.
    let starts: Vec<i64> = boundaries.into_iter().collect();
    let mut rows = Vec::new();
    for (j, &s) in starts.iter().enumerate() {
        let e = starts.get(j + 1).copied().unwrap_or(to);
        // Sample every participant at the left endpoint; keep the sub-interval
        // only if ALL are present (co-hold), per the overlap contract.
        let sample_index: Vec<Option<usize>> =
            participants.iter().map(|p| p.sample_index_at(s)).collect();
        if sample_index.iter().all(Option::is_some) {
            rows.push(AlignedRow {
                coordinate: AlignCoordinate::Interval { from: s, to: e },
                sample_index,
            });
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::query::temporal_window::parse_boundary_micros;

    fn t(rfc: &str) -> i64 {
        parse_boundary_micros(rfc).unwrap()
    }

    /// Build a timeline of `(valid_from_rfc, tier_int)` change-points.
    fn timeline(points: &[(&str, i64)]) -> Timeline {
        let changes = points
            .iter()
            .map(|(vf, tier)| ChangePoint {
                valid_from: t(vf),
                properties: PropertyMapBuilder::new().insert("v", *tier).build(),
            })
            .collect();
        Timeline::from_changes(changes)
    }

    fn v_at(p: &Participant, idx: Option<usize>) -> Option<i64> {
        let i = idx?;
        match p.timeline.changes[i].properties.get("v") {
            Some(crate::core::property::PropertyValue::Int(n)) => Some(*n),
            _ => None,
        }
    }

    #[test]
    fn sample_index_is_most_recent_at_or_before() {
        let tl = timeline(&[("2024-01-01", 1), ("2024-03-01", 2), ("2024-06-01", 3)]);
        // Before first change-point: absent.
        assert_eq!(tl.sample_index_at(t("2023-12-31")), None);
        // Exactly at a change-point: included (half-open start rule).
        assert_eq!(tl.sample_index_at(t("2024-03-01")), Some(1));
        // Between change-points: the earlier one holds.
        assert_eq!(tl.sample_index_at(t("2024-04-15")), Some(1));
        // After the last: the last holds forever.
        assert_eq!(tl.sample_index_at(t("2030-01-01")), Some(2));
    }

    #[test]
    fn events_one_row_per_driver_change_point_in_range() {
        // Driver (order) has placements; customer tier changes independently.
        let driver = Participant::ungated(timeline(&[
            ("2024-02-01", 10),
            ("2024-05-01", 20),
            ("2024-09-01", 30),
        ]));
        let customer = Participant::ungated(timeline(&[
            ("2024-01-01", 1), // tier 1
            ("2024-06-01", 2), // upgraded to tier 2
        ]));
        let parts = vec![driver, customer];
        let rows = align_events(&parts, 0, t("2024-01-01"), t("2025-01-01")).unwrap();
        // 3 driver events.
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[0].coordinate,
            AlignCoordinate::Instant(t("2024-02-01"))
        );
        assert_eq!(
            rows[1].coordinate,
            AlignCoordinate::Instant(t("2024-05-01"))
        );
        assert_eq!(
            rows[2].coordinate,
            AlignCoordinate::Instant(t("2024-09-01"))
        );
        // Customer tier AT each order instant:
        //  2024-02-01 -> tier 1 (before upgrade)
        //  2024-05-01 -> tier 1 (still before upgrade on 2024-06-01)
        //  2024-09-01 -> tier 2 (after upgrade)
        assert_eq!(v_at(&parts[1], rows[0].sample_index[1]), Some(1));
        assert_eq!(v_at(&parts[1], rows[1].sample_index[1]), Some(1));
        assert_eq!(v_at(&parts[1], rows[2].sample_index[1]), Some(2));
    }

    #[test]
    fn events_participant_absent_before_it_existed_is_null() {
        // Order placed before the customer record existed -> customer absent.
        let driver = Participant::ungated(timeline(&[("2024-01-05", 10)]));
        let customer = Participant::ungated(timeline(&[("2024-02-01", 1)]));
        let parts = vec![driver, customer];
        let rows = align_events(&parts, 0, t("2024-01-01"), t("2024-12-01")).unwrap();
        assert_eq!(rows.len(), 1);
        // Driver present at its own event; customer absent (None).
        assert!(rows[0].sample_index[0].is_some());
        assert_eq!(rows[0].sample_index[1], None);
    }

    #[test]
    fn events_range_boundaries_half_open() {
        // Change-points exactly at `from` (included) and exactly at `to` (excluded).
        let driver = Participant::ungated(timeline(&[
            ("2024-01-01", 1), // == from  -> included
            ("2024-06-01", 2), // interior -> included
            ("2025-01-01", 3), // == to    -> excluded
        ]));
        let parts = vec![driver];
        let rows = align_events(&parts, 0, t("2024-01-01"), t("2025-01-01")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].coordinate,
            AlignCoordinate::Instant(t("2024-01-01"))
        );
        assert_eq!(
            rows[1].coordinate,
            AlignCoordinate::Instant(t("2024-06-01"))
        );
    }

    #[test]
    fn events_edge_gate_absent_when_edge_not_valid() {
        // Customer reached via an edge valid only [2024-03-01, 2024-08-01).
        let driver = Participant::ungated(timeline(&[
            ("2024-02-01", 10), // edge not yet valid -> customer absent
            ("2024-05-01", 20), // edge valid          -> customer present
            ("2024-09-01", 30), // edge expired        -> customer absent
        ]));
        let customer = Participant {
            timeline: timeline(&[("2024-01-01", 7)]),
            gate: Some(PresenceIntervals {
                intervals: vec![(t("2024-03-01"), t("2024-08-01"))],
            }),
        };
        let parts = vec![driver, customer];
        let rows = align_events(&parts, 0, t("2024-01-01"), t("2025-01-01")).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].sample_index[1], None); // edge not valid yet
        assert!(rows[1].sample_index[1].is_some()); // edge valid
        assert_eq!(rows[2].sample_index[1], None); // edge expired
    }

    #[test]
    fn overlap_piecewise_intersection_of_two_timelines() {
        // A: [Jan..Apr)=a1, [Apr..)=a2 ; B: [Feb..)=b1
        let a = Participant::ungated(timeline(&[("2024-01-01", 1), ("2024-04-01", 2)]));
        let b = Participant::ungated(timeline(&[("2024-02-01", 10)]));
        let parts = vec![a, b];
        let rows = align_overlap(&parts, t("2024-01-01"), t("2024-06-01")).unwrap();
        // Boundaries in range: from=Jan01, Feb01 (B), Apr01 (A). Sub-intervals:
        //  [Jan01,Feb01) -> B absent -> DROPPED
        //  [Feb01,Apr01) -> a1 & b1  -> kept
        //  [Apr01,Jun01) -> a2 & b1  -> kept
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].coordinate,
            AlignCoordinate::Interval {
                from: t("2024-02-01"),
                to: t("2024-04-01")
            }
        );
        assert_eq!(v_at(&parts[0], rows[0].sample_index[0]), Some(1));
        assert_eq!(v_at(&parts[1], rows[0].sample_index[1]), Some(10));
        assert_eq!(
            rows[1].coordinate,
            AlignCoordinate::Interval {
                from: t("2024-04-01"),
                to: t("2024-06-01")
            }
        );
        assert_eq!(v_at(&parts[0], rows[1].sample_index[0]), Some(2));
    }

    #[test]
    fn overlap_empty_when_no_co_hold_gap() {
        // A present only early, B present only late: no overlap at all.
        let a = Participant::ungated(timeline(&[("2024-01-01", 1)]));
        let mut a_gated = a;
        a_gated.gate = Some(PresenceIntervals {
            intervals: vec![(t("2024-01-01"), t("2024-03-01"))],
        });
        let b = Participant {
            timeline: timeline(&[("2024-05-01", 2)]),
            gate: None,
        };
        let parts = vec![a_gated, b];
        let rows = align_overlap(&parts, t("2024-01-01"), t("2024-12-01")).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn overlap_edge_appears_and_disappears_mid_range() {
        // Product price + supplier rating co-held only while the SUPPLIES edge
        // is valid [Mar..Aug).
        let product = Participant::ungated(timeline(&[("2024-01-01", 100)]));
        let supplier = Participant {
            timeline: timeline(&[("2024-01-01", 5)]),
            gate: Some(PresenceIntervals {
                intervals: vec![(t("2024-03-01"), t("2024-08-01"))],
            }),
        };
        let parts = vec![product, supplier];
        let rows = align_overlap(&parts, t("2024-01-01"), t("2024-12-01")).unwrap();
        // Only one co-hold sub-interval: exactly the edge's validity window.
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].coordinate,
            AlignCoordinate::Interval {
                from: t("2024-03-01"),
                to: t("2024-08-01")
            }
        );
    }

    #[test]
    fn overlap_adjacent_versions_tile_without_gap_or_overlap() {
        let a = Participant::ungated(timeline(&[
            ("2024-01-01", 1),
            ("2024-02-01", 2),
            ("2024-03-01", 3),
        ]));
        let parts = vec![a];
        let rows = align_overlap(&parts, t("2024-01-01"), t("2024-04-01")).unwrap();
        assert_eq!(rows.len(), 3);
        // Contiguous: each end == next start, union == [from,to).
        assert_eq!(
            rows[0].coordinate,
            AlignCoordinate::Interval {
                from: t("2024-01-01"),
                to: t("2024-02-01")
            }
        );
        assert_eq!(
            rows[2].coordinate,
            AlignCoordinate::Interval {
                from: t("2024-03-01"),
                to: t("2024-04-01")
            }
        );
    }

    #[test]
    fn errors_empty_range_and_no_participants_and_driver_oob() {
        let p = Participant::ungated(timeline(&[("2024-01-01", 1)]));
        let one = std::slice::from_ref(&p);
        assert_eq!(
            align_events(one, 0, t("2024-02-01"), t("2024-01-01")),
            Err(AlignError::EmptyRange)
        );
        assert_eq!(
            align_events(&[], 0, t("2024-01-01"), t("2024-02-01")),
            Err(AlignError::NoParticipants)
        );
        assert_eq!(
            align_events(one, 5, t("2024-01-01"), t("2024-02-01")),
            Err(AlignError::DriverOutOfRange)
        );
        assert_eq!(
            align_overlap(&[], t("2024-01-01"), t("2024-02-01")),
            Err(AlignError::NoParticipants)
        );
    }

    #[test]
    fn events_same_valid_from_deduplicated_to_one_instant() {
        // Two driver change-points share a valid_from (a same-instant
        // correction already resolved) -> one event instant.
        let driver = Participant::ungated(Timeline {
            changes: vec![
                ChangePoint {
                    valid_from: t("2024-03-01"),
                    properties: PropertyMapBuilder::new().insert("v", 1).build(),
                },
                ChangePoint {
                    valid_from: t("2024-03-01"),
                    properties: PropertyMapBuilder::new().insert("v", 2).build(),
                },
            ],
        });
        let rows = align_events(&[driver], 0, t("2024-01-01"), t("2024-12-01")).unwrap();
        assert_eq!(rows.len(), 1);
    }
}
