//! Compare-and-set (CAS) conditional writes and the lease/claim convention
//! (Issue #3577).
//!
//! # What this is
//!
//! A CAS is a normal conditional write whose **precondition** — "the entity's
//! committed head is still exactly `expected_version`" — is evaluated at commit
//! time. If the precondition holds, the persisted artifact is an ordinary
//! `UpdateNode`/`UpdateEdge` op carrying the full replacement property map, so
//! there is **zero WAL on-disk format change**. If it fails, the whole
//! transaction aborts with [`TransactionError::CasMismatch`] and applies nothing
//! to in-memory state. A pure-CAS mismatch is rejected pre-WAL by the fast-path
//! (see below), so the common single-threaded case writes nothing durable
//! either; a mismatch that only manifests under the commit guard (a concurrent
//! pure-CAS race, or any lease claim) still leaves a durable frame that would
//! replay on crash absent WAL abort framing (#3413) — the same accepted caveat
//! as the #3416 sibling write-skew checks.
//!
//! CAS uses **full-replace** semantics for the property map (like the #3549
//! replace API, not the PATCH merge of `update_node`): a CAS is a conditional
//! *set* of the entire entity state, matching the DBOS-style "write the whole
//! claim state" use case the lease layer is built for.
//!
//! # Where the authoritative check lives
//!
//! The **authoritative** precondition check lives in
//! [`detect_cas_precondition_violations`], invoked from
//! [`apply::apply_changes`](super::apply::apply_changes) **under the
//! already-held `historical.write()` commit-serialization guard** — the same
//! place the #3416 delete-orphan / dangling-endpoint write-skew re-checks run.
//! Only re-reading the committed head **while holding the guard that serializes
//! commits** makes the second of two concurrent claimants observe the first's
//! new version and abort. Its abort is a non-retriable `CasMismatch` rather than
//! a retriable `SerializationFailure` (a lost claim must not be blindly retried),
//! which is why CAS-target entities are **excluded** from the pre-lock
//! snapshot-isolation conflict check.
//!
//! A **best-effort pre-lock fast-path** in
//! [`conflict::detect_conflicts`](super::conflict) additionally short-circuits a
//! **pure** CAS (`lease: None`) whose expected version already fails to match
//! the committed head, BEFORE any WAL frame is appended — so the common,
//! single-threaded stale-CAS path writes nothing durable at all. This is only
//! sound for pure CAS: the committed head advances monotonically, so a pre-lock
//! mismatch can never become a match at commit. It is NOT sound for a lease
//! claim (whose expiry OR-branch can flip true relative to the later commit
//! timestamp) and is NOT a replacement for the under-guard check (two concurrent
//! pure-CAS both pass the fast-path; the under-guard check catches the loser).
//! See `conflict::detect_conflicts` for the full argument.
//!
//! # Lock order
//!
//! The re-check reads the committed head from `historical` (already held) and,
//! for a lease claim, the entity's committed properties from current storage (a
//! leaf, order class 6/7) — never acquiring an earlier primitive while holding a
//! later one, and adding no new lock site. Lease expiry is judged against the
//! **commit HLC timestamp** taken under `current_timestamp`, not the tx snapshot.

use super::WriteTransaction;
use super::validation;
use crate::api::transaction::types::TxState;
use crate::api::transaction::{BufferedWrite, WriteRequestOptions};
use crate::core::error::{Result, TransactionError};
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::namespace;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
use crate::core::temporal::{Timestamp, time};

/// The entity a [`CasPrecondition`] guards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CasTarget {
    /// A node CAS keyed on the node id.
    Node(NodeId),
    /// An edge CAS keyed on the edge id.
    Edge(EdgeId),
}

/// The lease/expiry branch of a claim precondition (Issue #3577).
///
/// When present, the claim succeeds if EITHER the version matches OR the
/// entity's current `lease_until` property (an integer of microseconds since
/// epoch, the convention this module writes) is `<=` the commit timestamp —
/// i.e. the lease is expired or was never set. The property *key* is
/// caller-supplied: this is a convention, not a hardcoded schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseCondition {
    /// Property key whose integer value holds the current lease expiry
    /// (microseconds since epoch).
    pub(crate) lease_until_key: String,
}

/// The monotonic-fence branch of a claim precondition (DBOS Phase 3e).
///
/// When present, the claim is admitted only if the caller-supplied `new_fence`
/// is **strictly greater** than the entity's committed fence (the integer value
/// of the `fence_key` property; absent / non-integer is treated as `i64::MIN`,
/// i.e. no fence held). Re-read under the commit-serialization guard, this makes
/// the stale-fence steal collision impossible: two stealers computing the same
/// `new_fence` from a stale read cannot both commit — the second re-reads the
/// first's stored fence and fails the strict-`>` check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FenceCondition {
    /// Property key whose integer value holds the monotonic fence token.
    pub(crate) fence_key: String,
    /// The fence value the caller stamps into the full-replace map; it must be
    /// strictly greater than the committed fence for the claim to be admitted.
    pub(crate) new_fence: i64,
}

/// A buffered CAS precondition, carried on the [`WriteTransaction`] until the
/// commit-time re-check under the `historical.write()` guard.
///
/// The accompanying write (a full-replace `UpdateNode`/`UpdateEdge`) is buffered
/// in the normal write buffer; this side-record only carries the *condition*, so
/// no `BufferedWrite` variant and no WAL format changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CasPrecondition {
    /// The entity the precondition guards.
    pub(crate) target: CasTarget,
    /// The version the caller expects the entity's committed head to still be.
    pub(crate) expected_version: VersionId,
    /// Optional lease/expiry OR-branch (present for `claim_with_lease`).
    pub(crate) lease: Option<LeaseCondition>,
    /// Optional monotonic-fence AND-branch (present for a fenced claim,
    /// `claim_with_lease_fenced`). AND-composed with the version/lease gate:
    /// the claim must be a valid claim *and* beat the stored fence.
    pub(crate) fence: Option<FenceCondition>,
}

impl WriteTransaction {
    /// Node ids that are CAS targets in this transaction (for the pre-lock
    /// snapshot-isolation conflict check to skip — the under-guard CAS re-check
    /// is authoritative for them).
    pub(crate) fn cas_target_nodes(&self) -> std::collections::HashSet<NodeId> {
        self.cas_preconditions
            .iter()
            .filter_map(|p| match p.target {
                CasTarget::Node(id) => Some(id),
                CasTarget::Edge(_) => None,
            })
            .collect()
    }

    /// Edge ids that are CAS targets in this transaction; see
    /// [`cas_target_nodes`](Self::cas_target_nodes).
    pub(crate) fn cas_target_edges(&self) -> std::collections::HashSet<EdgeId> {
        self.cas_preconditions
            .iter()
            .filter_map(|p| match p.target {
                CasTarget::Edge(id) => Some(id),
                CasTarget::Node(_) => None,
            })
            .collect()
    }

    /// Core node compare-and-set: buffer a conditional full-replace of `node_id`
    /// guarded by `expected_version` (and, optionally, a lease OR-branch), and
    /// return the new version id the write will carry on success.
    ///
    /// The actual precondition is enforced at commit time under the historical
    /// guard by [`detect_cas_precondition_violations`]; this call only buffers.
    /// A node absent at buffer time short-circuits to `CasMismatch { actual }`
    /// (never a panic) so a CAS on a nonexistent/deleted node is a clean lost
    /// claim, not a `NodeNotFound`.
    pub(crate) fn cas_node_impl(
        &mut self,
        node_id: NodeId,
        expected_version: VersionId,
        properties: PropertyMap,
        lease: Option<LeaseCondition>,
        fence: Option<FenceCondition>,
        options: WriteRequestOptions,
    ) -> Result<VersionId> {
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // A namespace is immutable after creation: reject an explicit namespace
        // on a CAS / lease-claim rather than silently ignoring it (#3349). This
        // also covers `claim_with_lease_impl`, which routes through here.
        namespace::reject_namespace_on_update(options.namespace.as_ref())?;

        // Reject engine-reserved keys on the user-supplied replacement map
        // (Issue #3349); the immutable namespace is re-stamped from the
        // existing node below.
        namespace::reject_reserved_keys(&properties)?;

        // Preserve the label; a full-replace changes only the property map.
        // Buffer-aware read (read-your-own-writes): a node created/updated
        // earlier in THIS transaction is a valid CAS target.
        let node = match self.read_own_node(node_id) {
            Ok(node) => node,
            Err(_) => {
                // Absent from the live view (never created, or deleted): report
                // the lost claim as `actual: None` rather than a `NodeNotFound`.
                return Err(TransactionError::CasMismatch {
                    expected: expected_version,
                    actual: None,
                }
                .into());
            }
        };

        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let valid_from = options.valid_from.unwrap_or(self.start_timestamp);
        validation::validate_valid_from_future(valid_from)?;

        // Match the update path's "valid_from not before creation" guard so
        // backdated CAS writes honor the same temporal invariant.
        let creation_time = {
            let historical = self.historical.read();
            historical.node_creation_time(node_id)
        };
        if let Some(creation_time) = creation_time {
            validation::validate_valid_from_not_before_creation(
                &format!("node:{}", node_id.as_u64()),
                creation_time,
                valid_from,
            )?;
        }

        let provenance = options
            .provenance
            .filter(|p| !p.is_empty())
            .map(std::sync::Arc::new);

        // Full REPLACE (no PATCH merge): the provided map becomes the node's
        // entire property state. Re-stamp the immutable namespace (#3349).
        let properties = namespace::restamp_namespace(properties, &node.properties);
        self.buffer.add(BufferedWrite::UpdateNode {
            node_id,
            version_id,
            label: node.label,
            properties,
            valid_from,
            provenance,
        })?;

        self.cas_preconditions.push(CasPrecondition {
            target: CasTarget::Node(node_id),
            expected_version,
            lease,
            fence,
        });

        Ok(version_id)
    }

    /// Core edge compare-and-set; endpoints and type are immutable (like
    /// `replace_edge`), only the property map is conditionally replaced. See
    /// [`cas_node_impl`](Self::cas_node_impl).
    pub(crate) fn cas_edge_impl(
        &mut self,
        edge_id: EdgeId,
        expected_version: VersionId,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<VersionId> {
        if self.state != TxState::Active {
            return Err(TransactionError::InvalidState {
                current: format!("{:?}", self.state),
                expected: "Active".to_string(),
            }
            .into());
        }

        // A namespace is immutable after creation: reject an explicit namespace
        // on an edge CAS rather than silently ignoring it (#3349).
        namespace::reject_namespace_on_update(options.namespace.as_ref())?;

        // Reject engine-reserved keys on the user-supplied replacement map
        // (Issue #3349); the immutable namespace is re-stamped below.
        namespace::reject_reserved_keys(&properties)?;

        let edge = match self.read_own_edge(edge_id) {
            Ok(edge) => edge,
            Err(_) => {
                return Err(TransactionError::CasMismatch {
                    expected: expected_version,
                    actual: None,
                }
                .into());
            }
        };

        let version_id = VersionId::new_unchecked(self.version_id_gen.next()?);
        let valid_from = options.valid_from.unwrap_or(self.start_timestamp);
        validation::validate_valid_from_future(valid_from)?;

        let creation_time = {
            let historical = self.historical.read();
            historical.edge_creation_time(edge_id)
        };
        if let Some(creation_time) = creation_time {
            validation::validate_valid_from_not_before_creation(
                &format!("edge:{}", edge_id.as_u64()),
                creation_time,
                valid_from,
            )?;
        }

        let provenance = options
            .provenance
            .filter(|p| !p.is_empty())
            .map(std::sync::Arc::new);

        // Full REPLACE of properties; source/target/label preserved. Re-stamp
        // the immutable namespace (#3349).
        let properties = namespace::restamp_namespace(properties, &edge.properties);
        self.buffer.add(BufferedWrite::UpdateEdge {
            edge_id,
            version_id,
            source: edge.source,
            target: edge.target,
            label: edge.label,
            properties,
            valid_from,
            provenance,
        })?;

        self.cas_preconditions.push(CasPrecondition {
            target: CasTarget::Edge(edge_id),
            expected_version,
            lease: None,
            fence: None,
        });

        Ok(version_id)
    }

    /// Claim a node iff it is unclaimed / its lease is expired OR the version
    /// still matches (Issue #3577). A thin convenience over [`cas_node_impl`]:
    /// it stamps `lease_owner_key = owner` and `lease_until_key =
    /// lease_until` (as integer microseconds) into `properties`, then buffers a
    /// CAS whose precondition is `current_version == expected_version` **OR** the
    /// current `lease_until` property is `<=` the commit timestamp.
    ///
    /// The property key names are caller-supplied (a convention, not a hardcoded
    /// schema). Lease expiry is evaluated against the commit HLC timestamp.
    ///
    /// Note: the lease is stored and compared at **wallclock-microsecond**
    /// granularity — `lease_until.wallclock()` drops the HLC logical component,
    /// as does the commit-timestamp comparison in `lease_until_expired_at` — so
    /// two leases differing only in their logical tick compare equal.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn claim_with_lease_impl(
        &mut self,
        node_id: NodeId,
        expected_version: VersionId,
        lease_owner_key: &str,
        lease_until_key: &str,
        owner: PropertyValue,
        lease_until: Timestamp,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<VersionId> {
        // Stamp the lease owner + expiry onto the claim's full-replace map.
        // Interning the caller-supplied keys mirrors how the write path stores
        // property keys and validates them up front.
        let owner_key = GLOBAL_INTERNER.intern(lease_owner_key)?;
        let until_key = GLOBAL_INTERNER.intern(lease_until_key)?;
        let claimed = PropertyMapBuilder::from_map(properties)
            .insert_by_key(owner_key, owner)
            .insert_by_key(until_key, PropertyValue::Int(lease_until.wallclock()))
            .build();

        self.cas_node_impl(
            node_id,
            expected_version,
            claimed,
            Some(LeaseCondition {
                lease_until_key: lease_until_key.to_string(),
            }),
            None,
            options,
        )
    }

    /// Fenced claim (DBOS Phase 3e): a [`claim_with_lease_impl`] that ALSO
    /// enforces a server-side monotonic fence and computes the lease deadline
    /// on the **DB** clock.
    ///
    /// Two safety extensions over `claim_with_lease_impl`:
    ///
    /// - **Piece 1 (fence).** The claim stamps `fence_key = new_fence` and is
    ///   admitted at commit only if `new_fence` is **strictly greater** than the
    ///   entity's committed fence (re-read under the commit-serialization guard).
    ///   This makes the stale-fence steal collision impossible (see
    ///   [`FenceCondition`]); a violation aborts with
    ///   [`TransactionError::FenceTooLow`](crate::core::error::TransactionError::FenceTooLow).
    /// - **Piece 2 (DB-side lease deadline).** `lease_until` is computed as
    ///   `time::now() + lease_ttl` on the **engine** HLC, not the caller's
    ///   clock — a skewed-fast executor can no longer install a far-future,
    ///   un-stealable lease. `time::now()` is taken at buffer-build time (holding
    ///   no locks, before the `current_timestamp → wal → historical` chain); it
    ///   is within the transaction's own microsecond-scale duration of the commit
    ///   HLC and, being at most that sliver earlier, yields an if-anything-shorter
    ///   lease — the safe direction for liveness.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn claim_with_lease_fenced_impl(
        &mut self,
        node_id: NodeId,
        expected_version: VersionId,
        lease_owner_key: &str,
        lease_until_key: &str,
        fence_key: &str,
        owner: PropertyValue,
        lease_ttl: std::time::Duration,
        new_fence: i64,
        properties: PropertyMap,
        options: WriteRequestOptions,
    ) -> Result<VersionId> {
        // Piece 2: compute the lease deadline on the DB clock (buffer-build
        // time), never the caller's. Saturating so a huge TTL cannot overflow.
        let ttl_us = i64::try_from(lease_ttl.as_micros()).unwrap_or(i64::MAX);
        let lease_until_us = time::now().wallclock().saturating_add(ttl_us);

        // Stamp owner, DB-computed lease deadline, and the fence token into the
        // claim's full-replace map. Interning mirrors the write path.
        let owner_key = GLOBAL_INTERNER.intern(lease_owner_key)?;
        let until_key = GLOBAL_INTERNER.intern(lease_until_key)?;
        let fkey = GLOBAL_INTERNER.intern(fence_key)?;
        let claimed = PropertyMapBuilder::from_map(properties)
            .insert_by_key(owner_key, owner)
            .insert_by_key(until_key, PropertyValue::Int(lease_until_us))
            .insert_by_key(fkey, PropertyValue::Int(new_fence))
            .build();

        self.cas_node_impl(
            node_id,
            expected_version,
            claimed,
            Some(LeaseCondition {
                lease_until_key: lease_until_key.to_string(),
            }),
            Some(FenceCondition {
                fence_key: fence_key.to_string(),
                new_fence,
            }),
            options,
        )
    }
}

/// Commit-time re-check that every buffered CAS precondition still holds, run
/// under the exclusive `historical.write()` guard (Issue #3577).
///
/// For each precondition, the entity's committed head version is re-read from
/// current storage (authoritative because the `historical.write()` guard
/// serializes commits, so committed current state reflects every
/// earlier-committed transaction; `current_version` there tracks the historical
/// head). The precondition passes iff:
///
/// - the head equals `expected_version`, OR
/// - (lease claims only) the entity's current `lease_until` property is `<=`
///   `commit_timestamp` — the lease is expired / unclaimed.
///
/// On the first violation the whole transaction aborts with
/// [`TransactionError::CasMismatch`] (MCP `FAILED_PRECONDITION`, non-retriable)
/// and no buffered op is applied (this runs before any op is applied, mirroring
/// the #3416 under-guard checks).
///
/// # Locking
///
/// Called while `historical.write()` (order class 3) is held. Reads the
/// committed head and (for a lease branch) the entity's committed properties
/// from current storage (a leaf, class 6/7) — the same leaf the #3416 under-guard
/// checks read — never calling back into `historical`/`wal`/`current_timestamp`,
/// adding no new lock site, and introducing no lock-order inversion.
pub(crate) fn detect_cas_precondition_violations(
    tx: &WriteTransaction,
    commit_timestamp: Timestamp,
) -> Result<()> {
    for precondition in &tx.cas_preconditions {
        match precondition.target {
            CasTarget::Node(node_id) => {
                // `actual` is presence-aware: the committed head version if the
                // node is live in current storage, else `None` (deleted /
                // never-existed). Current-storage `current_version` tracks the
                // historical head (both are written together in
                // `apply_node_write`), so this is the committed current version.
                let actual = tx.current.get_node(node_id).ok().map(|n| n.current_version);
                // Claim gate: version matches OR (lease claim) the lease is
                // expired / unclaimed at the commit timestamp.
                let claim_ok = actual == Some(precondition.expected_version)
                    || precondition.lease.as_ref().is_some_and(|lease| {
                        node_lease_expired(tx, node_id, &lease.lease_until_key, commit_timestamp)
                    });
                if !claim_ok {
                    return Err(TransactionError::CasMismatch {
                        expected: precondition.expected_version,
                        actual,
                    }
                    .into());
                }
                // Fence gate (DBOS Phase 3e): AND-composed with the claim gate.
                // The committed fence is re-read under this guard, so a stale
                // `new_fence` computed from an earlier queue read is rejected —
                // making the stale-fence steal collision impossible.
                if let Some(fence) = &precondition.fence {
                    let stored = node_stored_fence(tx, node_id, &fence.fence_key);
                    if !fence_beats_stored(fence.new_fence, stored) {
                        return Err(TransactionError::FenceTooLow {
                            fence_key: fence.fence_key.clone(),
                            new_fence: fence.new_fence,
                            stored,
                        }
                        .into());
                    }
                }
                continue;
            }
            CasTarget::Edge(edge_id) => {
                let actual = tx.current.get_edge(edge_id).ok().map(|e| e.current_version);
                if actual == Some(precondition.expected_version) {
                    continue;
                }
                // Edge CAS has no lease branch (leases are a node-claim convention).
                return Err(TransactionError::CasMismatch {
                    expected: precondition.expected_version,
                    actual,
                }
                .into());
            }
        }
    }
    Ok(())
}

/// Whether the committed node's `lease_until_key` property denotes an expired
/// (or absent) lease relative to `commit_timestamp`.
///
/// A missing property, or a non-integer value, is treated as **unclaimed** (the
/// lease branch passes). An entity absent from current storage is NOT treated as
/// expired (there is nothing to claim there) — the caller falls through to
/// `CasMismatch`.
fn node_lease_expired(
    tx: &WriteTransaction,
    node_id: NodeId,
    lease_until_key: &str,
    commit_timestamp: Timestamp,
) -> bool {
    match tx.current.get_node(node_id) {
        Ok(node) => {
            // A missing key OR a non-integer value (e.g. a String `lease_until`)
            // maps to `None` here, which the predicate treats as unclaimed —
            // admitting the claim. This is intentional: a malformed lease is not
            // a valid hold.
            let until_us = node
                .properties
                .get(lease_until_key)
                .and_then(|v| v.as_int());
            lease_until_expired_at(until_us, commit_timestamp.wallclock())
        }
        Err(_) => false,
    }
}

/// Read a committed node's monotonic fence (DBOS Phase 3e): the integer value
/// of its `fence_key` property, or `i64::MIN` when the node is absent, the key
/// is missing, or the value is non-integer (all meaning "no fence held", so any
/// non-negative `new_fence` strictly beats it and a first claim is admitted).
fn node_stored_fence(tx: &WriteTransaction, node_id: NodeId, fence_key: &str) -> i64 {
    match tx.current.get_node(node_id) {
        Ok(node) => node
            .properties
            .get(fence_key)
            .and_then(|v| v.as_int())
            .unwrap_or(i64::MIN),
        Err(_) => i64::MIN,
    }
}

/// Pure boundary predicate for the monotonic fence gate (DBOS Phase 3e): does
/// `new_fence` **strictly beat** the committed `stored` fence?
///
/// A claim is admitted only when `new_fence > stored`, so an equal fence is
/// rejected (`8 > 8` is false — this is what makes a stale-fence steal
/// collision impossible: two stealers computing the same value cannot both
/// commit) and a strictly-greater one accepted. When no fence is held the
/// caller passes `stored == i64::MIN` (see [`node_stored_fence`]), so any
/// non-negative `new_fence` beats it and a first claim is admitted. Extracted
/// as a pure fn so the exact `>` boundary is unit-testable without a live
/// transaction, mirroring [`lease_until_expired_at`].
fn fence_beats_stored(new_fence: i64, stored: i64) -> bool {
    new_fence > stored
}

/// Pure boundary predicate for lease expiry: is a `lease_until` of `until_us`
/// microseconds (or `None` for absent/non-integer, i.e. unclaimed) expired at
/// commit wallclock `commit_wallclock`?
///
/// The lease is considered **expired** (claim admitted) iff `until_us <=
/// commit_wallclock` — so a lease whose expiry instant is *exactly* the commit
/// timestamp counts as EXPIRED (boundary is inclusive), and one microsecond
/// later counts as HELD. `None` (no integer lease recorded) is unclaimed →
/// expired. Extracted as a pure fn so the exact boundary is unit-testable
/// without constructing a live transaction.
fn lease_until_expired_at(until_us: Option<i64>, commit_wallclock: i64) -> bool {
    match until_us {
        Some(until_us) => until_us <= commit_wallclock,
        None => true,
    }
}

#[cfg(test)]
mod lease_boundary_tests {
    use super::lease_until_expired_at;

    #[test]
    fn lease_until_equal_to_commit_is_expired() {
        // Boundary: lease_until == commit wallclock. The code uses `<=`, so this
        // must count as EXPIRED (claim admitted).
        assert!(
            lease_until_expired_at(Some(1_000), 1_000),
            "lease_until == commit must be EXPIRED (inclusive boundary)"
        );
    }

    #[test]
    fn lease_until_one_micro_after_commit_is_held() {
        // lease_until == commit + 1 microsecond -> still HELD.
        assert!(
            !lease_until_expired_at(Some(1_001), 1_000),
            "lease_until == commit + 1us must be HELD"
        );
    }

    #[test]
    fn lease_until_before_commit_is_expired() {
        assert!(lease_until_expired_at(Some(999), 1_000));
    }

    #[test]
    fn absent_or_non_integer_lease_is_unclaimed() {
        // `None` models both a missing key and a non-integer value (e.g. a
        // String `lease_until`, which `as_int()` yields `None` for): unclaimed.
        assert!(
            lease_until_expired_at(None, 1_000),
            "absent / non-integer lease must be treated as unclaimed (expired)"
        );
    }
}

#[cfg(test)]
mod fence_boundary_tests {
    use super::fence_beats_stored;

    #[test]
    fn fence_equal_to_stored_is_rejected() {
        // Boundary: new_fence == stored. The gate is strict `>`, so an equal
        // fence must NOT beat the stored one — this is what makes the
        // stale-fence steal collision impossible (two stealers computing the
        // same value cannot both commit).
        assert!(
            !fence_beats_stored(8, 8),
            "new_fence == stored must be rejected (strict > boundary)"
        );
    }

    #[test]
    fn fence_greater_than_stored_is_accepted() {
        assert!(
            fence_beats_stored(9, 8),
            "new_fence strictly greater than stored must be accepted"
        );
    }

    #[test]
    fn any_fence_beats_absent_stored() {
        // `node_stored_fence` maps a missing / non-integer / absent-node fence
        // to `i64::MIN`, so a first claim's fence always beats it.
        assert!(
            fence_beats_stored(1, i64::MIN),
            "a positive fence must beat the i64::MIN 'no fence held' sentinel"
        );
        assert!(
            fence_beats_stored(0, i64::MIN),
            "even fence 0 must beat the i64::MIN 'no fence held' sentinel"
        );
    }
}
