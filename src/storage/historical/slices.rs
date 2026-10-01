//! Valid-time slice bookkeeping for updates (append-only supersession).
//!
//! # The problem
//!
//! The version chain records supersession on the **transaction-time** axis: an
//! update closes the previous version's transaction interval at the commit
//! timestamp. Issue #3504 deliberately stopped closing the previous version's
//! *valid* interval in place, because shrinking it would retroactively change
//! what earlier transaction-time snapshots observe.
//!
//! Closing only the transaction interval, however, loses the current belief
//! about the valid-time prefix the update did *not* touch. After
//! `create(valid_from = t1)` then `update(valid_from = t2)`, an as-of read at
//! `(valid = mid, tx = now)` with `t1 <= mid < t2` found no version at all.
//!
//! # The model
//!
//! The superseded version's full belief is an L-shaped region of the
//! bi-temporal plane:
//!
//! ```text
//!   tx [c1, c2) x valid [t1, inf)   -- what we believed before the update
//!   tx [c2, inf) x valid [t1, t2)   -- what we still believe after it
//! ```
//!
//! A version is one rectangle, so the update keeps the original version as-is
//! (transaction-closed at `c2`, valid interval untouched — #3504 holds) and
//! appends a **carry-forward** version with the predecessor's properties over
//! `[t1, t2)` recorded at `c2`. The *still-recorded* (transaction-open)
//! versions of a live entity — its **slices** — then partition valid time,
//! non-overlapping and gap-free, so every as-of valid-time read at the current
//! transaction time resolves to exactly one version.
//!
//! An update with `valid_from = t`:
//!
//! 1. finds the slice `P` whose valid interval contains `t`;
//! 2. transaction-closes `P` and, when `P` starts before `t`, appends the
//!    carry-forward `[P.start, t)`; when `P` starts exactly at `t` the update is
//!    a degenerate replacement and no carry-forward is needed;
//! 3. appends the new version over `[t, P.end)` — open-ended for an ordinary
//!    update, closed for a **backfill** landing before later slices, so the
//!    successor slices are never corrupted;
//! 4. after a backfill, re-asserts the open-ended head slice (transaction-close
//!    it, append an identical copy) so the head of the chain is always the
//!    entity's current-state version.
//!
//! Carry-forward and re-assertion versions are **structural**: their ids carry
//! the [`STRUCTURAL_VERSION_TAG`](crate::core::id::STRUCTURAL_VERSION_TAG) bit,
//! which survives every persistence format, so the pull changefeed can skip
//! them. A delete transaction-closes *every* slice so a deleted entity remains
//! absent at every valid time as of the deletion, exactly as before.

use super::{FastHashMap, HistoricalStorage, Result, StorageError};
use crate::core::history::VersionInfo;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::interning::InternedString;
use crate::core::property::PropertyMap;
use crate::core::provenance::Provenance;
use crate::core::temporal::{BiTemporalInterval, Timestamp};
use std::sync::Arc;

/// `true` if `temporal` is a still-recorded slice of current belief: its
/// transaction interval is open and its valid interval is non-empty.
#[inline]
pub(crate) fn is_open_slice(temporal: &BiTemporalInterval) -> bool {
    temporal.is_currently_recorded() && temporal.valid_time().start() < temporal.valid_time().end()
}

/// How an update with a given `valid_from` supersedes an entity's slices.
///
/// Computed read-only by [`HistoricalStorage::plan_node_update`] /
/// [`HistoricalStorage::plan_edge_update`] and executed by
/// [`HistoricalStorage::apply_node_update`] / [`HistoricalStorage::apply_edge_update`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct UpdatePlan {
    /// The slice whose valid interval contains the new `valid_from`; it is
    /// transaction-closed by the update.
    pub predecessor: Option<VersionId>,
    /// Start of the carry-forward slice `[carry_from, valid_from)`, when the
    /// predecessor starts strictly before `valid_from`.
    pub carry_from: Option<Timestamp>,
    /// Valid-time end of the new version; `None` means open-ended.
    pub new_valid_to: Option<Timestamp>,
    /// The open-ended head slice to re-assert after a backfill.
    pub reassert: Option<VersionId>,
}

#[cfg(test)]
impl UpdatePlan {
    /// Number of structural version ids executing this plan consumes.
    #[inline]
    pub(crate) fn structural_ids_needed(&self) -> usize {
        usize::from(self.carry_from.is_some()) + usize::from(self.reassert.is_some())
    }
}

/// What [`HistoricalStorage::apply_node_update`] /
/// [`HistoricalStorage::apply_edge_update`] appended.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppliedUpdate {
    /// Every appended version, in chain order, with its bi-temporal interval
    /// (for temporal-index insertion by the caller).
    pub appended: Vec<(VersionId, BiTemporalInterval)>,
    /// When the head was re-asserted (backfill), the re-assertion version, its
    /// label and properties: current storage must point at it, because the
    /// entity's current state did not change.
    pub reasserted: Option<(VersionId, InternedString, PropertyMap)>,
}

/// Resolve an interned label for display (mirrors the history builders).
fn resolve_label(label: InternedString) -> String {
    crate::core::interning::GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_else(|| label.to_string())
}

/// Plan an update from the head and the other still-recorded slices.
fn plan_update(
    head: Option<(VersionId, BiTemporalInterval)>,
    other_slices: impl Iterator<Item = (VersionId, BiTemporalInterval)>,
    valid_from: Timestamp,
) -> UpdatePlan {
    // Fast path (every ordinary update): the open-ended head contains
    // `valid_from`, so no other slice needs to be inspected.
    let head_slice = head.filter(|(_, t)| is_open_slice(t));
    let mut predecessor = head_slice.filter(|(_, t)| t.valid_time().contains(valid_from));
    let mut next_start: Option<Timestamp> = None;

    if predecessor.is_none() {
        for (id, temporal) in head_slice.into_iter().chain(other_slices) {
            if !is_open_slice(&temporal) {
                continue;
            }
            let valid = temporal.valid_time();
            if valid.contains(valid_from) {
                predecessor = Some((id, temporal));
                break;
            }
            if valid.start() > valid_from {
                next_start = Some(next_start.map_or(valid.start(), |n| n.min(valid.start())));
            }
        }
    }

    let new_valid_to = match predecessor {
        Some((_, t)) if !t.valid_time().is_current() => Some(t.valid_time().end()),
        Some(_) => None,
        None => next_start,
    };
    let carry_from = predecessor
        .map(|(_, t)| t.valid_time().start())
        .filter(|start| *start < valid_from);
    let reassert = match (new_valid_to, head_slice) {
        (Some(_), Some((head_id, head_t)))
            if head_t.valid_time().is_current() && predecessor.map(|p| p.0) != Some(head_id) =>
        {
            Some(head_id)
        }
        _ => None,
    };

    UpdatePlan {
        predecessor: predecessor.map(|(id, _)| id),
        carry_from,
        new_valid_to,
        reassert,
    }
}

/// Resolve the structural id for a plan step, failing loudly if the caller
/// did not supply one.
fn require_id(id: Option<VersionId>, what: &str) -> Result<VersionId> {
    id.ok_or_else(|| {
        StorageError::InconsistentState {
            reason: format!("update plan requires a {what} version id but none was supplied"),
        }
        .into()
    })
}

/// Remove `version_id` from `entity`'s open-slice list.
fn untrack<K: std::hash::Hash + Eq + Copy>(
    map: &mut FastHashMap<K, Vec<VersionId>>,
    entity: K,
    version_id: VersionId,
) {
    if let Some(slices) = map.get_mut(&entity) {
        slices.retain(|v| *v != version_id);
        if slices.is_empty() {
            map.remove(&entity);
        }
    }
}

impl HistoricalStorage {
    // --- Open-slice index maintenance --------------------------------------

    /// Track `version_id` as a non-head open slice of `node_id` if it is one.
    pub(super) fn track_node_slice_if_open(&mut self, node_id: NodeId, version_id: VersionId) {
        if self
            .node_versions
            .get(&version_id)
            .is_some_and(|v| is_open_slice(&v.temporal))
        {
            let slices = self.node_open_slices.entry(node_id).or_default();
            if !slices.contains(&version_id) {
                slices.push(version_id);
            }
        }
    }

    /// Track `version_id` as a non-head open slice of `edge_id` if it is one.
    pub(super) fn track_edge_slice_if_open(&mut self, edge_id: EdgeId, version_id: VersionId) {
        if self
            .edge_versions
            .get(&version_id)
            .is_some_and(|v| is_open_slice(&v.temporal))
        {
            let slices = self.edge_open_slices.entry(edge_id).or_default();
            if !slices.contains(&version_id) {
                slices.push(version_id);
            }
        }
    }

    /// Stop tracking `version_id` as an open slice of `node_id`.
    pub(super) fn untrack_node_slice(&mut self, node_id: NodeId, version_id: VersionId) {
        untrack(&mut self.node_open_slices, node_id, version_id);
    }

    /// Stop tracking `version_id` as an open slice of `edge_id`.
    pub(super) fn untrack_edge_slice(&mut self, edge_id: EdgeId, version_id: VersionId) {
        untrack(&mut self.edge_open_slices, edge_id, version_id);
    }

    /// Rebuild the open-slice indexes from the hot version maps (after a
    /// restore). A slice is any still-recorded, non-empty-valid version that is
    /// not its entity's head.
    pub(super) fn rebuild_open_slices(&mut self) {
        self.node_open_slices.clear();
        for (vid, v) in &self.node_versions {
            if is_open_slice(&v.temporal) && self.node_version_heads.get(&v.node_id) != Some(vid) {
                self.node_open_slices
                    .entry(v.node_id)
                    .or_default()
                    .push(*vid);
            }
        }
        self.edge_open_slices.clear();
        for (vid, v) in &self.edge_versions {
            if is_open_slice(&v.temporal) && self.edge_version_heads.get(&v.edge_id) != Some(vid) {
                self.edge_open_slices
                    .entry(v.edge_id)
                    .or_default()
                    .push(*vid);
            }
        }
    }

    /// The still-recorded valid-time slices of a node that are not its head.
    pub(crate) fn node_open_slices(&self, node_id: NodeId) -> &[VersionId] {
        self.node_open_slices
            .get(&node_id)
            .map_or(&[][..], Vec::as_slice)
    }

    /// The still-recorded valid-time slices of an edge that are not its head.
    pub(crate) fn edge_open_slices(&self, edge_id: EdgeId) -> &[VersionId] {
        self.edge_open_slices
            .get(&edge_id)
            .map_or(&[][..], Vec::as_slice)
    }

    /// Hot versions of a node that count against the per-entity cap: every
    /// version except structural ones.
    pub(super) fn node_logical_version_count(&self, node_id: NodeId) -> usize {
        let all = self.node_version_counts.get(&node_id).copied().unwrap_or(0);
        let structural = self
            .node_structural_counts
            .get(&node_id)
            .copied()
            .unwrap_or(0);
        all.saturating_sub(structural)
    }

    /// Edge counterpart of [`node_logical_version_count`](Self::node_logical_version_count).
    pub(super) fn edge_logical_version_count(&self, edge_id: EdgeId) -> usize {
        let all = self.edge_version_counts.get(&edge_id).copied().unwrap_or(0);
        let structural = self
            .edge_structural_counts
            .get(&edge_id)
            .copied()
            .unwrap_or(0);
        all.saturating_sub(structural)
    }

    // --- Current-belief view -----------------------------------------------

    /// The node's current belief over valid time: every still-recorded slice
    /// (the head plus carry-forwards), sorted by valid start. For a live node
    /// the slices partition `[creation, inf)` with no gaps or overlaps.
    ///
    /// `version_number` is the 1-based position in this list (not a history
    /// number). Structural slices are included — they are exactly what records
    /// an earlier write's content over a now-bounded valid interval.
    pub fn get_node_valid_time_slices(&self, node_id: NodeId) -> Result<Vec<VersionInfo>> {
        let mut ids: Vec<VersionId> = self.node_open_slices(node_id).to_vec();
        ids.extend(self.node_version_heads.get(&node_id).copied());
        let mut slices: Vec<(VersionId, &crate::core::version::NodeVersion)> = ids
            .into_iter()
            .filter_map(|id| self.node_versions.get(&id).map(|v| (id, v)))
            .filter(|(_, v)| is_open_slice(&v.temporal))
            .collect();
        slices.sort_by_key(|(_, v)| v.temporal.valid_time().start());
        slices
            .into_iter()
            .enumerate()
            .map(|(i, (id, v))| {
                Ok(VersionInfo {
                    version_number: (i + 1) as u64,
                    version_id: id,
                    temporal: v.temporal,
                    properties: self.reconstruct_node_properties(id)?,
                    label: resolve_label(v.label),
                    provenance: v.provenance.as_deref().cloned(),
                })
            })
            .collect()
    }

    /// Edge counterpart of [`get_node_valid_time_slices`](Self::get_node_valid_time_slices).
    pub fn get_edge_valid_time_slices(&self, edge_id: EdgeId) -> Result<Vec<VersionInfo>> {
        let mut ids: Vec<VersionId> = self.edge_open_slices(edge_id).to_vec();
        ids.extend(self.edge_version_heads.get(&edge_id).copied());
        let mut slices: Vec<(VersionId, &crate::core::version::EdgeVersion)> = ids
            .into_iter()
            .filter_map(|id| self.edge_versions.get(&id).map(|v| (id, v)))
            .filter(|(_, v)| is_open_slice(&v.temporal))
            .collect();
        slices.sort_by_key(|(_, v)| v.temporal.valid_time().start());
        slices
            .into_iter()
            .enumerate()
            .map(|(i, (id, v))| {
                Ok(VersionInfo {
                    version_number: (i + 1) as u64,
                    version_id: id,
                    temporal: v.temporal,
                    properties: self.reconstruct_edge_properties(id)?,
                    label: resolve_label(v.label),
                    provenance: v.provenance.as_deref().cloned(),
                })
            })
            .collect()
    }

    // --- Planning ----------------------------------------------------------

    /// Plan how an update of `node_id` with `valid_from` supersedes its slices.
    /// Read-only; see the module docs for the model.
    pub(crate) fn plan_node_update(&self, node_id: NodeId, valid_from: Timestamp) -> UpdatePlan {
        let head = self
            .node_version_heads
            .get(&node_id)
            .and_then(|id| self.node_versions.get(id).map(|v| (*id, v.temporal)));
        let others = self
            .node_open_slices(node_id)
            .iter()
            .filter_map(|id| self.node_versions.get(id).map(|v| (*id, v.temporal)));
        plan_update(head, others, valid_from)
    }

    /// Plan how an update of `edge_id` with `valid_from` supersedes its slices.
    pub(crate) fn plan_edge_update(&self, edge_id: EdgeId, valid_from: Timestamp) -> UpdatePlan {
        let head = self
            .edge_version_heads
            .get(&edge_id)
            .and_then(|id| self.edge_versions.get(id).map(|v| (*id, v.temporal)));
        let others = self
            .edge_open_slices(edge_id)
            .iter()
            .filter_map(|id| self.edge_versions.get(id).map(|v| (*id, v.temporal)));
        plan_update(head, others, valid_from)
    }

    // --- Execution ---------------------------------------------------------

    /// Reject the whole update up front if appending `needed` logical versions
    /// would exceed the per-entity cap, so a failure never leaves a
    /// half-applied update (the predecessor closed but the new version missing).
    fn check_update_capacity(&self, resource: String, count: usize, needed: usize) -> Result<()> {
        let limit = self.retention_policy.max_versions_per_entity;
        if count.saturating_add(needed) > limit {
            return Err(StorageError::CapacityExceeded {
                resource,
                current: count,
                limit,
            }
            .into());
        }
        Ok(())
    }

    /// Execute an [`UpdatePlan`] for a node: transaction-close the superseded
    /// slice(s), then append the carry-forward, the new version, and (after a
    /// backfill) the head re-assertion, in that chain order.
    ///
    /// `carry_id` / `reassert_id` must be supplied (structural ids) exactly when
    /// the plan needs them. Every append is recorded at `commit`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_node_update(
        &mut self,
        node_id: NodeId,
        plan: &UpdatePlan,
        valid_from: Timestamp,
        commit: Timestamp,
        version_id: VersionId,
        label: InternedString,
        properties: PropertyMap,
        provenance: Option<Arc<Provenance>>,
        carry_id: Option<VersionId>,
        reassert_id: Option<VersionId>,
    ) -> Result<AppliedUpdate> {
        // Only the logical write counts against the cap (structural versions
        // are exempt); checked before any mutation so a rejection never leaves
        // a half-applied update.
        let count = self.node_logical_version_count(node_id);
        self.check_update_capacity(format!("node {} versions", node_id), count, 1)?;

        // Gather the structural payloads before mutating anything.
        let carry = match (plan.predecessor, plan.carry_from) {
            (Some(pred), Some(from)) => {
                let v = self
                    .node_versions
                    .get(&pred)
                    .ok_or(StorageError::VersionNotFound(pred))?;
                let (pred_label, pred_prov) = (v.label, v.provenance.clone());
                let props = self.reconstruct_node_properties(pred)?;
                Some((
                    require_id(carry_id, "carry-forward")?,
                    from,
                    pred_label,
                    props,
                    pred_prov,
                ))
            }
            _ => None,
        };
        let reassert = match plan.reassert {
            Some(head) => {
                let v = self
                    .node_versions
                    .get(&head)
                    .ok_or(StorageError::VersionNotFound(head))?;
                let (h_label, h_prov, h_start) = (
                    v.label,
                    v.provenance.clone(),
                    v.temporal.valid_time().start(),
                );
                let props = self.reconstruct_node_properties(head)?;
                Some((
                    require_id(reassert_id, "re-assertion")?,
                    h_start,
                    h_label,
                    props,
                    h_prov,
                ))
            }
            None => None,
        };

        // Transaction-close the superseded slice(s). With no predecessor slice
        // the head (if still recorded) is superseded exactly as before.
        let head = self.node_version_heads.get(&node_id).copied();
        let mut to_close: Vec<VersionId> = Vec::with_capacity(2);
        to_close.extend(plan.predecessor);
        to_close.extend(plan.reassert);
        if plan.predecessor.is_none() && plan.reassert.is_none() {
            to_close.extend(head);
        }
        for vid in to_close {
            if self
                .node_versions
                .get(&vid)
                .is_some_and(|v| v.temporal.is_currently_recorded())
            {
                self.close_node_version_transaction_time(vid, commit)?;
            }
        }

        let mut applied = AppliedUpdate::default();
        if let Some((id, from, c_label, props, prov)) = carry {
            let temporal =
                BiTemporalInterval::with_valid_time(from, commit).close_valid_time(valid_from)?;
            self.add_node_version_with_interval(node_id, id, temporal, c_label, props, prov)?;
            applied.appended.push((id, temporal));
        }

        let mut temporal = BiTemporalInterval::with_valid_time(valid_from, commit);
        if let Some(end) = plan.new_valid_to {
            temporal = temporal.close_valid_time(end)?;
        }
        self.add_node_version_with_interval(
            node_id, version_id, temporal, label, properties, provenance,
        )?;
        applied.appended.push((version_id, temporal));

        if let Some((id, from, h_label, props, prov)) = reassert {
            let temporal = BiTemporalInterval::with_valid_time(from, commit);
            self.add_node_version_with_interval(
                node_id,
                id,
                temporal,
                h_label,
                props.clone(),
                prov,
            )?;
            applied.appended.push((id, temporal));
            applied.reasserted = Some((id, h_label, props));
        }
        Ok(applied)
    }

    /// Edge counterpart of [`apply_node_update`](Self::apply_node_update).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_edge_update(
        &mut self,
        edge_id: EdgeId,
        plan: &UpdatePlan,
        valid_from: Timestamp,
        commit: Timestamp,
        version_id: VersionId,
        label: InternedString,
        source: NodeId,
        target: NodeId,
        properties: PropertyMap,
        provenance: Option<Arc<Provenance>>,
        carry_id: Option<VersionId>,
        reassert_id: Option<VersionId>,
    ) -> Result<AppliedUpdate> {
        // Only the logical write counts against the cap (structural versions
        // are exempt); checked before any mutation so a rejection never leaves
        // a half-applied update.
        let count = self.edge_logical_version_count(edge_id);
        self.check_update_capacity(format!("edge {} versions", edge_id), count, 1)?;

        let carry = match (plan.predecessor, plan.carry_from) {
            (Some(pred), Some(from)) => {
                let v = self
                    .edge_versions
                    .get(&pred)
                    .ok_or(StorageError::VersionNotFound(pred))?;
                let (pred_label, pred_prov) = (v.label, v.provenance.clone());
                let props = self.reconstruct_edge_properties(pred)?;
                Some((
                    require_id(carry_id, "carry-forward")?,
                    from,
                    pred_label,
                    props,
                    pred_prov,
                ))
            }
            _ => None,
        };
        let reassert = match plan.reassert {
            Some(head) => {
                let v = self
                    .edge_versions
                    .get(&head)
                    .ok_or(StorageError::VersionNotFound(head))?;
                let (h_label, h_prov, h_start) = (
                    v.label,
                    v.provenance.clone(),
                    v.temporal.valid_time().start(),
                );
                let props = self.reconstruct_edge_properties(head)?;
                Some((
                    require_id(reassert_id, "re-assertion")?,
                    h_start,
                    h_label,
                    props,
                    h_prov,
                ))
            }
            None => None,
        };

        let head = self.edge_version_heads.get(&edge_id).copied();
        let mut to_close: Vec<VersionId> = Vec::with_capacity(2);
        to_close.extend(plan.predecessor);
        to_close.extend(plan.reassert);
        if plan.predecessor.is_none() && plan.reassert.is_none() {
            to_close.extend(head);
        }
        for vid in to_close {
            if self
                .edge_versions
                .get(&vid)
                .is_some_and(|v| v.temporal.is_currently_recorded())
            {
                self.close_edge_version_transaction_time(vid, commit)?;
            }
        }

        let mut applied = AppliedUpdate::default();
        if let Some((id, from, c_label, props, prov)) = carry {
            let temporal =
                BiTemporalInterval::with_valid_time(from, commit).close_valid_time(valid_from)?;
            self.add_edge_version_with_interval(
                edge_id, id, temporal, c_label, source, target, props, false, prov,
            )?;
            applied.appended.push((id, temporal));
        }

        let mut temporal = BiTemporalInterval::with_valid_time(valid_from, commit);
        if let Some(end) = plan.new_valid_to {
            temporal = temporal.close_valid_time(end)?;
        }
        self.add_edge_version_with_interval(
            edge_id, version_id, temporal, label, source, target, properties, false, provenance,
        )?;
        applied.appended.push((version_id, temporal));

        if let Some((id, from, h_label, props, prov)) = reassert {
            let temporal = BiTemporalInterval::with_valid_time(from, commit);
            self.add_edge_version_with_interval(
                edge_id,
                id,
                temporal,
                h_label,
                source,
                target,
                props.clone(),
                false,
                prov,
            )?;
            applied.appended.push((id, temporal));
            applied.reasserted = Some((id, h_label, props));
        }
        Ok(applied)
    }

    /// Transaction-close the head and every other still-recorded slice of a
    /// node at `commit` (delete: the entity is withdrawn at every valid time).
    pub(crate) fn close_all_node_slices(
        &mut self,
        node_id: NodeId,
        commit: Timestamp,
    ) -> Result<()> {
        let mut ids: Vec<VersionId> = self.node_open_slices(node_id).to_vec();
        ids.extend(self.node_version_heads.get(&node_id).copied());
        for vid in ids {
            if self
                .node_versions
                .get(&vid)
                .is_some_and(|v| v.temporal.is_currently_recorded())
            {
                self.close_node_version_transaction_time(vid, commit)?;
            }
        }
        Ok(())
    }

    /// Edge counterpart of [`close_all_node_slices`](Self::close_all_node_slices).
    pub(crate) fn close_all_edge_slices(
        &mut self,
        edge_id: EdgeId,
        commit: Timestamp,
    ) -> Result<()> {
        let mut ids: Vec<VersionId> = self.edge_open_slices(edge_id).to_vec();
        ids.extend(self.edge_version_heads.get(&edge_id).copied());
        for vid in ids {
            if self
                .edge_versions
                .get(&vid)
                .is_some_and(|v| v.temporal.is_currently_recorded())
            {
                self.close_edge_version_transaction_time(vid, commit)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::temporal::TIMESTAMP_MAX;

    fn ts(t: i64) -> Timestamp {
        HybridTimestamp::new(t, 0).unwrap()
    }

    fn vid(v: u64) -> VersionId {
        VersionId::new(v).unwrap()
    }

    /// A still-recorded slice `[start, end)` (`end = None` => open-ended).
    fn slice(start: i64, end: Option<i64>) -> BiTemporalInterval {
        let t = BiTemporalInterval::with_valid_time(ts(start), ts(1));
        match end {
            Some(e) => t.close_valid_time(ts(e)).unwrap(),
            None => t,
        }
    }

    #[test]
    fn ordinary_update_splits_open_head() {
        let plan = plan_update(Some((vid(1), slice(10, None))), std::iter::empty(), ts(20));
        assert_eq!(
            plan,
            UpdatePlan {
                predecessor: Some(vid(1)),
                carry_from: Some(ts(10)),
                new_valid_to: None,
                reassert: None,
            }
        );
        assert_eq!(plan.structural_ids_needed(), 1);
    }

    #[test]
    fn equal_valid_from_is_degenerate_replace() {
        let plan = plan_update(Some((vid(1), slice(10, None))), std::iter::empty(), ts(10));
        assert_eq!(plan.predecessor, Some(vid(1)));
        assert_eq!(plan.carry_from, None);
        assert_eq!(plan.new_valid_to, None);
        assert_eq!(plan.structural_ids_needed(), 0);
    }

    #[test]
    fn backfill_is_bounded_by_successor_and_reasserts_head() {
        // Slices: [0,10) v1, [10,20) v2, [20,inf) head v3. Backfill at 15.
        let others = [(vid(1), slice(0, Some(10))), (vid(2), slice(10, Some(20)))];
        let plan = plan_update(Some((vid(3), slice(20, None))), others.into_iter(), ts(15));
        assert_eq!(
            plan,
            UpdatePlan {
                predecessor: Some(vid(2)),
                carry_from: Some(ts(10)),
                new_valid_to: Some(ts(20)),
                reassert: Some(vid(3)),
            }
        );
        assert_eq!(plan.structural_ids_needed(), 2);
    }

    #[test]
    fn backfill_at_slice_start_replaces_that_slice_only() {
        let others = [(vid(1), slice(0, Some(10)))];
        let plan = plan_update(Some((vid(2), slice(10, None))), others.into_iter(), ts(0));
        assert_eq!(plan.predecessor, Some(vid(1)));
        assert_eq!(plan.carry_from, None);
        assert_eq!(plan.new_valid_to, Some(ts(10)));
        assert_eq!(plan.reassert, Some(vid(2)));
    }

    #[test]
    fn update_before_every_slice_is_bounded_by_the_first() {
        let plan = plan_update(Some((vid(1), slice(10, None))), std::iter::empty(), ts(5));
        assert_eq!(plan.predecessor, None);
        assert_eq!(plan.carry_from, None);
        assert_eq!(plan.new_valid_to, Some(ts(10)));
        assert_eq!(plan.reassert, Some(vid(1)));
    }

    #[test]
    fn no_live_slice_falls_back_to_plain_append() {
        // Head is a tombstone (empty valid interval): not a slice.
        let tomb = BiTemporalInterval::with_valid_time(ts(10), ts(1))
            .close_valid_time(ts(10))
            .unwrap();
        let plan = plan_update(Some((vid(1), tomb)), std::iter::empty(), ts(20));
        assert_eq!(plan, UpdatePlan::default());
        assert_eq!(
            plan_update(None, std::iter::empty(), ts(20)),
            UpdatePlan::default()
        );
    }

    #[test]
    fn tx_closed_versions_are_not_slices() {
        let closed = slice(0, None).close_transaction_time(ts(5)).unwrap();
        assert!(!is_open_slice(&closed));
        assert!(is_open_slice(&slice(0, None)));
        assert!(is_open_slice(&slice(0, Some(1))));
        let empty = slice(3, Some(3));
        assert!(!is_open_slice(&empty));
        assert_eq!(empty.valid_time().end(), ts(3));
        assert_ne!(empty.valid_time().end(), TIMESTAMP_MAX);
    }

    #[test]
    fn storage_update_records_carry_forward_and_tracks_slices() {
        let mut storage = HistoricalStorage::new();
        let node = NodeId::new(7).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER.intern("N").unwrap();
        let props = crate::core::property::PropertyMapBuilder::new()
            .insert("k", 1i64)
            .build();
        storage
            .add_node_version(node, vid(1), ts(10), ts(100), label, props.clone(), false)
            .unwrap();

        let plan = storage.plan_node_update(node, ts(20));
        let carry = VersionId::structural(2).unwrap();
        let applied = storage
            .apply_node_update(
                node,
                &plan,
                ts(20),
                ts(200),
                vid(3),
                label,
                PropertyMap::new(),
                None,
                Some(carry),
                None,
            )
            .unwrap();
        let ids: Vec<_> = applied.appended.iter().map(|(v, _)| *v).collect();
        assert_eq!(ids, vec![carry, vid(3)]);
        assert!(carry.is_structural());
        assert_eq!(storage.node_open_slices(node), &[carry]);
        assert_eq!(storage.get_current_node_version(node), Some(vid(3)));
        assert_eq!(storage.reconstruct_node_properties(carry).unwrap(), props);

        // Original version: tx-closed, valid interval untouched (#3504).
        let original = storage.get_node_version(vid(1)).unwrap();
        assert!(original.temporal.valid_time().is_current());
        assert!(!original.temporal.is_currently_recorded());

        // Restore round trip keeps the slice index and the head.
        storage.rebuild_version_chains();
        assert_eq!(storage.node_open_slices(node), &[carry]);
        assert_eq!(storage.get_current_node_version(node), Some(vid(3)));

        // Delete closes every slice.
        storage.close_all_node_slices(node, ts(300)).unwrap();
        assert!(storage.node_open_slices(node).is_empty());
        assert!(
            !storage
                .get_node_version(vid(3))
                .unwrap()
                .temporal
                .is_currently_recorded()
        );
    }

    #[test]
    fn update_capacity_counts_logical_versions_and_is_checked_first() {
        let mut storage = HistoricalStorage::with_config_and_retention(
            crate::core::version::AnchorConfig::default(),
            super::super::RetentionPolicy::new(2, i64::MAX),
        );
        let node = NodeId::new(1).unwrap();
        let label = crate::core::interning::GLOBAL_INTERNER.intern("N").unwrap();
        storage
            .add_node_version(
                node,
                vid(1),
                ts(10),
                ts(100),
                label,
                PropertyMap::new(),
                false,
            )
            .unwrap();

        // Second logical version fits (its carry-forward is exempt).
        let plan = storage.plan_node_update(node, ts(20));
        storage
            .apply_node_update(
                node,
                &plan,
                ts(20),
                ts(200),
                vid(3),
                label,
                PropertyMap::new(),
                None,
                Some(VersionId::structural(2).unwrap()),
                None,
            )
            .unwrap();
        assert_eq!(storage.node_logical_version_count(node), 2);

        // A third logical version exceeds the cap of 2: rejected before any
        // mutation, so the head stays open.
        let plan = storage.plan_node_update(node, ts(30));
        let err = storage
            .apply_node_update(
                node,
                &plan,
                ts(30),
                ts(300),
                vid(5),
                label,
                PropertyMap::new(),
                None,
                Some(VersionId::structural(4).unwrap()),
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("versions"), "{err}");
        assert!(
            storage
                .get_node_version(vid(3))
                .unwrap()
                .temporal
                .is_currently_recorded()
        );
        assert_eq!(storage.node_open_slices(node).len(), 1);
    }
}
