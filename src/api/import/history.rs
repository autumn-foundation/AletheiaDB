//! Shared history-replay machinery for the XTDB and Datomic importers
//! (Issue #3384).
//!
//! Unlike the CSV/Neo4j importers, which commit **one create per row**, an
//! XTDB entity-history sequence and a Datomic datom stream describe **multiple
//! versions per entity** (create → N updates → optional retract). This module
//! provides the dedicated per-entity replay driver those importers need, plus
//! the shared [`ImportFidelityReport`] (a sibling of `Neo4jFidelityReport`,
//! specialized for a history-preserving migration).
//!
//! The driver still reuses the existing spine: [`Importer::key_to_id`], the
//! `db.write(|tx| …)` batching path, [`WriteRequestOptions`], the
//! `create_*_with_options` / `retract_*_with_provenance` write APIs, and the
//! `RowError` / `UnresolvedEndpoint` error model.

use super::{Endpoint, Importer, RowError, UnresolvedEndpoint};
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::provenance::Provenance;
use crate::core::temporal::Timestamp;

/// One `(source → AletheiaDB)` mapping-table row in the fidelity report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MappingEntry {
    /// The kind of source construct (`document`, `attribute`, `ref`, `datom`).
    pub source_kind: String,
    /// The source name (doc field / attribute ident / entity label).
    pub source_name: String,
    /// The AletheiaDB construct it became (`node`, `edge`, `property`).
    pub aletheia_kind: String,
    /// The AletheiaDB label / property key chosen.
    pub aletheia_label: String,
    /// Number of source items mapped this way.
    pub count: usize,
}

/// A source construct that was **not** imported and is reported (never silently
/// dropped): a Datomic schema-alteration datom, a `:db/fn` install, an excision,
/// or an attribute retraction recorded as an explicit null.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnsupportedNote2 {
    /// A stable machine token (`schema_history`, `db_fn`, `excision`,
    /// `attr_retract_as_null`, `unknown_attribute`).
    pub kind: String,
    /// Human-readable detail (e.g. the offending attribute ident).
    pub detail: String,
    /// Number of occurrences.
    pub count: usize,
}

/// A value that was accepted but transformed on the way in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CoercionNote2 {
    /// A stable machine token (`inst_to_micros`, `ref_to_edge`,
    /// `default_label`, `unresolved_ref`, `cardinality_many_scalar`).
    pub kind: String,
    /// Human-readable detail.
    pub detail: String,
    /// Number of occurrences.
    pub count: usize,
}

/// Machine-readable fidelity report for a history-preserving import
/// (Issue #3384), shared by the XTDB and Datomic importers.
///
/// Mirrors `Neo4jFidelityReport`: counts, a mapping table, skipped/unresolved
/// items with reasons, and a `zero_loss` boolean that is `true` iff nothing was
/// skipped, left unresolved, or reported unsupported.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportFidelityReport {
    /// `"xtdb"` or `"datomic"`.
    pub source: &'static str,
    /// Number of source entities read (history maps / distinct `:e`).
    pub entities_read: usize,
    /// Total source versions read (history segments / datom transactions).
    pub versions_read: usize,
    /// Distinct nodes created.
    pub nodes_created: usize,
    /// Node versions written (creates + updates).
    pub node_versions_written: usize,
    /// Edges created.
    pub edges_created: usize,
    /// Node + edge valid-interval closes (retractions).
    pub retractions: usize,
    /// Property values written.
    pub properties_imported: usize,
    /// Source → AletheiaDB mapping table with counts.
    pub mapping: Vec<MappingEntry>,
    /// Malformed source items skipped (populated in skip-and-report mode).
    pub skipped: Vec<RowError>,
    /// Ref endpoints that did not resolve (populated in skip-and-report mode).
    pub unresolved_endpoints: Vec<UnresolvedEndpoint>,
    /// Source constructs reported as unsupported (never silently dropped).
    pub unsupported: Vec<UnsupportedNote2>,
    /// Values accepted but transformed.
    pub coerced: Vec<CoercionNote2>,
    /// `true` iff `skipped`, `unresolved_endpoints`, and `unsupported` are all
    /// empty — a lossless import.
    pub zero_loss: bool,
}

impl ImportFidelityReport {
    /// Create an empty report tagged with its `source`.
    pub(crate) fn new(source: &'static str) -> Self {
        ImportFidelityReport {
            source,
            ..Default::default()
        }
    }

    /// Recompute [`zero_loss`](Self::zero_loss) from the accumulated notes.
    pub(crate) fn finalize(&mut self) {
        self.zero_loss = self.skipped.is_empty()
            && self.unresolved_endpoints.is_empty()
            && self.unsupported.is_empty();
    }

    /// Fold one occurrence into the mapping table, deduping by
    /// `(source_kind, source_name, aletheia_kind, aletheia_label)`.
    pub(crate) fn note_mapping(
        &mut self,
        source_kind: &str,
        source_name: &str,
        aletheia_kind: &str,
        aletheia_label: &str,
    ) {
        if let Some(entry) = self.mapping.iter_mut().find(|e| {
            e.source_kind == source_kind
                && e.source_name == source_name
                && e.aletheia_kind == aletheia_kind
                && e.aletheia_label == aletheia_label
        }) {
            entry.count += 1;
        } else {
            self.mapping.push(MappingEntry {
                source_kind: source_kind.to_string(),
                source_name: source_name.to_string(),
                aletheia_kind: aletheia_kind.to_string(),
                aletheia_label: aletheia_label.to_string(),
                count: 1,
            });
        }
    }

    /// Fold one occurrence into the unsupported table, deduping by `(kind, detail)`.
    pub(crate) fn note_unsupported(&mut self, kind: &str, detail: &str) {
        if let Some(entry) = self
            .unsupported
            .iter_mut()
            .find(|e| e.kind == kind && e.detail == detail)
        {
            entry.count += 1;
        } else {
            self.unsupported.push(UnsupportedNote2 {
                kind: kind.to_string(),
                detail: detail.to_string(),
                count: 1,
            });
        }
    }

    /// Fold one occurrence into the coercion table, deduping by `(kind, detail)`.
    pub(crate) fn note_coerced(&mut self, kind: &str, detail: &str) {
        if let Some(entry) = self
            .coerced
            .iter_mut()
            .find(|e| e.kind == kind && e.detail == detail)
        {
            entry.count += 1;
        } else {
            self.coerced.push(CoercionNote2 {
                kind: kind.to_string(),
                detail: detail.to_string(),
                count: 1,
            });
        }
    }
}

/// One node version to write during replay: a create (first) or an update.
pub(crate) struct PreparedVersion {
    /// The node label for this version.
    pub label: String,
    /// The properties written at this version.
    pub properties: PropertyMap,
    /// When this version became valid in reality.
    pub valid_from: Timestamp,
    /// Optional write-time provenance for this version.
    pub provenance: Option<Provenance>,
    /// `true` for the entity's first version (a create); `false` for updates.
    pub is_create: bool,
    /// Number of property values in this version (for the fidelity count).
    pub property_count: usize,
}

/// A node-level valid-interval close (a delete / whole-entity retraction).
pub(crate) struct PreparedRetraction {
    /// The valid-time at which the entity stops being valid.
    pub valid_to: Timestamp,
    /// Optional write-time provenance for the retraction version.
    pub provenance: Option<Provenance>,
    /// Whether to co-retract connected edges (`retract_node_detach`).
    pub detach: bool,
}

/// A ref field / ref attribute that becomes an edge sourced at this entity.
pub(crate) struct PreparedRefEdge {
    /// The business key of the edge target.
    pub target_key: String,
    /// The edge label.
    pub label: String,
    /// The field/attribute name the edge came from (for the mapping table).
    pub source_field: String,
    /// When the edge became valid.
    pub valid_from: Timestamp,
    /// Optional write-time provenance.
    pub provenance: Option<Provenance>,
    /// If set, close the edge's valid interval at this time after creation.
    pub retract_at: Option<Timestamp>,
}

/// A fully-prepared source entity ready for replay.
pub(crate) struct PreparedEntity {
    /// The entity's business key (`:xt/id` / `:e`).
    pub key: String,
    /// The exact value stored under the id property — a `String` for XTDB, an
    /// `Int` for Datomic. The durable re-import guard probes with this so the
    /// property-value type matches how the importer stamped it.
    pub id_value: PropertyValue,
    /// Node versions, ascending by valid-time (create first, then updates).
    pub versions: Vec<PreparedVersion>,
    /// Ref edges sourced at this entity.
    pub ref_edges: Vec<PreparedRefEdge>,
    /// Optional whole-entity retraction (delete / interval close).
    pub retraction: Option<PreparedRetraction>,
}

/// Replay prepared entities into the database in a two-phase pass: **all nodes
/// first** (populating `key_to_id`), then ref edges, then retractions (so a
/// detach retraction can co-retract the edges just created).
///
/// Refuses the whole run with an `AlreadyImported` error if any entity key is
/// already present in this importer session (import into a fresh database — the
/// documented idempotency contract, AC5).
pub(crate) fn replay(
    importer: &mut Importer<'_>,
    entities: &[PreparedEntity],
    report: &mut ImportFidelityReport,
    id_property: &str,
) -> Result<()> {
    // Guard (AC5 — no silent duplication). Checked up front, before any write,
    // so a re-import is refused atomically rather than partially duplicating.
    //
    // Two layers:
    //   1. In-memory session guard — a second call on the same `Importer`.
    //   2. **Durable** guard — probe the target database's current state for a
    //      node already carrying this importer's business-key property, so a
    //      fresh-process re-run against a persistent target is refused too (not
    //      just a same-session repeat). The probe is a current-state
    //      label+property lookup keyed on the exact `id_property` value the
    //      importer stamps.
    for entity in entities {
        if importer.key_to_id.contains_key(&entity.key) {
            return Err(Error::other(format!(
                "AlreadyImported: business key '{}' is already present in this session; \
                 history imports must run against a fresh database",
                entity.key
            )));
        }
        if let Some(v0) = entity.versions.first() {
            let existing =
                importer
                    .db
                    .find_nodes_by_property(&v0.label, id_property, &entity.id_value);
            if !existing.is_empty() {
                return Err(Error::other(format!(
                    "AlreadyImported: the target database already contains a {} node with \
                     {id_property}='{}'; history imports must run against a fresh database \
                     (no silent duplication)",
                    v0.label, entity.key
                )));
            }
        }
    }

    // Phase 1: node creates + updates. Each version commits in its **own**
    // transaction so it lands as a distinct bi-temporal version — replaying all
    // versions of an entity in one transaction would collapse the intermediate
    // valid-time segments into a single final version (supersession needs
    // separate commits, exactly like a normal sequence of `update_node` calls).
    for entity in entities {
        if entity.versions.is_empty() {
            continue;
        }
        let mut node_id: Option<NodeId> = None;
        for version in &entity.versions {
            let label = version.label.clone();
            let properties = version.properties.clone();
            let valid_from = version.valid_from;
            let provenance = version.provenance.clone();

            // The first version of an entity is a create; the rest are updates.
            // `is_create` is authoritative (the importers mark exactly the head
            // version), with `node_id.is_none()` as a defensive fallback.
            if version.is_create || node_id.is_none() {
                let nid = importer.db.write(move |tx| {
                    use crate::api::transaction::{WriteOps, WriteRequestOptions};
                    let mut options = WriteRequestOptions::new().with_valid_from(valid_from);
                    if let Some(provenance) = provenance {
                        options = options.with_provenance(provenance);
                    }
                    tx.create_node_with_options(&label, properties, options)
                })?;
                node_id = Some(nid);
            } else if let Some(nid) = node_id {
                // Subsequent version → update (supersession) in its own commit.
                importer.db.write(move |tx| {
                    use crate::api::transaction::{WriteOps, WriteRequestOptions};
                    let mut options = WriteRequestOptions::new().with_valid_from(valid_from);
                    if let Some(provenance) = provenance {
                        options = options.with_provenance(provenance);
                    }
                    tx.update_node_with_options(nid, properties, options)?;
                    Ok::<(), Error>(())
                })?;
            }
        }

        if let Some(nid) = node_id {
            importer.key_to_id.insert(entity.key.clone(), nid);
            report.nodes_created += 1;
            report.node_versions_written += entity.versions.len();
            report.properties_imported += entity
                .versions
                .iter()
                .map(|v| v.property_count)
                .sum::<usize>();
        }
    }

    // Phase 2: ref edges (endpoints resolve against the fully-populated map).
    let mut edge_seq = 0usize;
    for entity in entities {
        let Some(source) = importer.key_to_id.get(&entity.key).copied() else {
            continue;
        };
        for edge in &entity.ref_edges {
            edge_seq += 1;
            let Some(target) = importer.key_to_id.get(&edge.target_key).copied() else {
                let unresolved = UnresolvedEndpoint {
                    row: edge_seq,
                    key: edge.target_key.clone(),
                    side: Endpoint::Target,
                    file: None,
                };
                match importer.config.failure_mode {
                    super::FailureMode::Abort => {
                        return Err(super::ImportError::UnresolvedEndpoint(unresolved).into());
                    }
                    super::FailureMode::SkipAndReport => {
                        report.unresolved_endpoints.push(unresolved);
                        continue;
                    }
                }
            };

            let label = edge.label.clone();
            let valid_from = edge.valid_from;
            let provenance = edge.provenance.clone();
            let props = PropertyMap::new();
            let edge_id = importer.db.write(move |tx| {
                use crate::api::transaction::{WriteOps, WriteRequestOptions};
                let mut options = WriteRequestOptions::new().with_valid_from(valid_from);
                if let Some(provenance) = provenance {
                    options = options.with_provenance(provenance);
                }
                tx.create_edge_with_options(source, target, &label, props, options)
            })?;
            report.edges_created += 1;
            report.note_mapping("ref", &edge.source_field, "edge", &edge.label);

            if let Some(retract_at) = edge.retract_at {
                importer.db.retract_edge_with_provenance(
                    edge_id,
                    retract_at,
                    edge.provenance.clone(),
                )?;
                report.retractions += 1;
            }
        }
    }

    // Phase 3: node retractions (after edges, so detach co-retracts them).
    for entity in entities {
        let Some(retraction) = &entity.retraction else {
            continue;
        };
        let Some(node_id) = importer.key_to_id.get(&entity.key).copied() else {
            continue;
        };
        let provenance = retraction.provenance.clone();
        let result = if retraction.detach {
            importer.db.retract_node_detach_with_provenance(
                node_id,
                retraction.valid_to,
                provenance,
            )?
        } else {
            importer
                .db
                .retract_node_with_provenance(node_id, retraction.valid_to, provenance)?
        };
        if !result.already_retracted {
            report.retractions += 1;
            report.retractions += result.edges_retracted;
        }
    }

    Ok(())
}
