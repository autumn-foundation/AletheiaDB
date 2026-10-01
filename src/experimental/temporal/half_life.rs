//! Knowledge half-life analytics — survival analysis over fact volatility (Issue #3377).
//!
//! Every knowledge base silently decays. A person's `email` changes every couple
//! of years; a company's `stock_price` changes hourly; a country's `capital`
//! almost never. AletheiaDB is the only engine that records the raw material to
//! measure that decay: every fact's full supersession history across valid and
//! transaction time. **Knowledge half-life analytics** performs *survival
//! analysis* over that history — per node label, per edge type, per property —
//! to answer *how long does a fact of this kind survive before it is
//! superseded?* From that one measurement fall out three read-only products:
//!
//! - **Volatility statistics** ([`VolatilityStats`] via [`AletheiaDB::knowledge_half_life`]):
//!   the median survival time ("half-life"), a dispersion measure, and
//!   observation/event/censored counts for a cohort.
//! - **Freshness scores** ([`FreshnessScore`] via [`AletheiaDB::fact_freshness`]):
//!   a single fact's age expressed in cohort half-lives plus an estimated
//!   survival probability — "this `email` was recorded 3.2 half-lives ago; treat
//!   as probably stale".
//! - **Staleness inventories** ([`StalenessPage`] via [`AletheiaDB::staleness_inventory`]):
//!   the operator's re-verification worklist — facts in a scope exceeding a
//!   caller-supplied age threshold, paginated.
//!
//! It is a **pure read** over already-stored history: no writes, no
//! storage-format change, no write-path hook. Its result is fully determined by
//! `(cohort, options, as-of transaction time)`, so the same history in yields
//! the same statistics out (Issue #3377 AC2 reproducibility).
//!
//! # The estimator — Kaplan–Meier (deterministic, censoring-aware)
//!
//! A fact still current at analysis time has *not yet* been superseded; treating
//! its current age as a completed lifespan would bias every half-life downward.
//! The [Kaplan–Meier][km] non-parametric estimator is the standard tool that
//! stays unbiased under such **right-censoring**, so it is the committed
//! estimator (Issue #3377's "recover planted half-lives within ±10% under 30%
//! censoring" success metric effectively mandates it — a naive mean/median of
//! completed lifespans fails it). The half-life is the KM **median**: the
//! smallest `t` at which the survival curve `Ŝ(t) ≤ 0.5`.
//!
//! [km]: https://en.wikipedia.org/wiki/Kaplan%E2%80%93Meier_estimator
//!
//! # What one *observation* is (the operational definition, AC2)
//!
//! One observation is one **lifespan** of a fact version: the valid-time
//! duration a version was the live assertion before a *terminating event*. The
//! event oracle is reused verbatim from the belief-revision audit (Issue #3362,
//! [`RevisionClass`](super::belief_revision::RevisionClass)):
//!
//! - A [`WorldChange`](super::belief_revision::RevisionClass::WorldChange) or
//!   [`Retraction`](super::belief_revision::RevisionClass::Retraction) **terminates**
//!   a lifespan — the fact genuinely changed in the world, or was retracted /
//!   deleted (Issue #3230). The lifespan is a completed **event** observation.
//! - A [`Correction`](super::belief_revision::RevisionClass::Correction) or
//!   [`Reaffirmation`](super::belief_revision::RevisionClass::Reaffirmation)
//!   does **not** terminate a lifespan — it rewrites the record of the same
//!   valid period (correction-of-record), so version churn from corrections is
//!   *not* counted as change-in-world (AC2).
//! - An **open** valid interval (`valid_to == ∞`, i.e.
//!   [`TimeRange::duration_micros`](crate::core::temporal::TimeRange::duration_micros)
//!   returns `None`) means the fact is still alive at analysis time — a
//!   **right-censored** observation, not a completed lifespan.
//!
//! Property cohorts ([`Cohort::NodeProperty`]) terminate a lifespan only when
//! *that property's* value changes across a terminating transition (a
//! per-property refinement of #3362's entity-level classification).
//!
//! # Bounding & coverage caveats (AC7 / AC8)
//!
//! Cohort scans are bounded by the established
//! [`max_schema_as_of_entities`](crate::storage::historical) cap (default
//! 50,000, lowest ids kept); a truncated scan sets `sampled: true`. The
//! effective cap is `min(options.max_entities, max_schema_as_of_entities)`, read
//! from historical-storage configuration at scan time (Issue #3377 Fix-B M3), so
//! lowering the storage cap lowers the half-life scan cap in lockstep.
//! Statistics reflect **available history** — the hot tier plus restored
//! history; versions migrated to the cold tier follow the Issue #3238
//! coverage-caveat model. The response discloses the window it saw.
//!
//! # Performance & complexity (v1 caveats)
//!
//! A cohort's `(VolatilityStats, KmCurve)` is memoized in a gated, in-memory,
//! recomputable [`CohortStatsCache`] keyed on the full determinant
//! `(cohort, as_of, min_events, max_entities)` and reused until a committed
//! write advances the WAL append count (for an `as_of == None` query); an
//! `as_of == Some(_)` past-snapshot entry is immutable and cached indefinitely.
//! This makes a **warm** [`fact_freshness`](AletheiaDB::fact_freshness) O(1) in
//! the cohort size (one entity-history read plus a hash lookup). The cache is
//! **off the write path** — there is no write-time hook, so a write pays
//! nothing; invalidation is a cheap O(1) generation comparison on read.
//!
//! Two known super-linear costs remain on a **cold** scan and are deliberately
//! left as documented v1 limitations (a lean history accessor that fixes them
//! would touch the core `historical`/`temporal` reconstruction paths broadly —
//! out of scope for this performance pass):
//!
//! - **O(k²) per entity in versions-per-entity.** A cohort scan reconstructs
//!   each member's full history via `get_node_history` / `get_edge_history`,
//!   which for a k-version entity applies anchor+delta reconstruction O(k) per
//!   version → O(k²) total. The `max_entities` cap bounds the *entity count*,
//!   not the versions-per-entity, so a cohort whose versions concentrate in a
//!   few hot entities (a `stock_price`-style fact) can dominate the scan. The
//!   cap + cache are the v1 mitigation; a per-entity version budget and a lean
//!   `(label, interval, one-key)` accessor are tracked follow-ups.
//! - **Full `PropertyMap` cloned per version.** `get_*_history` clones every
//!   reconstructed version's whole property map (including any embedding
//!   vectors), though the estimator needs only the label, the valid interval,
//!   and — for a property cohort — one key's value. For embedding-carrying
//!   entities this is the dominant allocation cost. Same mitigation and same
//!   follow-up as above.
//!
//! # Feature gate
//!
//! Experimental — gated by `features = ["semantic-temporal"]` (or the `nova`
//! umbrella) per ADR-0050. The entire module and its [`AletheiaDB`] accessors
//! compile out when the flag is off, so there is zero cost (and no write-path
//! hook) in a build without it.
//!
//! # Status
//!
//! **Stage B landed (Issue #3377).** The pure Kaplan–Meier estimator, the
//! bounded cohort scan, and the three [`AletheiaDB`] accessors
//! ([`knowledge_half_life`](AletheiaDB::knowledge_half_life),
//! [`fact_freshness`](AletheiaDB::fact_freshness),
//! [`staleness_inventory`](AletheiaDB::staleness_inventory)) are implemented and
//! green. The correctness/validation hardening pass (Fix-A) folded in the
//! property-removal lifespan fix, the `insufficient_data` freshness fix, as-of
//! clock pinning for reproducibility, threshold/option validation, and the AC8
//! [`CoverageScope`] disclosure.
//!
//! **Fix-B landed (performance).** The [`CohortStatsCache`] is now wired as a
//! gated field on [`AletheiaDB`] and all three accessors route through it (a
//! single scan + one Kaplan–Meier pass per determinant, no double-KM), the scan
//! cap is config-driven (M3), and a benchmark (`benches/half_life.rs`) measures
//! cohort throughput and warm freshness latency against the SM3 targets. The
//! O(k²)/PropertyMap-clone cold-scan costs are documented above as v1
//! limitations (mitigated by the cap + cache; a lean history accessor is a
//! tracked follow-up).

use std::time::Duration;

use super::belief_revision::{RevisionClass, classify};
use crate::AletheiaDB;
use crate::core::error::{Error, QueryError, Result};
use crate::core::history::{EntityHistory, VersionInfo};
use crate::core::id::EntityId;
use crate::core::temporal::{Timestamp, time};

/// The minimum number of **observations** (events *plus* right-censored) a
/// cohort must have before a half-life is estimated at all. Below this floor the
/// analysis returns [`VolatilityStats`] with `insufficient_data == true` and no
/// half-life — never a spurious number (Issue #3377 AC3).
///
/// Despite the historical `EVENTS` in the name (kept for API stability while the
/// feature is experimental), the floor is measured against the total
/// *observation* count, matching the issue's "insufficient **observations**"
/// wording — see [`summarize`] for why gating on the event count alone would
/// collapse the distinct "plenty of data, all right-censored" state into
/// `insufficient_data`.
///
/// This is the *refusal* floor and is deliberately smaller than the ≥100
/// observations the success metric names for its ±10% *accuracy* guarantee: the
/// two thresholds answer different questions ("is any estimate defensible?" vs
/// "is the estimate within ±10%?").
pub const MIN_EVENTS_FOR_ESTIMATE: usize = 20;

/// The default value of [`HalfLifeOptions::max_entities`], matching the
/// established `max_schema_as_of_entities` default (Issue #3377 AC7).
///
/// The **effective** cap a scan applies is
/// `min(options.max_entities, historical.max_schema_as_of_entities())` — the
/// storage-wide configuration read at scan time bounds every scan, and the
/// per-call option can only tighten it further (Issue #3377 Fix-B M3). So a
/// deployment that lowers `max_schema_as_of_entities` lowers the half-life scan
/// cap in lockstep, and this constant is merely the option's standalone default
/// (used when no database config is in play).
pub const DEFAULT_MAX_ENTITIES: usize = 50_000;

/// The default page size for a [`staleness_inventory`](AletheiaDB::staleness_inventory)
/// query when the caller passes no explicit limit.
pub const DEFAULT_STALENESS_LIMIT: usize = 100;

/// A group of facts whose survival is analyzed together (Issue #3377 AC1).
///
/// All three granularities the acceptance criteria require are expressible:
/// per node label, per edge type, and per `(label, property_key)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cohort {
    /// All node versions carrying the given label.
    NodeLabel(String),
    /// All edge versions carrying the given edge type.
    EdgeType(String),
    /// Node versions of `label`, scoped to the lifespan of a single property
    /// `key` (a lifespan terminates only when *that key's* value changes).
    NodeProperty {
        /// The node label.
        label: String,
        /// The property key whose per-value lifespans are measured.
        key: String,
    },
}

impl Cohort {
    /// A stable lowercase token naming the cohort kind (for JSON / MCP
    /// responses): `"node_label"`, `"edge_type"`, or `"node_property"`.
    #[must_use]
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Cohort::NodeLabel(_) => "node_label",
            Cohort::EdgeType(_) => "edge_type",
            Cohort::NodeProperty { .. } => "node_property",
        }
    }
}

/// Options controlling a volatility analysis / cohort scan.
#[derive(Debug, Clone)]
pub struct HalfLifeOptions {
    /// Compute the statistics *as of* a past transaction time (Issue #3377 AC6):
    /// only versions recorded at or before this transaction time are considered,
    /// making the report replayable and pinnable (composes with #3370 named
    /// snapshots). `None` = current transaction time (now).
    pub as_of_transaction_time: Option<Timestamp>,
    /// The minimum event count below which the cohort is reported as
    /// `insufficient_data` (Issue #3377 AC3). Defaults to
    /// [`MIN_EVENTS_FOR_ESTIMATE`].
    pub min_events: usize,
    /// The candidate cap for the cohort scan (Issue #3377 AC7). Defaults to the
    /// effective `max_schema_as_of_entities` (see [`DEFAULT_MAX_ENTITIES`]).
    pub max_entities: usize,
}

impl Default for HalfLifeOptions {
    fn default() -> Self {
        Self {
            as_of_transaction_time: None,
            min_events: MIN_EVENTS_FOR_ESTIMATE,
            max_entities: DEFAULT_MAX_ENTITIES,
        }
    }
}

impl HalfLifeOptions {
    /// Empty options (identical to [`Default::default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Scope the analysis to a past transaction-time coordinate (AC6).
    #[must_use]
    pub fn with_as_of_transaction_time(mut self, ts: Timestamp) -> Self {
        self.as_of_transaction_time = Some(ts);
        self
    }

    /// Override the `insufficient_data` event floor.
    #[must_use]
    pub fn with_min_events(mut self, min_events: usize) -> Self {
        self.min_events = min_events;
        self
    }

    /// Override the cohort-scan candidate cap.
    #[must_use]
    pub fn with_max_entities(mut self, max_entities: usize) -> Self {
        self.max_entities = max_entities;
        self
    }
}

/// Which storage tiers a cohort scan actually covered (Issue #3377 AC8).
///
/// Half-life statistics reflect **available history**. The v1 cohort scan
/// enumerates hot-tier version heads
/// ([`versioned_node_ids`](crate::storage::historical)/`versioned_edge_ids`)
/// plus any history restored into the hot tier; versions migrated to the cold
/// tier are **excluded** (the Issue #3238 coverage-caveat model). This struct
/// makes that window explicit on every result so a caller never mistakes a
/// partial scan for a complete one — it is **distinct from `sampled`**, which
/// flags only candidate-cap truncation, not tier coverage. A lifespan whose
/// terminating event lives only in the cold tier can therefore be under-observed
/// (counted as still-open/censored); `cold_migrated_included == false` discloses
/// that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageScope {
    /// The hot tier (in-RAM version heads plus restored history) was scanned.
    /// Always `true` in v1.
    pub hot_tier: bool,
    /// Whether cold-tier-migrated history was included in the scan. Always
    /// `false` in v1 — the scan sees hot-tier heads only (Issue #3238 model).
    pub cold_migrated_included: bool,
}

impl CoverageScope {
    /// The v1 coverage scope: hot tier scanned, cold-migrated history excluded.
    #[must_use]
    pub const fn hot_only() -> Self {
        Self {
            hot_tier: true,
            cold_migrated_included: false,
        }
    }
}

/// The survival statistics of a cohort (Issue #3377 AC1).
///
/// When `insufficient_data` is `true` the cohort had fewer than
/// [`HalfLifeOptions::min_events`] observations and `half_life` / `iqr` are
/// `None` (AC3). A `half_life` of `None` with `insufficient_data == false` is the
/// *distinct* "heavily censored — the survival curve never reached 0.5" case,
/// which still reports its counts and an accompanying survival floor rather than
/// a fabricated median.
#[derive(Debug, Clone, PartialEq)]
pub struct VolatilityStats {
    /// The cohort these statistics describe.
    pub cohort: Cohort,
    /// The half-life = Kaplan–Meier median survival time. `None` when the cohort
    /// is `insufficient_data`, or when the survival curve never crossed 0.5.
    pub half_life: Option<Duration>,
    /// The dispersion measure: the (25th, 75th) percentile survival times of the
    /// KM curve. `None` when `half_life` is `None`, and *additionally* whenever
    /// the curve never descends to the 25th survival quantile (`S ≤ 0.25`): a
    /// curve whose floor sits in `(0.25, 0.5]` yields `half_life == Some` but
    /// `iqr == None`.
    pub iqr: Option<(Duration, Duration)>,
    /// Total observations considered (events + censored).
    pub observation_count: usize,
    /// Completed-lifespan (terminating-event) observations.
    pub event_count: usize,
    /// Right-censored (still-current) observations.
    pub censored_count: usize,
    /// `true` when the cohort scan hit the candidate cap and did not see every
    /// member (Issue #3377 AC7). This flags **candidate-cap truncation only**;
    /// see [`coverage`](Self::coverage) for tier coverage.
    pub sampled: bool,
    /// `true` when the cohort had fewer than the observation floor and no
    /// half-life was estimated (Issue #3377 AC3).
    pub insufficient_data: bool,
    /// Which storage tiers the scan covered (Issue #3377 AC8) — the window these
    /// statistics actually saw.
    pub coverage: CoverageScope,
}

/// The freshness of a single fact relative to its cohort's survival statistics
/// (Issue #3377 AC4).
#[derive(Debug, Clone, PartialEq)]
pub struct FreshnessScore {
    /// The scored fact.
    pub entity: EntityId,
    /// The cohort whose statistics were used.
    pub cohort: Cohort,
    /// The fact's current age: `now - version.valid_from`.
    pub age: Duration,
    /// The age expressed in cohort half-lives (`age / half_life`). `None` when
    /// the cohort has no half-life (insufficient data / heavy censoring).
    pub age_in_half_lives: Option<f64>,
    /// The estimated probability the fact is still true, read from the cohort's
    /// cached KM survival curve at `age` (fallback: the exponential
    /// `0.5^(age / half_life)`). `None` when the cohort has no usable curve.
    pub survival_probability: Option<f64>,
}

/// A staleness threshold for a [`staleness_inventory`](AletheiaDB::staleness_inventory)
/// query (Issue #3377 AC5) — absolute or in cohort half-lives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StalenessThreshold {
    /// Facts older than this absolute age qualify.
    AbsoluteAge(Duration),
    /// Facts older than this many cohort half-lives qualify (requires the cohort
    /// to have a half-life; a cohort without one matches nothing under this
    /// threshold).
    HalfLives(f64),
}

/// One entry in a staleness inventory: a fact exceeding the threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct StalenessEntry {
    /// The stale fact.
    pub entity: EntityId,
    /// The fact's current age.
    pub age: Duration,
    /// The age in cohort half-lives, when available.
    pub age_in_half_lives: Option<f64>,
    /// The estimated survival probability, when available.
    pub survival_probability: Option<f64>,
}

/// A paginated page of a staleness inventory (Issue #3377 AC5), following the
/// established offset/limit MCP list conventions.
#[derive(Debug, Clone, PartialEq)]
pub struct StalenessPage {
    /// The stale facts on this page, sorted by descending age (stable secondary
    /// sort by entity id for deterministic pagination).
    pub entries: Vec<StalenessEntry>,
    /// The total number of matching facts within the sampled candidate set.
    pub total_matching: usize,
    /// `true` when the cohort scan hit the candidate cap (Issue #3377 AC7) —
    /// candidate-cap truncation only; see [`coverage`](Self::coverage) for tier
    /// coverage.
    pub sampled: bool,
    /// The offset to pass on the next call to continue paging, or `None` when the
    /// page is the last.
    pub next_offset: Option<usize>,
    /// Which storage tiers the underlying cohort scan covered (Issue #3377 AC8).
    pub coverage: CoverageScope,
}

// ---------------------------------------------------------------------------
// PURE Kaplan–Meier estimator (no I/O; the statistical core).
// ---------------------------------------------------------------------------

/// One survival observation: a lifespan measured in microseconds and whether it
/// is a completed event or a right-censored (still-alive) observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// The observed lifespan (event) or current age (censored) in microseconds.
    pub duration_micros: i64,
    /// `true` when the fact was still current at analysis time (right-censored);
    /// `false` for a completed lifespan (a terminating event).
    pub censored: bool,
}

impl Observation {
    /// A completed-event observation of the given microsecond lifespan.
    #[must_use]
    pub const fn event(duration_micros: i64) -> Self {
        Self {
            duration_micros,
            censored: false,
        }
    }

    /// A right-censored observation of the given microsecond age.
    #[must_use]
    pub const fn censored(duration_micros: i64) -> Self {
        Self {
            duration_micros,
            censored: true,
        }
    }
}

/// A Kaplan–Meier survival curve: a right-continuous step function represented
/// as `(t_micros, S(t))` points at the event times, in ascending time order.
///
/// `S(t)` is the estimated probability a fact survives *beyond* time `t`. Before
/// the first event time the survival is `1.0` (implicit).
#[derive(Debug, Clone, PartialEq)]
pub struct KmCurve {
    /// The `(event_time_micros, survival_probability)` steps, ascending by time.
    pub steps: Vec<(i64, f64)>,
}

impl KmCurve {
    /// The median survival time: the smallest `t` at which `S(t) ≤ 0.5`.
    ///
    /// Returns `None` when the curve never reaches `0.5` (heavy censoring) — the
    /// caller distinguishes this from `insufficient_data`.
    #[must_use]
    pub fn median(&self) -> Option<i64> {
        self.percentile(0.5)
    }

    /// The survival-time percentile: the smallest `t` at which `S(t) ≤ 1 - p`
    /// (so `percentile(0.5)` == [`median`](Self::median)). `p` is clamped to
    /// `[0, 1]`. Returns `None` when the curve never drops that far.
    #[must_use]
    pub fn percentile(&self, p: f64) -> Option<i64> {
        let threshold = 1.0 - p.clamp(0.0, 1.0);
        self.steps
            .iter()
            .find(|(_, survival)| *survival <= threshold)
            .map(|(t, _)| *t)
    }

    /// The estimated survival probability at time `t` (microseconds): the value
    /// of the step function at `t`. `1.0` before the first event time.
    ///
    /// Beyond the largest event time this returns the KM **plateau** — the
    /// survival of the last step — because the curve does not descend through a
    /// censored tail. A fact aged far past the last observed event therefore
    /// reads that plateau (a survival *floor*), which can be materially higher
    /// than the exponential fallback `0.5^(age/half_life)` would give for the
    /// same age; [`survival_probability`] prefers the curve when one exists, so
    /// the two estimators can disagree for `t` past `max_event_time`. This is
    /// standard Kaplan–Meier behavior (the estimator makes no assumption about
    /// the unobserved tail).
    #[must_use]
    pub fn survival_at(&self, t: i64) -> f64 {
        // Right-continuous step: S(t) is the survival of the last step whose
        // event time is <= t; 1.0 before the first step. Steps are ascending.
        let mut survival = 1.0;
        for (event_time, step_survival) in &self.steps {
            if *event_time <= t {
                survival = *step_survival;
            } else {
                break;
            }
        }
        survival
    }
}

/// Compute the Kaplan–Meier survival curve from a set of observations.
///
/// Right-censored observations ([`Observation::censored`]) remain in the
/// risk set until their censoring time but never trigger a downward step, so
/// the estimate is unbiased under censoring. Ties (equal event times) are folded
/// into one step. An empty or all-censored input yields an empty curve (no
/// steps, `S ≡ 1.0`).
///
/// Pure over its input — no I/O, deterministic — so identical observations
/// always yield an identical curve.
#[must_use]
pub fn kaplan_meier(obs: &[Observation]) -> KmCurve {
    if obs.is_empty() {
        return KmCurve { steps: Vec::new() };
    }

    // Process observations in ascending time order, folding tied times into a
    // single group. `at_risk` is the number of observations with time >= the
    // current group's time (everyone still alive just before it); it starts at
    // the full sample and is decremented by each group after that group is
    // applied (censored observations leave the risk set without an event).
    let mut sorted: Vec<&Observation> = obs.iter().collect();
    sorted.sort_by_key(|o| o.duration_micros);

    let mut steps: Vec<(i64, f64)> = Vec::new();
    let mut survival = 1.0_f64;
    let mut at_risk = sorted.len();

    let mut i = 0;
    while i < sorted.len() {
        let t = sorted[i].duration_micros;
        let mut events_at_t = 0usize; // d_i
        let mut total_at_t = 0usize; // events + censored leaving at t
        while i < sorted.len() && sorted[i].duration_micros == t {
            if !sorted[i].censored {
                events_at_t += 1;
            }
            total_at_t += 1;
            i += 1;
        }
        // Only event times produce a downward step; censored-only times just
        // shrink the risk set (no NaN — at_risk > 0 while any obs remain).
        if events_at_t > 0 && at_risk > 0 {
            survival *= 1.0 - (events_at_t as f64) / (at_risk as f64);
            steps.push((t, survival));
        }
        at_risk -= total_at_t;
    }

    KmCurve { steps }
}

/// Assemble cohort [`VolatilityStats`] from a set of observations (pure).
///
/// This is the seam between the estimator and the cohort scan: the scan
/// produces observations, this folds them into the reported statistics
/// (half-life, IQR, counts, and the `insufficient_data` decision against
/// `min_events`). Kept pure so the statistics contract is unit-testable without
/// a database.
#[must_use]
pub fn summarize(
    cohort: Cohort,
    observations: &[Observation],
    min_events: usize,
) -> VolatilityStats {
    summarize_with_curve(cohort, observations, min_events).0
}

/// [`summarize`] plus the Kaplan–Meier curve it derived the half-life/IQR from,
/// computing the KM pass **exactly once** (Issue #3377 Fix-B: the accessors
/// previously ran `summarize` — which computes KM internally — and then a second
/// `kaplan_meier` on the same observations for the freshness/staleness survival
/// estimate). The curve is empty for an `insufficient_data` cohort (no estimate
/// is defensible below the floor), mirroring the freshness sufficiency gate.
#[must_use]
pub fn summarize_with_curve(
    cohort: Cohort,
    observations: &[Observation],
    min_events: usize,
) -> (VolatilityStats, KmCurve) {
    let observation_count = observations.len();
    let event_count = observations.iter().filter(|o| !o.censored).count();
    let censored_count = observation_count - event_count;

    // The `insufficient_data` floor is measured against the number of
    // *observations*, not events: a cohort with enough observations that are
    // all right-censored is NOT insufficient_data — it is the distinct
    // "half-life undefined under heavy censoring" case (half_life == None with
    // insufficient_data == false). Gating on event_count would make that
    // distinct case unreachable (all-censored has zero events), collapsing two
    // deliberately-separate states into one.
    let insufficient_data = observation_count < min_events;

    // Compute the KM curve at most once. Below the floor no estimate is
    // defensible, so the curve is empty and half_life/iqr are None.
    let curve = if insufficient_data {
        KmCurve { steps: Vec::new() }
    } else {
        kaplan_meier(observations)
    };
    let (half_life, iqr) = if insufficient_data {
        (None, None)
    } else {
        let half_life = curve.median().map(micros_to_duration);
        let iqr = match (curve.percentile(0.25), curve.percentile(0.75)) {
            (Some(p25), Some(p75)) => Some((micros_to_duration(p25), micros_to_duration(p75))),
            _ => None,
        };
        (half_life, iqr)
    };

    let stats = VolatilityStats {
        cohort,
        half_life,
        iqr,
        observation_count,
        event_count,
        censored_count,
        sampled: false,
        insufficient_data,
        // The pure summary describes a hot-tier scan; the cold tier is never
        // enumerated in v1 (Issue #3377 AC8 / #3238 coverage model).
        coverage: CoverageScope::hot_only(),
    };
    (stats, curve)
}

/// Fold a completed cohort [`CohortScan`] into its reported [`VolatilityStats`]
/// (with the scan's `sampled` flag applied) and the KM curve backing survival
/// estimates, computing Kaplan–Meier exactly once via [`summarize_with_curve`]
/// (Issue #3377 Fix-B). The single seam the cache-`compute` closure and
/// `staleness_inventory` share.
fn stats_and_curve_from_scan(
    cohort: &Cohort,
    scan: &CohortScan,
    min_events: usize,
) -> (VolatilityStats, KmCurve) {
    let (mut stats, curve) = summarize_with_curve(cohort.clone(), &scan.observations, min_events);
    stats.sampled = scan.sampled;
    (stats, curve)
}

/// Convert a non-negative microsecond duration to [`Duration`], clamping any
/// (defensive) negative value to zero.
#[must_use]
fn micros_to_duration(micros: i64) -> Duration {
    Duration::from_micros(micros.max(0) as u64)
}

/// The estimated survival probability of a fact of `age` given its cohort's
/// statistics (the freshness fallback, AC4).
///
/// Prefers the cohort's cached KM curve when supplied; otherwise falls back to
/// the exponential closed form `0.5^(age / half_life)`. Returns `None` when the
/// cohort has neither a curve nor a half-life.
#[must_use]
pub fn survival_probability(
    age: Duration,
    half_life: Option<Duration>,
    curve: Option<&KmCurve>,
) -> Option<f64> {
    // Prefer the cohort's cached KM curve when it carries at least one step.
    if let Some(curve) = curve
        && !curve.steps.is_empty()
    {
        let age_us = i64::try_from(age.as_micros()).unwrap_or(i64::MAX);
        return Some(curve.survival_at(age_us));
    }

    // Exponential fallback: 0.5^(age / half_life). Requires a positive
    // half-life; without one there is nothing to estimate against.
    let half_life_us = half_life?.as_micros() as f64;
    if half_life_us <= 0.0 {
        return None;
    }
    let age_us = age.as_micros() as f64;
    Some(0.5_f64.powf(age_us / half_life_us))
}

// ---------------------------------------------------------------------------
// In-memory cohort-statistics cache (recomputable; no durable format).
// ---------------------------------------------------------------------------

/// The maximum number of live entries the cache retains. Because
/// `as_of == Some(_)` entries pin immutable past snapshots and are **never**
/// invalidated, an unbounded map could grow without limit under many distinct
/// `as_of` coordinates; this cap bounds memory with a coarse single-entry
/// eviction on overflow (v1). Warm-path hits are unaffected until the cap is
/// reached.
const MAX_CACHE_ENTRIES: usize = 256;

/// An in-memory, recomputable cache of per-cohort [`VolatilityStats`] plus the
/// KM curve backing the sub-millisecond freshness path (Issue #3377 performance
/// metric).
///
/// Half-life statistics are always recomputable from history, so this cache is
/// **never persisted** — there is no durable sidecar and no on-disk format
/// change. It is entirely **off the write path**: there is no write-time hook
/// (so a write pays nothing), and staleness is instead detected lazily by
/// comparing a cheap global **write generation** — the WAL append count, an
/// O(1) atomic read — captured at compute time against the current one on each
/// read (see [`get_or_compute`](Self::get_or_compute)). A build with the
/// feature off pays nothing (the whole module compiles out).
///
/// # Invalidation model
///
/// - An `as_of == None` ("now") entry is reused only while the write generation
///   it was computed at is still current; any committed write advances the WAL
///   append count and forces a recompute on the next read. This is
///   **conservative** — an unrelated cohort's write also invalidates — trading a
///   little extra recompute for zero write-path cost and never serving stale
///   statistics.
/// - An `as_of == Some(T)` entry pins a **fixed past transaction snapshot**
///   (only versions recorded at or before `T` are ever considered), so it is
///   **immutable** and reused indefinitely regardless of later writes.
///
/// The entry is keyed on the full result determinant
/// `(cohort, as_of, min_events, max_entities)` so a differently-optioned call
/// never reads a stale entry (e.g. two `knowledge_half_life` calls on one cohort
/// with different `max_entities` must report different `sampled` flags).
#[derive(Debug, Default)]
pub struct CohortStatsCache {
    inner: std::sync::Mutex<CacheInner>,
}

/// Cache key: the full determinant tuple of a cohort analysis. Two calls that
/// would compute byte-identical statistics share one entry; any difference in
/// cohort, as-of coordinate, or bounding option is a distinct entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CohortStatsKey {
    cohort: Cohort,
    /// The as-of transaction-time coordinate in wallclock micros (`None` ==
    /// "now"); `Some(_)` marks an immutable past snapshot.
    as_of: Option<i64>,
    min_events: usize,
    max_entities: usize,
}

#[derive(Debug, Default)]
struct CacheInner {
    entries: std::collections::HashMap<CohortStatsKey, CachedStats>,
}

#[derive(Debug, Clone)]
struct CachedStats {
    stats: VolatilityStats,
    curve: KmCurve,
    /// The write generation (WAL append count) captured when this entry was
    /// computed. An `as_of == None` entry is valid only while this equals the
    /// current generation; an `as_of == Some(_)` entry ignores it (immutable).
    generation: u64,
}

impl CohortStatsCache {
    /// Create an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetch cached `(VolatilityStats, KmCurve)` for the given determinant,
    /// computing and storing them via `compute` on a miss (or a stale entry).
    ///
    /// `write_generation` is the cheap global staleness signal (the WAL append
    /// count): an `as_of == None` entry is reused only while the generation it
    /// was computed at still matches; an `as_of == Some(_)` entry is immutable
    /// and reused regardless. `compute` runs the O(scan) cohort analysis and is
    /// fallible (a scan can reject invalid options / miss a version); a warm hit
    /// is a hash lookup plus two clones, backing the sub-millisecond freshness
    /// path.
    pub fn get_or_compute<F>(
        &self,
        cohort: &Cohort,
        as_of: Option<i64>,
        min_events: usize,
        max_entities: usize,
        write_generation: u64,
        compute: F,
    ) -> Result<(VolatilityStats, KmCurve)>
    where
        F: FnOnce() -> Result<(VolatilityStats, KmCurve)>,
    {
        let key = CohortStatsKey {
            cohort: cohort.clone(),
            as_of,
            min_events,
            max_entities,
        };
        // Warm hit: an as_of-pinned entry (immutable past snapshot) is valid
        // regardless of generation; an as_of == None entry is valid only while
        // the write generation it was computed at is still current.
        if let Ok(inner) = self.inner.lock()
            && let Some(cached) = inner.entries.get(&key)
            && (as_of.is_some() || cached.generation == write_generation)
        {
            return Ok((cached.stats.clone(), cached.curve.clone()));
        }
        let (stats, curve) = compute()?;
        self.store(key, write_generation, &stats, &curve);
        Ok((stats, curve))
    }

    /// Insert (or refresh) a computed entry, bounding the map with a coarse
    /// single-entry eviction on overflow. Used both by the miss path of
    /// [`get_or_compute`](Self::get_or_compute) and to warm the cache from an
    /// accessor that already holds a fresh scan (so a subsequent freshness call
    /// is a warm hit).
    fn store(
        &self,
        key: CohortStatsKey,
        write_generation: u64,
        stats: &VolatilityStats,
        curve: &KmCurve,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.entries.len() >= MAX_CACHE_ENTRIES
                && !inner.entries.contains_key(&key)
                && let Some(evict) = inner.entries.keys().next().cloned()
            {
                inner.entries.remove(&evict);
            }
            inner.entries.insert(
                key,
                CachedStats {
                    stats: stats.clone(),
                    curve: curve.clone(),
                    generation: write_generation,
                },
            );
        }
    }

    /// The number of live cache entries (test-only observability for the
    /// eviction bound).
    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.inner.lock().map(|i| i.entries.len()).unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Database accessors (gated with the module).
// ---------------------------------------------------------------------------

impl AletheiaDB {
    /// Compute the survival statistics ("knowledge half-life") of a cohort
    /// (Issue #3377 AC1).
    ///
    /// Scans the cohort's recorded history (bounded by
    /// [`HalfLifeOptions::max_entities`]), classifies each version's lifespan
    /// via the #3362 event oracle, and folds the resulting observations into a
    /// [`VolatilityStats`] with a Kaplan–Meier half-life. Read-only; never
    /// touches the write path.
    ///
    /// # Errors
    ///
    /// Returns a structured error (Issue #3234 codes) for an invalid cohort or
    /// option.
    pub fn knowledge_half_life(
        &self,
        cohort: Cohort,
        options: &HalfLifeOptions,
    ) -> Result<VolatilityStats> {
        let (stats, _curve) = self.cohort_stats_cached(&cohort, options)?;
        Ok(stats)
    }

    /// Compute (or fetch from the warm cache) a cohort's
    /// `(VolatilityStats, KmCurve)` — the single seam
    /// [`knowledge_half_life`](Self::knowledge_half_life) and
    /// [`fact_freshness`](Self::fact_freshness) share (Issue #3377 Fix-B).
    ///
    /// The O(scan) cohort analysis plus **one** Kaplan–Meier pass runs once per
    /// `(cohort, as_of, min_events, max_entities)` determinant and is memoized in
    /// the gated [`half_life_cache`](AletheiaDB::half_life_cache). Reuse is
    /// governed by the WAL append count (an O(1) atomic read, no write-path
    /// hook): an `as_of == None` ("now") entry is served warm only while that
    /// generation is unchanged; an `as_of == Some(_)` past-snapshot entry is
    /// immutable and served warm indefinitely. This is what makes a warm
    /// `fact_freshness` O(1) in the cohort size.
    fn cohort_stats_cached(
        &self,
        cohort: &Cohort,
        options: &HalfLifeOptions,
    ) -> Result<(VolatilityStats, KmCurve)> {
        let as_of = options.as_of_transaction_time.map(|t| t.wallclock());
        let generation = self.wal.total_appends();
        self.half_life_cache.get_or_compute(
            cohort,
            as_of,
            options.min_events,
            options.max_entities,
            generation,
            || {
                let scan = self.scan_cohort(cohort, options)?;
                Ok(stats_and_curve_from_scan(cohort, &scan, options.min_events))
            },
        )
    }

    /// Compute the freshness score of a single fact relative to its cohort's
    /// survival statistics (Issue #3377 AC4).
    ///
    /// Reads the fact's current age and the (cached) cohort statistics, returning
    /// the age in half-lives and an estimated survival probability. Exposed on
    /// demand; it never silently alters any existing read response.
    ///
    /// The fact's `age` and the cohort's censored durations are measured against
    /// the analysis clock: the live wall clock by default, or — when
    /// [`HalfLifeOptions::as_of_transaction_time`] is set — that pinned
    /// coordinate, so a past-as-of report is reproducible (re-running it on
    /// frozen history yields the same age / survival, Issue #3377 AC6).
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for an unknown entity; a structured error for an invalid
    /// option.
    pub fn fact_freshness(
        &self,
        entity: EntityId,
        options: &HalfLifeOptions,
    ) -> Result<FreshnessScore> {
        let as_of = options.as_of_transaction_time;
        // Pin the analysis clock to `as_of` when scoped to a past tx-time so the
        // age / censored durations are reproducible (AC6); else the live clock.
        let now_us = analysis_now_us(as_of);

        // Fetch the entity's history (NOT_FOUND for an unknown entity).
        let history = match entity {
            EntityId::Node(id) => self.get_node_history(id)?,
            EntityId::Edge(id) => self.get_edge_history(id)?,
        };
        let visible = visible_versions(&history, as_of);
        let Some(current) = visible.last().copied() else {
            return Err(invalid_argument(
                "as_of_transaction_time",
                format!(
                    "entity {entity} has no version recorded at or before the given transaction time"
                ),
            ));
        };

        // The entity's cohort is its own label / edge type.
        let cohort = match entity {
            EntityId::Node(_) => Cohort::NodeLabel(current.label.clone()),
            EntityId::Edge(_) => Cohort::EdgeType(current.label.clone()),
        };

        // Current valid age of the entity's current version (saturating so an
        // extreme backdated valid_from cannot overflow, Fix-A L1).
        let age_us = now_us
            .saturating_sub(current.temporal.valid_time().start().wallclock())
            .max(0);
        let age = micros_to_duration(age_us);

        // Cohort statistics + KM curve via the shared warm cache (Issue #3377
        // Fix-B): O(1) on a warm hit, so the freshness path no longer re-scans
        // the whole cohort on every call. The curve is empty for a below-floor
        // cohort, so gating on sufficiency degrades to
        // `(age_in_half_lives: None, survival_probability: None)` in lockstep —
        // never a spurious survival number from an under-powered sample
        // (Fix-A MAJOR-2, AC3).
        let (stats, km_curve) = self.cohort_stats_cached(&cohort, options)?;
        let curve = (!stats.insufficient_data).then_some(&km_curve);

        let age_in_half_lives = stats.half_life.and_then(|hl| {
            let hl_us = hl.as_micros() as f64;
            (hl_us > 0.0).then_some(age_us as f64 / hl_us)
        });
        let survival_probability = survival_probability(age, stats.half_life, curve);

        Ok(FreshnessScore {
            entity,
            cohort,
            age,
            age_in_half_lives,
            survival_probability,
        })
    }

    /// List the facts in a cohort whose age exceeds `threshold` — the operator's
    /// re-verification worklist (Issue #3377 AC5).
    ///
    /// Paginated per the established offset/limit MCP list conventions; the scan
    /// is bounded and flags `sampled` on truncation (AC7).
    ///
    /// # Errors
    ///
    /// A structured error for an invalid cohort, threshold, or paging argument.
    pub fn staleness_inventory(
        &self,
        cohort: Cohort,
        threshold: StalenessThreshold,
        offset: usize,
        limit: usize,
        options: &HalfLifeOptions,
    ) -> Result<StalenessPage> {
        if limit == 0 {
            return Err(invalid_argument("limit", "limit must be greater than 0"));
        }
        // Reject a non-finite or negative half-lives threshold: NaN would
        // silently match nothing and a negative value would match the whole
        // cohort — either produces a misleading worklist (Fix-A M1, AC5).
        if let StalenessThreshold::HalfLives(n) = threshold
            && (!n.is_finite() || n < 0.0)
        {
            return Err(invalid_argument(
                "threshold",
                "half-lives threshold must be finite and >= 0",
            ));
        }

        // The staleness inventory needs the per-member ages the cache does not
        // hold, so it runs its own bounded scan. It derives the cohort stats +
        // KM curve from that scan **once** (Issue #3377 Fix-B M4: no more double
        // Kaplan–Meier) and routes them through the shared cache so a subsequent
        // `knowledge_half_life` / `fact_freshness` on the same determinant is a
        // warm hit — and, symmetrically, a *prior* warm entry is reused here
        // (Fix-B M2 scan-sharing). Either way the pair is byte-identical to what
        // this scan produced.
        let scan = self.scan_cohort(&cohort, options)?;
        let as_of = options.as_of_transaction_time.map(|t| t.wallclock());
        let generation = self.wal.total_appends();
        let (stats, km_curve) = self.half_life_cache.get_or_compute(
            &cohort,
            as_of,
            options.min_events,
            options.max_entities,
            generation,
            || {
                Ok(stats_and_curve_from_scan(
                    &cohort,
                    &scan,
                    options.min_events,
                ))
            },
        )?;

        // A `HalfLives` threshold uses the cohort half-life (and curve for the
        // per-entry survival estimate); an `AbsoluteAge` threshold uses neither,
        // matching the prior per-entry output exactly.
        let needs_half_life = matches!(threshold, StalenessThreshold::HalfLives(_));
        let cohort_half_life = if needs_half_life {
            stats.half_life
        } else {
            None
        };
        let curve = needs_half_life.then_some(&km_curve);

        let mut matches: Vec<StalenessEntry> = Vec::new();
        for (entity, age_us) in &scan.members {
            let qualifies = match threshold {
                StalenessThreshold::AbsoluteAge(d) => (*age_us as u128) >= d.as_micros(),
                StalenessThreshold::HalfLives(n) => match cohort_half_life {
                    // A cohort with no half-life matches nothing under a
                    // half-lives threshold (documented AC5 behavior).
                    Some(hl) => (*age_us as f64) >= hl.as_micros() as f64 * n,
                    None => false,
                },
            };
            if !qualifies {
                continue;
            }
            let age = micros_to_duration(*age_us);
            let age_in_half_lives = cohort_half_life.and_then(|hl| {
                let hl_us = hl.as_micros() as f64;
                (hl_us > 0.0).then_some(*age_us as f64 / hl_us)
            });
            let survival_probability = survival_probability(age, cohort_half_life, curve);
            matches.push(StalenessEntry {
                entity: *entity,
                age,
                age_in_half_lives,
                survival_probability,
            });
        }

        // Stable order: oldest first, then by entity id for deterministic
        // pagination under equal ages.
        matches.sort_by(|a, b| b.age.cmp(&a.age).then_with(|| a.entity.cmp(&b.entity)));

        let total_matching = matches.len();
        let entries: Vec<StalenessEntry> = matches.into_iter().skip(offset).take(limit).collect();
        let next_offset = {
            let consumed = offset + entries.len();
            (consumed < total_matching).then_some(consumed)
        };

        Ok(StalenessPage {
            entries,
            total_matching,
            sampled: scan.sampled,
            next_offset,
            coverage: CoverageScope::hot_only(),
        })
    }
}

// ---------------------------------------------------------------------------
// Cohort scan (bounded, capped, engine-leaf read over already-stored history).
// ---------------------------------------------------------------------------

/// The result of scanning a cohort's recorded history: the survival
/// observations feeding the estimator, the current members (with their current
/// valid ages) feeding the staleness inventory, and whether the candidate scan
/// hit the cap.
struct CohortScan {
    observations: Vec<Observation>,
    /// `(entity, current_valid_age_micros)` for each current cohort member.
    members: Vec<(EntityId, i64)>,
    sampled: bool,
}

impl AletheiaDB {
    /// Enumerate a cohort's members (bounded by `options.max_entities`), read
    /// each one's recorded history, and derive its survival observations +
    /// current age. Read-only; takes only short historical read locks.
    ///
    /// An **unknown but structurally-valid** cohort label/edge-type/property key
    /// is not an error: it simply matches no members and yields an empty scan,
    /// which the accessors surface as an `insufficient_data`
    /// [`VolatilityStats`] (consistent across `knowledge_half_life` /
    /// `staleness_inventory` / the cohort scan inside `fact_freshness`). This is
    /// deliberately asymmetric with `fact_freshness`'s *entity* lookup, where an
    /// unknown entity id is a `NOT_FOUND` storage error — a missing entity is a
    /// caller mistake, whereas a cohort with no members is a legitimate empty
    /// result.
    fn scan_cohort(&self, cohort: &Cohort, options: &HalfLifeOptions) -> Result<CohortScan> {
        // Reject a structurally-invalid cohort (an empty label/type/key).
        match cohort {
            Cohort::NodeLabel(label) | Cohort::EdgeType(label) if label.is_empty() => {
                return Err(invalid_argument("cohort", "cohort label must not be empty"));
            }
            Cohort::NodeProperty { label, key } if label.is_empty() || key.is_empty() => {
                return Err(invalid_argument(
                    "cohort",
                    "node-property cohort label and key must not be empty",
                ));
            }
            _ => {}
        }

        // Reject degenerate options: `min_events == 0` would defeat the AC3
        // insufficiency floor (an empty cohort would read as "heavy censoring"
        // rather than insufficient), and `max_entities == 0` would truncate the
        // entire scan away (Fix-A L2).
        if options.min_events == 0 {
            return Err(invalid_argument(
                "min_events",
                "min_events must be greater than 0",
            ));
        }
        if options.max_entities == 0 {
            return Err(invalid_argument(
                "max_entities",
                "max_entities must be greater than 0",
            ));
        }

        let as_of = options.as_of_transaction_time;
        // Pin the analysis clock to `as_of` when scoped to a past tx-time so the
        // censored durations / member ages are reproducible across re-runs on
        // frozen history (Fix-A MAJOR-3, AC2/AC6); else the live wall clock.
        let now_us = analysis_now_us(as_of);
        let is_edge = matches!(cohort, Cohort::EdgeType(_));
        let label = cohort_label(cohort);
        let key = match cohort {
            Cohort::NodeProperty { key, .. } => Some(key.as_str()),
            _ => None,
        };

        // Enumerate candidate ids under a single short read lock, sorted
        // ascending so the cap keeps the lowest ids (mirroring the
        // `max_schema_as_of_entities` scan discipline). `sampled` reflects
        // candidate truncation.
        let (node_ids, edge_ids, sampled) = {
            let historical = self.historical.read();
            // Effective candidate cap (Issue #3377 Fix-B M3): honor the
            // storage-wide `max_schema_as_of_entities` configuration read at scan
            // time, with the per-call `options.max_entities` as a further
            // (only-tightening) override — a smaller per-call value wins, and
            // neither exceeds the configured storage ceiling. (A degenerate
            // configured cap of 0 is ignored so it cannot truncate every scan.)
            let configured_cap = historical.max_schema_as_of_entities();
            let cap = if configured_cap == 0 {
                options.max_entities
            } else {
                options.max_entities.min(configured_cap)
            };
            if is_edge {
                let mut ids = historical.versioned_edge_ids();
                ids.sort_unstable();
                let sampled = ids.len() > cap;
                ids.truncate(cap);
                (Vec::new(), ids, sampled)
            } else {
                let mut ids = historical.versioned_node_ids();
                ids.sort_unstable();
                let sampled = ids.len() > cap;
                ids.truncate(cap);
                (ids, Vec::new(), sampled)
            }
        };

        let mut observations = Vec::new();
        let mut members = Vec::new();

        if is_edge {
            for id in edge_ids {
                let history = self.get_edge_history(id)?;
                if let Some((obs, member_age)) =
                    process_entity_history(&history, label, key, as_of, now_us)
                {
                    observations.extend(obs);
                    members.push((EntityId::Edge(id), member_age));
                }
            }
        } else {
            for id in node_ids {
                let history = self.get_node_history(id)?;
                if let Some((obs, member_age)) =
                    process_entity_history(&history, label, key, as_of, now_us)
                {
                    observations.extend(obs);
                    members.push((EntityId::Node(id), member_age));
                }
            }
        }

        Ok(CohortScan {
            observations,
            members,
            sampled,
        })
    }
}

/// The versions of `history` visible at `as_of` transaction time (all versions
/// when `as_of` is `None`), oldest-first, mirroring the belief-revision audit's
/// visibility filter (Issue #3362) so a half-life run at a past coordinate is
/// replayable.
fn visible_versions(history: &EntityHistory, as_of: Option<Timestamp>) -> Vec<&VersionInfo> {
    history
        .versions
        .iter()
        .filter(|v| match as_of {
            Some(a) => v.temporal.transaction_time().start() <= a,
            None => true,
        })
        .collect()
}

/// The wall-clock reference ("now") an analysis measures censored durations and
/// current ages against.
///
/// When the caller pins an `as_of` transaction-time coordinate, the analysis
/// clock is pinned to it too — so re-running the same past-as-of report on
/// frozen history yields identical censored durations and ages (Issue #3377
/// AC2/AC6 reproducibility). Otherwise it is the live wall clock (which honors a
/// [`SimulatedClock`](crate::simulation) injection under the `simulation`
/// feature). An `as_of` transaction-time coordinate is, by construction,
/// approximately the wall clock at which the measurement "would have been" taken.
fn analysis_now_us(as_of: Option<Timestamp>) -> i64 {
    match as_of {
        Some(t) => t.wallclock(),
        None => time::now().wallclock(),
    }
}

/// The label/type/property-label string a cohort scopes to.
fn cohort_label(cohort: &Cohort) -> &str {
    match cohort {
        Cohort::NodeLabel(label) | Cohort::EdgeType(label) | Cohort::NodeProperty { label, .. } => {
            label
        }
    }
}

/// Derive one entity's survival observations and current valid age for the
/// cohort, or `None` when the entity is not in the cohort (wrong label, or no
/// version visible at the as-of coordinate).
fn process_entity_history(
    history: &EntityHistory,
    label: &str,
    key: Option<&str>,
    as_of: Option<Timestamp>,
    now_us: i64,
) -> Option<(Vec<Observation>, i64)> {
    let visible = visible_versions(history, as_of);
    let current = visible.last().copied()?;
    if current.label != label {
        return None;
    }
    let observations = observations_from_versions(&visible, key, now_us);
    let current_age = now_us
        .saturating_sub(current.temporal.valid_time().start().wallclock())
        .max(0);
    Some((observations, current_age))
}

/// Derive the survival observations for one entity's visible version history.
///
/// A fact's valid-time lifespan runs from the `valid_from` that established it
/// (the initial assertion, or a later WorldChange) until the next terminating
/// transition — a WorldChange (the fact changed: ends at the successor's
/// `valid_from`) or a Retraction (ends at the closed valid interval's end). A
/// Correction/Reaffirmation rewrites the record of the same validity period and
/// does NOT terminate the lifespan (Issue #3362 event oracle). A still-open
/// final lifespan yields a right-censored observation.
///
/// For a property cohort (`key` = `Some`), a terminating transition only ends
/// *that property's* lifespan when the property's value actually changes across
/// it (a per-property refinement: a WorldChange to a different property, or a
/// correction, leaves this property's lifespan running). A WorldChange that
/// *removes* the measured key ends the lifespan (one completed event) and does
/// **not** begin a new one — the property no longer exists, so no phantom
/// censored/event observation is emitted (Fix-A MAJOR-1).
fn observations_from_versions(
    visible: &[&VersionInfo],
    key: Option<&str>,
    now_us: i64,
) -> Vec<Observation> {
    let mut observations = Vec::new();
    // Wallclock micros at which the currently-live fact's valid interval began,
    // or `None` when no fact is currently live (before the property appears, or
    // after a retraction).
    let mut life_start: Option<i64> = None;
    let mut max_prior_valid_from: Option<Timestamp> = None;

    for i in 0..visible.len() {
        let version = visible[i];
        let predecessor = (i > 0).then(|| visible[i - 1]);
        let class = classify(version, predecessor, max_prior_valid_from);
        let valid_from = version.temporal.valid_time().start();
        let valid_from_us = valid_from.wallclock();

        if predecessor.is_none() {
            // Initial assertion: a lifespan begins iff the property is present
            // (always, for an entity cohort).
            life_start = key_present(version, key).then_some(valid_from_us);
        } else {
            match class {
                RevisionClass::WorldChange => {
                    let terminates = match key {
                        None => true,
                        Some(k) => value_changed(predecessor, version, k),
                    };
                    if terminates {
                        if let Some(start) = life_start {
                            observations.push(Observation::event(
                                valid_from_us.saturating_sub(start).max(0),
                            ));
                        }
                        // Only restart the lifespan if the fact still exists in
                        // this version. A WorldChange that REMOVES the measured
                        // property terminates the lifespan but does NOT begin a
                        // new one — restarting unconditionally would emit a
                        // phantom censored/event observation for a property that
                        // is no longer present (Fix-A MAJOR-1). For an entity
                        // cohort (`key == None`) `key_present` is always true, so
                        // this preserves the prior behavior exactly.
                        life_start = key_present(version, key).then_some(valid_from_us);
                    }
                }
                RevisionClass::Retraction => {
                    let terminates = match key {
                        None => true,
                        Some(k) => predecessor.is_some_and(|p| p.properties.get(k).is_some()),
                    };
                    if terminates {
                        if let Some(start) = life_start {
                            let end = version.temporal.valid_time().end().wallclock();
                            observations.push(Observation::event(end.saturating_sub(start).max(0)));
                        }
                        life_start = None;
                    }
                }
                // Correction / Reaffirmation continue the current lifespan; if
                // the (property) fact had not yet begun, it may begin here.
                _ => {
                    if life_start.is_none() && key_present(version, key) {
                        life_start = Some(valid_from_us);
                    }
                }
            }
        }

        max_prior_valid_from = Some(match max_prior_valid_from {
            Some(m) if m >= valid_from => m,
            _ => valid_from,
        });
    }

    // A still-open final lifespan is a right-censored observation.
    if let Some(start) = life_start {
        observations.push(Observation::censored(now_us.saturating_sub(start).max(0)));
    }

    observations
}

/// Whether `version` carries the cohort's property key (always `true` for an
/// entity cohort, where `key` is `None`).
fn key_present(version: &VersionInfo, key: Option<&str>) -> bool {
    match key {
        None => true,
        Some(k) => version.properties.get(k).is_some(),
    }
}

/// Whether the value of property `key` differs between `predecessor` and
/// `version` (an added or removed key counts as a change; NaN-aware via
/// `semantically_equal`).
fn value_changed(predecessor: Option<&VersionInfo>, version: &VersionInfo, key: &str) -> bool {
    let before = predecessor.and_then(|p| p.properties.get(key));
    let after = version.properties.get(key);
    match (before, after) {
        (Some(a), Some(b)) => !a.semantically_equal(b),
        (None, None) => false,
        _ => true,
    }
}

/// Build an `INVALID_ARGUMENT`-mapped error (`QueryError::InvalidParameter`),
/// mirroring the belief-revision helper (Issue #3234 structured codes).
fn invalid_argument(parameter: &str, reason: impl Into<String>) -> Error {
    Error::Query(QueryError::InvalidParameter {
        parameter: parameter.to_string(),
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    //! Pure-estimator unit tests (no database) for the Kaplan–Meier core and the
    //! `summarize` seam (Issue #3377); the cohort/freshness/staleness/as-of
    //! coverage lives in the gated `tests/half_life_e2e.rs` integration file.
    //! These assert concrete expected values against the (green) Stage-B
    //! implementation plus the Fix-A correctness/validation hardening.

    use super::*;

    const SECOND_US: i64 = 1_000_000;
    const DAY_US: i64 = 86_400 * SECOND_US;

    /// A tiny deterministic pseudo-random sequence (a linear congruential
    /// generator) — NOT `rand`/`Math.random` — so the 30%-censoring fixture is
    /// byte-reproducible across runs (Issue #3377 reproducibility).
    struct Lcg(u64);
    impl Lcg {
        fn next_u64(&mut self) -> u64 {
            // Numerical Recipes LCG constants.
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0
        }
        /// A pseudo-random f64 in `[0, 1)`.
        fn next_unit(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    // ---- Test 1: KM recovers a planted half-life within ±10% (30% censored) --

    #[test]
    fn km_recovers_planted_half_life_within_tolerance() {
        // Plant an exponential-lifespan cohort with a known median (half-life)
        // and apply PROPER right-censoring, then assert Kaplan–Meier recovers
        // the planted half-life within ±10%.
        //
        // Right-censoring means the censoring time C is drawn INDEPENDENTLY of
        // the true lifetime T and we observe `min(T, C)` with the indicator
        // `C < T`. The Stage-A fixture instead drew one `t` and flipped a 30%
        // coin to *relabel* it censored — which is NOT right-censoring: the
        // censoring time equals the event time, so it is not independent of T.
        // Under that flawed model KM provably estimates S(t)^0.7, whose median
        // is 0.5^(1/0.7)-quantile == 1.4288·half_life — so a *correct* KM can
        // never land within ±10%. This is a fixture bug, not an estimator bug
        // (verified: on a large PROPERLY-censored sample KM converges to the
        // planted half-life; on the old model it converges to 1.43·half_life).
        // Per the Stage-B mandate the TEST is corrected to the design (KM's
        // censoring-awareness) rather than the estimator loosened. The ±10%
        // tolerance is unchanged; the sample is enlarged to 1000 to keep the
        // median's sampling error comfortably inside it (the success metric
        // names ">=100 obs" as the accuracy floor).
        let planted_half_life_us = 30 * DAY_US;
        let hl = planted_half_life_us as f64;
        let mut rng = Lcg(0x5150_1234_ABCD_0001);
        // C's mean = (7/3)·T's mean makes P(C < T) = 1/(1 + 7/3) = 0.30 for two
        // independent exponentials — a planted ~30% right-censoring rate.
        const CENSOR_SCALE: f64 = 7.0 / 3.0;
        let mut obs = Vec::with_capacity(1000);
        for _ in 0..1000 {
            // Exponential deviate with median == planted_half_life_us:
            //   t = half_life · (-ln(u)/ln2),  u ~ Uniform(0, 1]
            let u_t = 1.0 - rng.next_unit();
            let true_lifetime = hl * (-(u_t.ln()) / std::f64::consts::LN_2);
            // Independent censoring time, scaled so ~30% of units are censored.
            let u_c = 1.0 - rng.next_unit();
            let censor_time = CENSOR_SCALE * hl * (-(u_c.ln()) / std::f64::consts::LN_2);
            obs.push(if censor_time < true_lifetime {
                Observation::censored(censor_time as i64)
            } else {
                Observation::event(true_lifetime as i64)
            });
        }
        let censored_n = obs.iter().filter(|o| o.censored).count();
        assert!(
            (250..=400).contains(&censored_n),
            "≈30% of 1000 right-censored, got {censored_n}"
        );
        let curve = kaplan_meier(&obs);
        let median = curve.median().expect("curve reaches 0.5 with ~70% events");
        let lo = (hl * 0.90) as i64;
        let hi = (hl * 1.10) as i64;
        assert!(
            (lo..=hi).contains(&median),
            "KM median {median} not within ±10% of planted {planted_half_life_us}"
        );
    }

    // ---- Test 2: all-censored cohort => half-life undefined (not insufficient) -

    #[test]
    fn all_censored_has_no_median() {
        let obs: Vec<Observation> = (1..=50)
            .map(|i| Observation::censored(i * DAY_US))
            .collect();
        let curve = kaplan_meier(&obs);
        assert_eq!(
            curve.median(),
            None,
            "an all-censored curve never reaches 0.5, so the median is undefined"
        );
        // ...but this is NOT insufficient_data: there are >= min_events observations.
        let stats = summarize(Cohort::NodeLabel("Person".into()), &obs, 20);
        assert!(
            !stats.insufficient_data,
            "50 observations exceed the floor; heavy censoring is a distinct case"
        );
        assert_eq!(stats.half_life, None, "no median => no half-life");
        assert_eq!(stats.censored_count, 50);
        assert_eq!(stats.event_count, 0);
    }

    // ---- Test 3: below-floor events => insufficient_data (no number) ----------

    #[test]
    fn below_floor_is_insufficient_data() {
        // 5 events, floor 20 => insufficient_data, no fabricated half-life.
        let obs: Vec<Observation> = (1..=5).map(|i| Observation::event(i * DAY_US)).collect();
        let stats = summarize(Cohort::NodeLabel("Person".into()), &obs, 20);
        assert!(
            stats.insufficient_data,
            "5 < 20 events => insufficient_data"
        );
        assert_eq!(
            stats.half_life, None,
            "insufficient_data => no half-life number"
        );
        assert_eq!(stats.iqr, None);
        assert_eq!(stats.event_count, 5);
    }

    // ---- Test 11: median = smallest t with S<=0.5 + IQR percentiles + ties ----

    #[test]
    fn median_and_percentiles_over_explicit_curve() {
        // A hand-built survival curve stepping through 0.5 exactly at t=200.
        //   S: 0.8 @100, 0.5 @200, 0.25 @300, 0.10 @400
        let curve = KmCurve {
            steps: vec![(100, 0.80), (200, 0.50), (300, 0.25), (400, 0.10)],
        };
        // median = smallest t with S(t) <= 0.5 => 200.
        assert_eq!(curve.median(), Some(200));
        // 25th percentile survival time: smallest t with S <= 0.75 => 200.
        assert_eq!(curve.percentile(0.25), Some(200));
        // 75th percentile survival time: smallest t with S <= 0.25 => 300.
        assert_eq!(curve.percentile(0.75), Some(300));
        // survival_at is right-continuous: 1.0 before first step, then the step.
        assert_eq!(curve.survival_at(50), 1.0);
        assert_eq!(curve.survival_at(100), 0.80);
        assert_eq!(curve.survival_at(150), 0.80);
        assert_eq!(curve.survival_at(200), 0.50);
    }

    #[test]
    fn tied_event_times_fold_into_one_step() {
        // Three events all at t=100 must produce a single step at 100, not three.
        let obs = vec![
            Observation::event(100),
            Observation::event(100),
            Observation::event(100),
        ];
        let curve = kaplan_meier(&obs);
        assert_eq!(curve.steps.len(), 1, "tied event times fold into one step");
        assert_eq!(curve.steps[0].0, 100);
        // With all three failing at the same time, S drops from 1.0 to 0.0.
        assert!((curve.steps[0].1 - 0.0).abs() < 1e-9);
    }

    // ---- Test 12: empty cohort => insufficient_data, no panic -----------------

    #[test]
    fn empty_cohort_is_insufficient_data() {
        let curve = kaplan_meier(&[]);
        assert!(curve.steps.is_empty(), "no observations => empty curve");
        assert_eq!(curve.median(), None);
        let stats = summarize(Cohort::EdgeType("KNOWS".into()), &[], 20);
        assert!(stats.insufficient_data);
        assert_eq!(stats.observation_count, 0);
        assert_eq!(stats.half_life, None);
    }

    // ---- Test 13: single-observation cohort -----------------------------------

    #[test]
    fn single_event_curve_drops_to_zero() {
        let curve = kaplan_meier(&[Observation::event(500)]);
        assert_eq!(curve.steps.len(), 1);
        assert_eq!(curve.steps[0].0, 500);
        // A single event: S goes 1.0 -> 0.0 at t=500, so the median is 500.
        assert_eq!(curve.median(), Some(500));
    }

    // ---- Test 6 (pure part): survival probability off a known curve -----------

    #[test]
    fn freshness_survival_probability_reads_curve() {
        let curve = KmCurve {
            steps: vec![
                (10 * DAY_US, 0.75),
                (20 * DAY_US, 0.50),
                (40 * DAY_US, 0.25),
            ],
        };
        // A fact aged 20 days sits exactly on the 0.50 step.
        let p = survival_probability(
            Duration::from_micros((20 * DAY_US) as u64),
            Some(Duration::from_micros((20 * DAY_US) as u64)),
            Some(&curve),
        );
        assert_eq!(
            p,
            Some(0.50),
            "survival at the fact's age reads off the curve"
        );
        // Exponential fallback when no curve is supplied: 0.5^(age/half_life).
        // age == half_life => 0.5.
        let p_fallback = survival_probability(
            Duration::from_micros((20 * DAY_US) as u64),
            Some(Duration::from_micros((20 * DAY_US) as u64)),
            None,
        );
        let pf = p_fallback.expect("exponential fallback available with a half-life");
        assert!((pf - 0.5).abs() < 1e-9, "age==half_life => 0.5, got {pf}");
    }

    // ---- AC1: IQR percentiles surface as concrete Durations -------------------

    #[test]
    fn iqr_percentiles_are_concrete_durations() {
        // Four uncensored events at distinct times => the KM survival curve steps
        // down by 1/4 each: S = 3/4 @100, 2/4 @200, 1/4 @300, 0 @400. Hence:
        //   median             (smallest t, S <= 0.50) => 200
        //   25th-pctile surv.  (smallest t, S <= 0.75) => 100
        //   75th-pctile surv.  (smallest t, S <= 0.25) => 300
        let obs: Vec<Observation> = [100_i64, 200, 300, 400]
            .into_iter()
            .map(Observation::event)
            .collect();
        let stats = summarize(Cohort::NodeLabel("Person".into()), &obs, 4);
        assert!(
            !stats.insufficient_data,
            "4 observations meet the floor of 4"
        );
        assert_eq!(stats.half_life, Some(Duration::from_micros(200)));
        assert_eq!(
            stats.iqr,
            Some((Duration::from_micros(100), Duration::from_micros(300))),
            "IQR = (25th, 75th) survival-time percentiles as concrete Durations"
        );
    }

    // ---- AC8: the summary discloses the coverage window it saw ----------------

    #[test]
    fn summarize_discloses_hot_only_coverage() {
        let obs = vec![Observation::event(DAY_US)];
        let stats = summarize(Cohort::NodeLabel("Person".into()), &obs, 1);
        assert!(stats.coverage.hot_tier, "the hot tier is scanned");
        assert!(
            !stats.coverage.cold_migrated_included,
            "v1 excludes cold-migrated history (Issue #3238 coverage model)"
        );
    }

    // ---- Fix-B: cache generation invalidation + immutable as-of + bound ------

    #[test]
    fn cache_reuses_and_invalidates_on_write_generation() {
        use std::cell::Cell;
        let cache = CohortStatsCache::new();
        let cohort = Cohort::NodeLabel("Person".into());
        let obs = vec![Observation::event(DAY_US), Observation::event(2 * DAY_US)];
        let calls = Cell::new(0usize);
        let run = |as_of: Option<i64>, generation: u64| {
            cache
                .get_or_compute(&cohort, as_of, 1, 100, generation, || {
                    calls.set(calls.get() + 1);
                    Ok(summarize_with_curve(cohort.clone(), &obs, 1))
                })
                .expect("compute ok")
        };

        // First call at generation 0: a miss (compute runs).
        run(None, 0);
        assert_eq!(calls.get(), 1, "first call computes");

        // Same (cohort, as_of=None, opts) at the SAME generation: a warm hit —
        // compute must NOT run again.
        run(None, 0);
        assert_eq!(calls.get(), 1, "same generation => warm hit, no recompute");

        // Advance the write generation: an as_of=None entry must recompute.
        run(None, 1);
        assert_eq!(
            calls.get(),
            2,
            "advanced generation => recompute for as_of=None"
        );

        // An as_of=Some(T) entry is immutable: computed once, then served warm
        // even as the generation advances.
        run(Some(12_345), 5);
        assert_eq!(calls.get(), 3, "first as_of=Some call computes");
        run(Some(12_345), 999);
        assert_eq!(
            calls.get(),
            3,
            "as_of=Some entry is immutable => warm hit regardless of generation"
        );
    }

    #[test]
    fn cache_is_bounded_under_many_as_of_entries() {
        let cache = CohortStatsCache::new();
        let cohort = Cohort::NodeLabel("Person".into());
        let obs = vec![Observation::event(DAY_US)];
        // Insert far more distinct (immutable) as_of-keyed entries than the cap;
        // eviction must keep the map bounded rather than growing without limit.
        for t in 0..(MAX_CACHE_ENTRIES as i64 + 64) {
            let _ = cache.get_or_compute(&cohort, Some(t), 1, 100, 0, || {
                Ok(summarize_with_curve(cohort.clone(), &obs, 1))
            });
        }
        assert!(
            cache.entry_count() <= MAX_CACHE_ENTRIES,
            "cache must stay bounded at {MAX_CACHE_ENTRIES}, got {}",
            cache.entry_count()
        );
    }

    // ---- Test 15 note: zero-cost-when-off ------------------------------------
    //
    // There is no runtime test for "zero cost when the feature is off": the
    // entire module (including these tests and the `AletheiaDB` accessors) is
    // gated by `features = ["semantic-temporal"]`, so it compiles out of a build
    // without the flag. That property is enforced by `just check-features` and
    // the CI matrix job that builds *without* `semantic-temporal` — a compile-out
    // guarantee, not something a runtime assertion can observe.
}
