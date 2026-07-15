//! XTDB entity-history EDN importer (Issue #3384).
//!
//! Replays an XTDB `xt/entity-history` export taken with `:with-docs? true`,
//! preserving valid-time, transaction-time provenance, supersession, and
//! deletes. The export is a single top-level EDN vector of **per-entity history
//! maps**:
//!
//! ```clojure
//! [{:xt/id :alice
//!   :history
//!   [{:xtdb.api/valid-time #inst "2021-03-01T00:00:00Z"
//!     :xtdb.api/tx-time    #inst "2021-03-01T09:00:00Z"
//!     :xtdb.api/tx-id 42
//!     :xtdb.api/doc {:xt/id :alice :type "Person" :name "Alice" :title "Engineer"}}
//!    ...]}]
//! ```
//!
//! # Mapping
//!
//! | Source | AletheiaDB |
//! |--------|-----------|
//! | history map `:xt/id` | business key + `xt/id` property |
//! | `:xtdb.api/doc` (map) | **Node**; label from [`XtdbOptions::label_field`] (default `type`) |
//! | doc field detected as a ref | **Edge** (see ref detection) |
//! | `:xtdb.api/valid-time` | `valid_from` |
//! | `:xtdb.api/valid-to` | interval close (retraction) |
//! | `:xtdb.api/tx-time` + `:xtdb.api/tx-id` | provenance |
//! | `:xtdb.api/doc` = nil / absent | delete → `retract_node_detach` |
//!
//! Keys are matched by **local name** ignoring namespace, so both the canonical
//! `:xtdb.api/valid-time` and a short `:xt/valid-time` are accepted.

use std::collections::HashSet;
use std::path::Path;

use super::edn::{self, Edn};
use super::history::{
    ImportFidelityReport, PreparedEntity, PreparedRefEdge, PreparedRetraction, PreparedVersion,
    replay,
};
use super::{Importer, RowError};
use crate::core::error::{Error, Result};
use crate::core::property::{PropertyMapBuilder, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;

/// Options controlling an XTDB entity-history import (Issue #3384).
#[derive(Debug, Clone)]
pub struct XtdbOptions {
    /// Doc field (local name) whose value becomes the node label
    /// (default `"type"`).
    pub label_field: String,
    /// Label used when the `label_field` is absent (default `"Entity"`).
    pub default_label: String,
    /// Property key under which the `:xt/id` business key is stored
    /// (default `"xt/id"`).
    pub id_property: String,
    /// When `true` (default), a scalar doc field whose value resolves to another
    /// entity's `:xt/id` is promoted to an edge automatically.
    pub auto_detect_refs: bool,
    /// Explicit `field-name → edge-label` overrides. A field named here is
    /// always treated as a ref, and the mapped value is the edge label.
    pub ref_fields: std::collections::HashMap<String, String>,
    /// Abort on the first bad entity, or skip-and-report.
    pub failure_mode: super::FailureMode,
}

impl Default for XtdbOptions {
    fn default() -> Self {
        XtdbOptions {
            label_field: "type".to_string(),
            default_label: "Entity".to_string(),
            id_property: "xt/id".to_string(),
            auto_detect_refs: true,
            ref_fields: std::collections::HashMap::new(),
            failure_mode: super::FailureMode::Abort,
        }
    }
}

impl XtdbOptions {
    /// Create default XTDB import options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the doc field used as the node label.
    #[must_use]
    pub fn label_field(mut self, field: impl Into<String>) -> Self {
        self.label_field = field.into();
        self
    }

    /// Set the fallback label used when the label field is absent.
    #[must_use]
    pub fn default_label(mut self, label: impl Into<String>) -> Self {
        self.default_label = label.into();
        self
    }

    /// Declare an explicit ref field with its edge label.
    #[must_use]
    pub fn ref_field(mut self, field: impl Into<String>, label: impl Into<String>) -> Self {
        self.ref_fields.insert(field.into(), label.into());
        self
    }

    /// Enable or disable automatic ref detection.
    #[must_use]
    pub fn auto_detect_refs(mut self, on: bool) -> Self {
        self.auto_detect_refs = on;
        self
    }

    /// Set the failure mode.
    #[must_use]
    pub fn failure_mode(mut self, mode: super::FailureMode) -> Self {
        self.failure_mode = mode;
        self
    }
}

/// Canonicalize an EDN scalar into a business-key string. Keywords use their
/// local (namespace-stripped) name so a `:xt/id :alice` entity and an
/// `:employer :alice` ref both resolve to `"alice"`.
fn business_key(v: &Edn) -> Option<String> {
    match v {
        Edn::Keyword(k) => Some(edn::keyword_local(k).to_string()),
        Edn::Str(s) => Some(s.clone()),
        Edn::Int(i) => Some(i.to_string()),
        Edn::Uuid(u) => Some(u.clone()),
        _ => None,
    }
}

/// Convert an EDN scalar / scalar-array into a [`PropertyValue`]. Nested maps
/// and `nil` yield `None` (absent property).
fn edn_to_property(v: &Edn) -> Option<PropertyValue> {
    match v {
        Edn::Str(s) => Some(PropertyValue::string(s)),
        Edn::Int(i) => Some(PropertyValue::Int(*i)),
        Edn::Float(f) => Some(PropertyValue::Float(*f)),
        Edn::Bool(b) => Some(PropertyValue::Bool(*b)),
        Edn::Keyword(k) => Some(PropertyValue::string(k)),
        Edn::Uuid(u) => Some(PropertyValue::string(u)),
        Edn::Inst(m) => Some(PropertyValue::Int(*m)),
        Edn::Nil => None,
        Edn::Symbol(s) => Some(PropertyValue::string(s)),
        Edn::Vector(items) | Edn::List(items) | Edn::Set(items) => {
            let vals: Vec<PropertyValue> = items.iter().filter_map(edn_to_property).collect();
            Some(PropertyValue::array(vals))
        }
        Edn::Map(_) => None,
    }
}

/// Uppercase-snake a field name for a default edge label (`employer` → `EMPLOYER`).
fn screaming_snake(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for c in field.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Build the provenance bundle stamped on every version of an entity.
fn xtdb_provenance(
    file: &str,
    tx_id: Option<i64>,
    tx_time_micros: Option<i64>,
) -> Result<Provenance> {
    let mut builder = Provenance::builder()
        .source(format!("xtdb-import::{file}"))
        .confidence(1.0);
    let tx_id_str = tx_id
        .map(|i| i.to_string())
        .unwrap_or_else(|| "?".to_string());
    let tx_time_str = tx_time_micros
        .map(|m| m.to_string())
        .unwrap_or_else(|| "?".to_string());
    builder = builder.note(format!("xtdb tx-id={tx_id_str} tx-time={tx_time_str}"));
    if let Some(id) = tx_id {
        builder = builder.correlation_id(format!("xt:tx:{id}"));
    }
    builder.build().map_err(|e| Error::other(e.to_string()))
}

/// The basename used for the provenance `source`.
fn file_basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

impl<'a> Importer<'a> {
    /// Replay a full XTDB entity-history EDN export, preserving valid-time and
    /// provenance (Issue #3384).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read; the EDN is malformed
    /// (a typed [`edn::EdnError`](super::edn::EdnError) surfaced with position);
    /// an entity is already present in this session (re-import refusal); or — in
    /// the default [`FailureMode::Abort`](super::FailureMode::Abort) — the first
    /// malformed entity or unresolved ref endpoint.
    pub fn xtdb_import(
        &mut self,
        path: impl AsRef<Path>,
        opts: &XtdbOptions,
    ) -> Result<ImportFidelityReport> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::other(format!("reading {}: {e}", path.display())))?;
        let file = file_basename(path);
        self.xtdb_import_from_str(&text, &file, opts)
    }

    /// Parse and replay an XTDB entity-history export already in memory. Shared
    /// by [`xtdb_import`](Self::xtdb_import) and the test suite.
    pub(crate) fn xtdb_import_from_str(
        &mut self,
        text: &str,
        file: &str,
        opts: &XtdbOptions,
    ) -> Result<ImportFidelityReport> {
        // Keep the shared replay driver's failure-mode in step with the options.
        self.config.failure_mode = opts.failure_mode;
        let mut report = ImportFidelityReport::new("xtdb");
        let entity_maps =
            edn::parse_top_vector(text).map_err(|e| Error::other(format!("EDN parse: {e}")))?;

        // First pass: collect all business keys so ref detection can resolve
        // intra-export references.
        let mut known_keys: HashSet<String> = HashSet::new();
        for m in &entity_maps {
            if let Some(id) = m.map_get_local("id").and_then(business_key) {
                known_keys.insert(id);
            }
        }

        let mut prepared: Vec<PreparedEntity> = Vec::with_capacity(entity_maps.len());
        for (idx, entity_map) in entity_maps.iter().enumerate() {
            report.entities_read += 1;
            match self.prepare_xtdb_entity(
                entity_map,
                opts,
                file,
                &known_keys,
                &mut report,
                idx + 1,
            ) {
                Ok(Some(entity)) => prepared.push(entity),
                Ok(None) => {}
                Err(row_err) => match opts.failure_mode {
                    super::FailureMode::Abort => {
                        return Err(super::ImportError::Row(row_err).into());
                    }
                    super::FailureMode::SkipAndReport => report.skipped.push(row_err),
                },
            }
        }

        replay(self, &prepared, &mut report)?;
        report.finalize();
        Ok(report)
    }

    /// Prepare one per-entity history map into a [`PreparedEntity`], or `None`
    /// if it carried no live version (pure tombstone with nothing to create).
    #[allow(clippy::too_many_arguments)]
    fn prepare_xtdb_entity(
        &self,
        entity_map: &Edn,
        opts: &XtdbOptions,
        file: &str,
        known_keys: &HashSet<String>,
        report: &mut ImportFidelityReport,
        row: usize,
    ) -> std::result::Result<Option<PreparedEntity>, RowError> {
        let err = |msg: String| RowError::new(row, msg);

        let id_value = entity_map
            .map_get_local("id")
            .ok_or_else(|| err("entity map has no :xt/id".to_string()))?;
        let key = business_key(id_value)
            .ok_or_else(|| err(format!("unsupported :xt/id value ({})", id_value.kind())))?;

        let history = entity_map
            .map_get_local("history")
            .and_then(Edn::as_seq)
            .ok_or_else(|| err("entity map has no :history vector".to_string()))?;

        // Defensive sort by (valid-time, tx-id) ascending — the producer claims
        // ascending, but a mis-ordered replay would fail the downstream
        // valid_from-not-before-creation check, so we normalize here (AC5).
        let mut segments: Vec<&Edn> = history.iter().collect();
        segments.sort_by_key(|seg| {
            let vt = seg.map_get_local("valid-time").and_then(inst_micros);
            let tx = seg.map_get_local("tx-id").and_then(as_int);
            (vt.unwrap_or(i64::MIN), tx.unwrap_or(i64::MIN))
        });

        let mut versions: Vec<PreparedVersion> = Vec::new();
        let mut ref_edges: Vec<PreparedRefEdge> = Vec::new();
        let mut seen_refs: HashSet<(String, String)> = HashSet::new();
        let mut retraction: Option<PreparedRetraction> = None;
        let mut last_live_valid_to: Option<i64> = None;

        for seg in segments {
            report.versions_read += 1;
            let valid_from = seg
                .map_get_local("valid-time")
                .and_then(inst_micros)
                .ok_or_else(|| err("history segment has no :xtdb.api/valid-time".to_string()))?;
            let tx_id = seg.map_get_local("tx-id").and_then(as_int);
            let tx_time = seg.map_get_local("tx-time").and_then(inst_micros);
            let provenance = xtdb_provenance(file, tx_id, tx_time)
                .map_err(|e| err(format!("provenance: {e}")))?;

            let doc = seg.map_get_local("doc");
            let is_delete = match doc {
                None => true,
                Some(Edn::Nil) => true,
                Some(_) => false,
            };

            if is_delete {
                // Tombstone: close the entity's valid interval at this segment's
                // valid-time (detach co-retracts any edges).
                retraction = Some(PreparedRetraction {
                    valid_to: Timestamp::from(valid_from),
                    provenance: Some(provenance),
                    detach: true,
                });
                last_live_valid_to = None;
                continue;
            }

            let doc = doc.ok_or_else(|| err("delete/live classification error".to_string()))?;
            if doc.as_map().is_none() {
                return Err(err(format!(
                    "history segment :doc must be a map, got {}",
                    doc.kind()
                )));
            }

            let (label, properties, prop_count) =
                build_xtdb_node(doc, &key, opts, known_keys, report, &err)?;

            // Collect ref edges (deduped across versions; earliest valid_from).
            collect_xtdb_refs(
                doc,
                opts,
                known_keys,
                valid_from,
                &provenance,
                &mut ref_edges,
                &mut seen_refs,
                report,
            );

            let is_create = versions.is_empty();
            if is_create {
                report.note_mapping("document", &key, "node", &label);
            }
            versions.push(PreparedVersion {
                label,
                properties,
                valid_from: Timestamp::from(valid_from),
                provenance: Some(provenance),
                is_create,
                property_count: prop_count,
            });

            last_live_valid_to = seg.map_get_local("valid-to").and_then(inst_micros);
        }

        // A trailing live segment with an explicit valid-to closes the interval
        // (only when no later delete already set a retraction).
        if retraction.is_none()
            && let Some(valid_to) = last_live_valid_to
        {
            let provenance =
                xtdb_provenance(file, None, None).map_err(|e| err(format!("provenance: {e}")))?;
            retraction = Some(PreparedRetraction {
                valid_to: Timestamp::from(valid_to),
                provenance: Some(provenance),
                detach: true,
            });
        }

        if versions.is_empty() {
            // Pure tombstone with no live version to create — nothing to do.
            return Ok(None);
        }

        Ok(Some(PreparedEntity {
            key,
            versions,
            ref_edges,
            retraction,
        }))
    }
}

/// Resolve the ref targets of a doc field: the (target_key, resolves) pairs the
/// field promotes to edges. Empty when the field is a plain property. This is
/// the single source of truth shared by property-building (skip a ref field) and
/// edge-collection (emit the edges).
fn field_ref_targets(
    local: &str,
    v: &Edn,
    opts: &XtdbOptions,
    known_keys: &HashSet<String>,
) -> Vec<(String, bool)> {
    let explicit = opts.ref_fields.contains_key(local);
    let candidates: Vec<&Edn> = match v {
        Edn::Vector(items) | Edn::List(items) | Edn::Set(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut targets = Vec::new();
    for cand in candidates {
        let Some(target_key) = business_key(cand) else {
            continue;
        };
        let resolves = known_keys.contains(&target_key);
        if explicit || (opts.auto_detect_refs && resolves) {
            targets.push((target_key, resolves));
        }
    }
    targets
}

/// Whether a doc field is treated as a ref (and thus excluded from properties).
fn is_ref_field(local: &str, v: &Edn, opts: &XtdbOptions, known_keys: &HashSet<String>) -> bool {
    !field_ref_targets(local, v, opts, known_keys).is_empty()
}

/// Build a node label + property map from a doc, excluding the id / label /
/// content-hash / ref fields.
fn build_xtdb_node(
    doc: &Edn,
    key: &str,
    opts: &XtdbOptions,
    known_keys: &HashSet<String>,
    report: &mut ImportFidelityReport,
    err: &impl Fn(String) -> RowError,
) -> std::result::Result<(String, crate::core::property::PropertyMap, usize), RowError> {
    let label = match doc.map_get_local(&opts.label_field) {
        Some(Edn::Str(s)) => s.to_string(),
        Some(Edn::Keyword(k)) => edn::keyword_local(k).to_string(),
        _ => {
            report.note_coerced("default_label", &opts.default_label);
            opts.default_label.clone()
        }
    };

    let mut builder = PropertyMapBuilder::new();
    let mut prop_count = 0usize;
    // Always store the business key as the id property.
    builder = builder
        .try_insert(&opts.id_property, PropertyValue::string(key))
        .map_err(|e| err(e.to_string()))?;
    prop_count += 1;

    let entries = doc.as_map().unwrap_or(&[]);
    for (k, v) in entries {
        let Edn::Keyword(kw) = k else {
            continue;
        };
        let local = edn::keyword_local(kw);
        // Skip structural / already-handled fields.
        if local == "id" || local == opts.label_field || local == "content-hash" {
            continue;
        }
        // Skip fields that are refs (they become edges).
        if is_ref_field(local, v, opts, known_keys) {
            continue;
        }
        if let Some(value) = edn_to_property(v) {
            if matches!(value, PropertyValue::Null) {
                continue;
            }
            builder = builder
                .try_insert(local, value)
                .map_err(|e| err(e.to_string()))?;
            prop_count += 1;
            report.note_mapping("property", local, "property", local);
        }
    }

    Ok((label, builder.build(), prop_count))
}

/// Collect ref edges from a doc's ref fields, deduped by (field, target).
#[allow(clippy::too_many_arguments)]
fn collect_xtdb_refs(
    doc: &Edn,
    opts: &XtdbOptions,
    known_keys: &HashSet<String>,
    valid_from: i64,
    provenance: &Provenance,
    ref_edges: &mut Vec<PreparedRefEdge>,
    seen_refs: &mut HashSet<(String, String)>,
    report: &mut ImportFidelityReport,
) {
    let entries = doc.as_map().unwrap_or(&[]);
    for (k, v) in entries {
        let Edn::Keyword(kw) = k else {
            continue;
        };
        let local = edn::keyword_local(kw);
        if local == "id" || local == opts.label_field || local == "content-hash" {
            continue;
        }
        let edge_label = opts
            .ref_fields
            .get(local)
            .cloned()
            .unwrap_or_else(|| screaming_snake(local));

        for (target_key, resolves) in field_ref_targets(local, v, opts, known_keys) {
            if !resolves {
                report.note_coerced("unresolved_ref", &format!("{local} -> {target_key}"));
            }
            let dedup_key = (local.to_string(), target_key.clone());
            if !seen_refs.insert(dedup_key) {
                continue;
            }
            report.note_coerced("ref_to_edge", local);
            ref_edges.push(PreparedRefEdge {
                target_key,
                label: edge_label.clone(),
                source_field: local.to_string(),
                valid_from: Timestamp::from(valid_from),
                provenance: Some(provenance.clone()),
                retract_at: None,
            });
        }
    }
}

/// Extract epoch micros from an `#inst` value.
fn inst_micros(v: &Edn) -> Option<i64> {
    match v {
        Edn::Inst(m) => Some(*m),
        _ => None,
    }
}

/// Extract an i64 from an EDN int.
fn as_int(v: &Edn) -> Option<i64> {
    match v {
        Edn::Int(i) => Some(*i),
        _ => None,
    }
}
