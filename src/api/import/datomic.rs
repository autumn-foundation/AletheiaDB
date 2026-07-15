//! Datomic datom-stream EDN importer (Issue #3384).
//!
//! Replays a Datomic transaction log — a flat, tx-ordered **datom stream** —
//! together with an **attribute-schema map** that classifies ref vs scalar
//! attributes (a datom's `:a` alone does not reveal its `:db/valueType`).
//!
//! ```clojure
//! ;; datoms
//! [{:e 200 :a :person/name :v "Alice" :tx 1001 :tx-instant #inst "2021-03-01T00:00:00Z" :op true}
//!  {:e 200 :a :person/title :v "Engineer" :tx 1001 :tx-instant #inst "2021-03-01T00:00:00Z" :op true}
//!  {:e 200 :a :person/title :v "Engineer" :tx 1002 :tx-instant #inst "2023-01-01T00:00:00Z" :op false}
//!  {:e 200 :a :person/title :v "CEO" :tx 1002 :tx-instant #inst "2023-01-01T00:00:00Z" :op true}]
//! ;; schema
//! {:person/name  {:db/valueType :db.type/string :db/cardinality :db.cardinality/one}
//!  :person/employer {:db/valueType :db.type/ref :aletheia/edge-label "EMPLOYER"}}
//! ```
//!
//! # Mapping
//!
//! | Source | AletheiaDB |
//! |--------|-----------|
//! | entity id `:e` | business key + `db/id` property |
//! | scalar attr (`:op true`) | **Property** |
//! | ref attr (`:db.type/ref`) | **Edge** to `:v` |
//! | first tx for entity → later tx | `create` → `update` (new version) |
//! | scalar retract (`:op false`) | key set to `Null`, reported |
//! | ref retract (`:op false`) | close the edge's valid interval |
//! | `:db/txInstant` | `valid_from` (default single-axis rule) + provenance |
//!
//! # Single-axis rule
//!
//! Datomic records only transaction time. By default `:db/txInstant` becomes
//! AletheiaDB's `valid_from`; [`DatomicOptions::valid_time_attr`] can redirect
//! the rule at a domain effective-date attribute instead.
//!
//! # Unsupported (reported, never silently dropped)
//!
//! Schema-alteration history (`:db/ident`, `:db.install/attribute`, ...),
//! transaction functions (`:db/fn`), and excision (`:db/excise`) datoms are
//! skipped and enumerated in the fidelity report.

use std::collections::HashMap;
use std::path::Path;

use super::edn::{self, Edn};
use super::history::{
    ImportFidelityReport, PreparedEntity, PreparedRefEdge, PreparedVersion, replay,
};
use super::{Importer, RowError};
use crate::core::error::{Error, Result};
use crate::core::property::{PropertyMapBuilder, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;

/// How a cardinality-many scalar attribute collapses onto the single-value
/// property model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ManyScalarPolicy {
    /// Keep the last asserted value (default); reported.
    #[default]
    LastValue,
    /// Accumulate all asserted values into an array property.
    List,
}

/// Options controlling a Datomic datom-stream import (Issue #3384).
#[derive(Debug, Clone)]
pub struct DatomicOptions {
    /// Optional attribute (full ident) whose value becomes the node label. When
    /// `None`, the label is derived from the namespace of the entity's
    /// attributes (`:person/name` → `Person`).
    pub label_attr: Option<String>,
    /// Label used when no attribute namespace is available (default `"Entity"`).
    pub default_label: String,
    /// Property key under which the `:e` entity id is stored (default `"db/id"`).
    pub id_property: String,
    /// Optional attribute (full ident) to use as `valid_from` instead of
    /// `:db/txInstant` (redirects the single-axis rule).
    pub valid_time_attr: Option<String>,
    /// How cardinality-many scalar attributes collapse (default `LastValue`).
    pub cardinality_many_scalar: ManyScalarPolicy,
    /// Abort on the first bad datom, or skip-and-report.
    pub failure_mode: super::FailureMode,
}

impl Default for DatomicOptions {
    fn default() -> Self {
        DatomicOptions {
            label_attr: None,
            default_label: "Entity".to_string(),
            id_property: "db/id".to_string(),
            valid_time_attr: None,
            cardinality_many_scalar: ManyScalarPolicy::LastValue,
            failure_mode: super::FailureMode::Abort,
        }
    }
}

impl DatomicOptions {
    /// Create default Datomic import options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Redirect the valid-time rule at a domain effective-date attribute.
    #[must_use]
    pub fn valid_time_attr(mut self, attr: impl Into<String>) -> Self {
        self.valid_time_attr = Some(attr.into());
        self
    }

    /// Set the attribute whose value becomes the node label.
    #[must_use]
    pub fn label_attr(mut self, attr: impl Into<String>) -> Self {
        self.label_attr = Some(attr.into());
        self
    }

    /// Set the cardinality-many scalar collapse policy.
    #[must_use]
    pub fn cardinality_many_scalar(mut self, policy: ManyScalarPolicy) -> Self {
        self.cardinality_many_scalar = policy;
        self
    }

    /// Set the failure mode.
    #[must_use]
    pub fn failure_mode(mut self, mode: super::FailureMode) -> Self {
        self.failure_mode = mode;
        self
    }
}

/// A schema descriptor for one attribute.
struct AttrSchema {
    is_ref: bool,
    cardinality_many: bool,
    edge_label: String,
}

/// A parsed datom.
struct Datom {
    e: i64,
    attr_full: String,
    attr_local: String,
    v: Edn,
    tx: i64,
    tx_instant: i64,
    op: bool,
}

/// Uppercase-snake a name for a default edge label.
fn screaming_snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_uppercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Capitalize the first character of a namespace for a default node label
/// (`person` → `Person`).
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Namespace portion of a full attribute ident (`person/name` → `person`).
fn attr_namespace(full: &str) -> &str {
    full.split_once('/').map(|(ns, _)| ns).unwrap_or(full)
}

/// Classify a system/schema attribute (namespace under `db`), returning the
/// unsupported-note kind, or `None` for a normal domain attribute.
fn system_attr_kind(full: &str) -> Option<&'static str> {
    let ns = attr_namespace(full);
    let is_system = ns == "db" || ns.starts_with("db.") || ns == "fressian";
    if !is_system {
        return None;
    }
    if full.contains("excise") {
        Some("excision")
    } else if full.contains("fn") {
        Some("db_fn")
    } else {
        Some("schema_history")
    }
}

fn edn_to_property(v: &Edn) -> Option<PropertyValue> {
    match v {
        Edn::Str(s) => Some(PropertyValue::string(s)),
        Edn::Int(i) => Some(PropertyValue::Int(*i)),
        Edn::Float(f) => Some(PropertyValue::Float(*f)),
        Edn::Bool(b) => Some(PropertyValue::Bool(*b)),
        Edn::Keyword(k) => Some(PropertyValue::string(k)),
        Edn::Uuid(u) => Some(PropertyValue::string(u)),
        Edn::Inst(m) => Some(PropertyValue::Int(*m)),
        Edn::Symbol(s) => Some(PropertyValue::string(s)),
        Edn::Nil => None,
        Edn::Vector(items) | Edn::List(items) | Edn::Set(items) => Some(PropertyValue::array(
            items.iter().filter_map(edn_to_property).collect(),
        )),
        Edn::Map(_) => None,
    }
}

/// Build the provenance stamped on a datom-derived version.
fn datomic_provenance(file: &str, tx: i64, tx_instant: i64) -> Result<Provenance> {
    Provenance::builder()
        .source(format!("datomic-import::{file}"))
        .confidence(1.0)
        .note(format!("datomic tx={tx} txInstant={tx_instant}"))
        .correlation_id(format!("datomic:tx:{tx}"))
        .build()
        .map_err(|e| Error::other(e.to_string()))
}

fn file_basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

impl<'a> Importer<'a> {
    /// Replay a Datomic datom stream + attribute schema (both EDN, Issue #3384).
    ///
    /// # Errors
    ///
    /// Returns an error if either file cannot be read; the EDN is malformed;
    /// an entity is already present in this session (re-import refusal); or — in
    /// the default [`FailureMode::Abort`](super::FailureMode::Abort) — the first
    /// malformed datom or unresolved ref endpoint.
    pub fn datomic_import(
        &mut self,
        datoms_path: impl AsRef<Path>,
        schema_path: impl AsRef<Path>,
        opts: &DatomicOptions,
    ) -> Result<ImportFidelityReport> {
        let datoms_path = datoms_path.as_ref();
        let schema_path = schema_path.as_ref();
        let datoms_text = std::fs::read_to_string(datoms_path)
            .map_err(|e| Error::other(format!("reading {}: {e}", datoms_path.display())))?;
        let schema_text = std::fs::read_to_string(schema_path)
            .map_err(|e| Error::other(format!("reading {}: {e}", schema_path.display())))?;
        let file = file_basename(datoms_path);
        self.datomic_import_from_str(&datoms_text, &schema_text, &file, opts)
    }

    /// Parse and replay in-memory Datomic EDN. Shared by [`datomic_import`] and
    /// the test suite.
    pub(crate) fn datomic_import_from_str(
        &mut self,
        datoms_text: &str,
        schema_text: &str,
        file: &str,
        opts: &DatomicOptions,
    ) -> Result<ImportFidelityReport> {
        // Keep the shared replay driver's failure-mode in step with the options.
        self.config.failure_mode = opts.failure_mode;
        let mut report = ImportFidelityReport::new("datomic");
        let schema = parse_schema(schema_text)?;

        let datom_maps = edn::parse_top_vector(datoms_text)
            .map_err(|e| Error::other(format!("EDN parse (datoms): {e}")))?;

        // Parse + classify datoms; system/schema datoms are reported and dropped.
        let mut datoms: Vec<Datom> = Vec::with_capacity(datom_maps.len());
        for (idx, dm) in datom_maps.iter().enumerate() {
            match parse_datom(dm, idx + 1) {
                Ok(datom) => {
                    if let Some(kind) = system_attr_kind(&datom.attr_full) {
                        report.note_unsupported(kind, &datom.attr_full);
                        continue;
                    }
                    datoms.push(datom);
                }
                Err(row_err) => match opts.failure_mode {
                    super::FailureMode::Abort => {
                        return Err(super::ImportError::Row(row_err).into());
                    }
                    super::FailureMode::SkipAndReport => report.skipped.push(row_err),
                },
            }
        }

        // Group by entity id, preserving ascending (e, tx) order for determinism.
        let mut by_entity: Vec<i64> = datoms.iter().map(|d| d.e).collect();
        by_entity.sort_unstable();
        by_entity.dedup();

        let mut prepared: Vec<PreparedEntity> = Vec::with_capacity(by_entity.len());
        for e in by_entity {
            report.entities_read += 1;
            let entity_datoms: Vec<&Datom> = {
                let mut v: Vec<&Datom> = datoms.iter().filter(|d| d.e == e).collect();
                v.sort_by_key(|d| d.tx);
                v
            };
            match build_datomic_entity(e, &entity_datoms, &schema, opts, file, &mut report) {
                Ok(entity) => prepared.push(entity),
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
}

/// Parse the attribute-schema EDN map into `ident → descriptor`.
fn parse_schema(text: &str) -> Result<HashMap<String, AttrSchema>> {
    let value =
        edn::parse_one(text).map_err(|e| Error::other(format!("EDN parse (schema): {e}")))?;
    let entries = value
        .as_map()
        .ok_or_else(|| Error::other("schema must be an EDN map".to_string()))?;
    let mut schema = HashMap::new();
    for (k, descriptor) in entries {
        let Edn::Keyword(ident) = k else {
            continue;
        };
        let local = edn::keyword_local(ident);
        let value_type_ref = descriptor
            .map_get_local("valueType")
            .and_then(Edn::as_keyword)
            .map(|kw| edn::keyword_local(kw) == "ref")
            .unwrap_or(false);
        let cardinality_many = descriptor
            .map_get_local("cardinality")
            .and_then(Edn::as_keyword)
            .map(|kw| edn::keyword_local(kw) == "many")
            .unwrap_or(false);
        let edge_label = descriptor
            .map_get_local("edge-label")
            .and_then(Edn::as_str)
            .map(|s| s.to_string())
            .unwrap_or_else(|| screaming_snake(local));
        schema.insert(
            ident.clone(),
            AttrSchema {
                is_ref: value_type_ref,
                cardinality_many,
                edge_label,
            },
        );
    }
    Ok(schema)
}

/// Parse one datom map into a [`Datom`].
fn parse_datom(dm: &Edn, row: usize) -> std::result::Result<Datom, RowError> {
    let err = |msg: String| RowError::new(row, msg);
    let e = dm
        .map_get_local("e")
        .and_then(as_int)
        .ok_or_else(|| err("datom has no integer :e".to_string()))?;
    let attr_full = match dm.map_get_local("a") {
        Some(Edn::Keyword(k)) => k.clone(),
        _ => return Err(err("datom has no keyword :a".to_string())),
    };
    let attr_local = edn::keyword_local(&attr_full).to_string();
    let v = dm
        .map_get_local("v")
        .cloned()
        .ok_or_else(|| err("datom has no :v".to_string()))?;
    let tx = dm
        .map_get_local("tx")
        .and_then(as_int)
        .ok_or_else(|| err("datom has no integer :tx".to_string()))?;
    let tx_instant = dm
        .map_get_local("tx-instant")
        .and_then(inst_micros)
        .ok_or_else(|| err("datom has no #inst :tx-instant".to_string()))?;
    let op = match dm.map_get_local("op") {
        Some(Edn::Bool(b)) => *b,
        None => true,
        Some(other) => return Err(err(format!(":op must be a boolean, got {}", other.kind()))),
    };
    Ok(Datom {
        e,
        attr_full,
        attr_local,
        v,
        tx,
        tx_instant,
        op,
    })
}

/// Fold an entity's ordered datoms into a [`PreparedEntity`].
fn build_datomic_entity(
    e: i64,
    datoms: &[&Datom],
    schema: &HashMap<String, AttrSchema>,
    opts: &DatomicOptions,
    file: &str,
    report: &mut ImportFidelityReport,
) -> std::result::Result<PreparedEntity, RowError> {
    let err = |msg: String| RowError::new(0, msg);
    let key = e.to_string();

    // Derive the node label.
    let label = derive_label(datoms, opts);

    // Distinct transactions in ascending order.
    let mut txs: Vec<i64> = datoms.iter().map(|d| d.tx).collect();
    txs.sort_unstable();
    txs.dedup();

    let mut versions: Vec<PreparedVersion> = Vec::new();
    let mut ref_edges: Vec<PreparedRefEdge> = Vec::new();
    // (attr_local, target_key) -> index into ref_edges, for retract matching.
    let mut edge_index: HashMap<(String, String), usize> = HashMap::new();
    let mut first = true;

    for tx in txs {
        report.versions_read += 1;
        let tx_datoms: Vec<&Datom> = datoms.iter().copied().filter(|d| d.tx == tx).collect();
        let tx_instant = tx_datoms.first().map(|d| d.tx_instant).unwrap_or(0);
        // Redirect valid_from to a domain attribute if configured.
        let valid_from = opts
            .valid_time_attr
            .as_deref()
            .and_then(|va| {
                tx_datoms
                    .iter()
                    .find(|d| d.attr_full == va && d.op)
                    .and_then(|d| inst_micros(&d.v).or_else(|| as_int(&d.v)))
            })
            .unwrap_or(tx_instant);

        let provenance = datomic_provenance(file, tx, tx_instant)
            .map_err(|e| err(format!("provenance: {e}")))?;

        let mut builder = PropertyMapBuilder::new();
        let mut prop_count = 0usize;
        if first {
            builder = builder
                .try_insert(&opts.id_property, PropertyValue::Int(e))
                .map_err(|e| err(e.to_string()))?;
            prop_count += 1;
        }

        // Split this tx's datoms: refs applied immediately; scalar asserts
        // grouped per attribute (to honor cardinality-many); scalar retracts
        // recorded so a retract with no same-tx re-assert becomes an explicit
        // null.
        let mut scalar_asserts: Vec<(String, String, Vec<PropertyValue>, bool)> = Vec::new();
        let mut asserted_attrs: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut retracted: Vec<(String, String)> = Vec::new();
        for &d in &tx_datoms {
            if opts.valid_time_attr.as_deref() == Some(d.attr_full.as_str()) {
                continue;
            }
            let sch = schema.get(&d.attr_full);
            let is_ref = sch.map(|s| s.is_ref).unwrap_or(false);
            let is_many = sch.map(|s| s.cardinality_many).unwrap_or(false);
            if sch.is_none() {
                report.note_unsupported("unknown_attribute", &d.attr_full);
            }
            if is_ref {
                handle_ref_datom(d, schema, &provenance, &mut ref_edges, &mut edge_index);
                continue;
            }
            if d.op {
                let Some(value) = edn_to_property(&d.v) else {
                    continue;
                };
                if matches!(value, PropertyValue::Null) {
                    continue;
                }
                asserted_attrs.insert(d.attr_full.clone());
                if let Some(group) = scalar_asserts
                    .iter_mut()
                    .find(|(af, _, _, _)| af == &d.attr_full)
                {
                    group.2.push(value);
                } else {
                    scalar_asserts.push((
                        d.attr_full.clone(),
                        d.attr_local.clone(),
                        vec![value],
                        is_many,
                    ));
                }
            } else {
                retracted.push((d.attr_full.clone(), d.attr_local.clone()));
            }
        }

        for (_attr_full, attr_local, values, is_many) in scalar_asserts {
            let value = if is_many {
                report.note_coerced("cardinality_many_scalar", &attr_local);
                match opts.cardinality_many_scalar {
                    ManyScalarPolicy::List => PropertyValue::array(values),
                    ManyScalarPolicy::LastValue => match values.into_iter().next_back() {
                        Some(v) => v,
                        None => continue,
                    },
                }
            } else {
                match values.into_iter().next_back() {
                    Some(v) => v,
                    None => continue,
                }
            };
            builder = builder
                .try_insert(&attr_local, value)
                .map_err(|e| err(e.to_string()))?;
            prop_count += 1;
            report.note_mapping("attribute", &attr_local, "property", &attr_local);
        }

        // Scalar retract with no same-tx re-assert → explicit null (reported).
        for (attr_full, attr_local) in retracted {
            if asserted_attrs.contains(&attr_full) {
                continue;
            }
            builder = builder
                .try_insert(&attr_local, PropertyValue::Null)
                .map_err(|e| err(e.to_string()))?;
            prop_count += 1;
            report.note_unsupported("attr_retract_as_null", &attr_local);
        }

        versions.push(PreparedVersion {
            label: label.clone(),
            properties: builder.build(),
            valid_from: Timestamp::from(valid_from),
            provenance: Some(provenance),
            is_create: first,
            property_count: prop_count,
        });
        first = false;
    }

    report.note_mapping("entity", &key, "node", &label);

    Ok(PreparedEntity {
        key,
        versions,
        ref_edges,
        retraction: None,
    })
}

/// Handle a ref-typed datom: assert creates/dedupes an edge, retract closes it.
fn handle_ref_datom(
    d: &Datom,
    schema: &HashMap<String, AttrSchema>,
    provenance: &Provenance,
    ref_edges: &mut Vec<PreparedRefEdge>,
    edge_index: &mut HashMap<(String, String), usize>,
) {
    let Some(target_key) = ref_target_key(&d.v) else {
        return;
    };
    let label = schema
        .get(&d.attr_full)
        .map(|s| s.edge_label.clone())
        .unwrap_or_else(|| screaming_snake(&d.attr_local));
    let dedup = (d.attr_local.clone(), target_key.clone());

    if d.op {
        if let std::collections::hash_map::Entry::Vacant(slot) = edge_index.entry(dedup) {
            let idx = ref_edges.len();
            ref_edges.push(PreparedRefEdge {
                target_key,
                label,
                source_field: d.attr_local.clone(),
                valid_from: Timestamp::from(d.tx_instant),
                provenance: Some(provenance.clone()),
                retract_at: None,
            });
            slot.insert(idx);
        }
    } else if let Some(&idx) = edge_index.get(&dedup)
        && let Some(edge) = ref_edges.get_mut(idx)
    {
        edge.retract_at = Some(Timestamp::from(d.tx_instant));
    }
}

/// The business key an edge's ref value points at.
fn ref_target_key(v: &Edn) -> Option<String> {
    match v {
        Edn::Int(i) => Some(i.to_string()),
        Edn::Str(s) => Some(s.clone()),
        Edn::Keyword(k) => Some(edn::keyword_local(k).to_string()),
        Edn::Uuid(u) => Some(u.clone()),
        _ => None,
    }
}

/// Derive a node label from the configured attribute or the attribute namespace.
fn derive_label(datoms: &[&Datom], opts: &DatomicOptions) -> String {
    if let Some(label_attr) = &opts.label_attr
        && let Some(d) = datoms.iter().find(|d| &d.attr_full == label_attr && d.op)
    {
        if let Some(s) = d.v.as_str() {
            return s.to_string();
        }
        if let Edn::Keyword(k) = &d.v {
            return edn::keyword_local(k).to_string();
        }
    }
    // Default: capitalize the namespace of the first domain attribute.
    datoms
        .iter()
        .map(|d| attr_namespace(&d.attr_full))
        .find(|ns| !ns.is_empty())
        .map(capitalize)
        .unwrap_or_else(|| opts.default_label.clone())
}

fn inst_micros(v: &Edn) -> Option<i64> {
    match v {
        Edn::Inst(m) => Some(*m),
        _ => None,
    }
}

fn as_int(v: &Edn) -> Option<i64> {
    match v {
        Edn::Int(i) => Some(*i),
        _ => None,
    }
}
