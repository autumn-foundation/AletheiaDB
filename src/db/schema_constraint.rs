//! Property-type and required-key schema constraints (Issue #3378).
//!
//! Constraints are **opt-in**: a `(kind, label)` with no declaration is fully
//! schemaless and behaves exactly as before. Declaring constraints scans the
//! current state for conformance, and — once active — every buffered write is
//! validated at commit time (see
//! [`crate::api::transaction::write::constraint::check_constraints`]).
//!
//! ## Forward-only temporal semantics
//!
//! Enabling scans **current state** only and enforcement applies to **new
//! writes** only; pre-existing history is never re-scanned or invalidated
//! (AC: time-travel reads of superseded versions keep working). A backdated
//! (`valid_time`) write is recorded at transaction-time *now*, so it is
//! validated against the constraint set active *now* (AC7) — a backdated write
//! that violates a currently-active constraint is rejected.
//!
//! ## Durability
//!
//! Active constraints are persisted to a bitcode+CRC sidecar under the data dir
//! (`schema_constraints.dat`), written atomically (temp → fsync → rename) on
//! every declare/drop and loaded at startup. They are also folded into the
//! `.albk` backup payload, so a backup→restore round-trip preserves them.
//!
//! Note: the pre-existing uniqueness constraints (Issue #3218) are persisted via
//! the WAL and are **not** captured in `.albk` today; the schema constraints
//! added here *are*.

use crate::core::changefeed::EntityKind;
use crate::core::constraint::{
    ConformanceReport, ConformanceViolation, ConstraintRegistry, DeclaredType,
    MAX_CONFORMANCE_SAMPLE_IDS, PropertyConstraint, PropertyConstraintDescriptor,
    SchemaConstraintDescriptor,
};
use crate::core::error::{ConstraintError, Result, ResultExt};
use crate::core::interning::GLOBAL_INTERNER;
use crate::db::AletheiaDB;
use bitcode::{Decode, Encode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Maximum sidecar size accepted on load (DoS guard).
const MAX_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;

/// Current sidecar format version.
const SIDECAR_VERSION: u16 = 1;

/// On-disk sidecar wrapper (bitcode + CRC), mirroring the index-persistence
/// durable-file convention (non-optional, works without the `serde` feature).
#[derive(Debug, Clone, Encode, Decode)]
pub(crate) struct SchemaConstraintSidecar {
    /// Format version for forward compatibility.
    pub version: u16,
    /// The declared schema constraints, string-keyed.
    pub constraints: Vec<SchemaConstraintDescriptor>,
}

/// The sidecar file path under a data directory.
pub(crate) fn sidecar_path(data_dir: &Path) -> PathBuf {
    data_dir.join("schema_constraints.dat")
}

/// Tolerantly load the sidecar into `registry`. Missing file → no-op; a
/// corrupt/undecodable file is quarantined (renamed aside) and treated as
/// empty so startup never bricks.
pub(crate) fn load_sidecar(path: &Path, registry: &ConstraintRegistry) {
    if !path.exists() {
        return;
    }

    // Quarantine the file aside (preserved for diagnosis) and start with no
    // schema constraints — a bad sidecar must never brick startup. Shared by
    // the decode-error and incompatible-version paths (FIX-3).
    let quarantine_aside = |reason: &str| {
        let quarantine = path.with_extension(format!(
            "dat.corrupt-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or(0)
        ));
        let _ = std::fs::rename(path, &quarantine);
        #[cfg(feature = "observability")]
        tracing::warn!(
            "schema-constraint sidecar at {} was unusable ({reason}); quarantined to {} and starting with no schema constraints",
            path.display(),
            quarantine.display()
        );
        #[cfg(not(feature = "observability"))]
        let _ = reason;
    };

    match crate::storage::index_persistence::common::load_encoded_with_crc::<SchemaConstraintSidecar>(
        path,
        MAX_SIDECAR_BYTES,
        "schema constraints",
    ) {
        Ok(sidecar) => {
            // Validate the format version before importing (FIX-3): a
            // future/unknown version may encode a shape this build would
            // misinterpret, so quarantine it and start empty rather than
            // silently importing a possibly-misread set.
            if sidecar.version != SIDECAR_VERSION {
                quarantine_aside(&format!(
                    "incompatible sidecar version {} (supported {})",
                    sidecar.version, SIDECAR_VERSION
                ));
                return;
            }
            registry.import_schema_constraints(&sidecar.constraints);
        }
        Err(_e) => {
            quarantine_aside(&format!("decode failed: {_e}"));
        }
    }
}

/// Persist a fixed descriptor set to the sidecar (atomic write). No-op when the
/// database is ephemeral (no path).
fn write_sidecar_descriptors(
    path: &Path,
    descriptors: &[SchemaConstraintDescriptor],
) -> Result<()> {
    let sidecar = SchemaConstraintSidecar {
        version: SIDECAR_VERSION,
        constraints: descriptors.to_vec(),
    };
    crate::storage::index_persistence::common::save_encoded_with_crc(&sidecar, path).map_err(|e| {
        crate::core::error::Error::Other(format!(
            "failed to persist schema-constraint sidecar: {e}"
        ))
    })
}

/// Write the schema-constraint sidecar into `data_dir` from an explicit
/// descriptor set (used by the `.albk` restore path so the reopened database
/// loads the restored constraints through the normal startup sidecar-load).
/// A no-op for an empty descriptor set.
pub(crate) fn persist_descriptors_to_dir(
    data_dir: &Path,
    descriptors: &[SchemaConstraintDescriptor],
) -> Result<()> {
    if descriptors.is_empty() {
        return Ok(());
    }
    write_sidecar_descriptors(&sidecar_path(data_dir), descriptors)
}

/// A single property specification accumulated by the builder.
struct PropertySpec {
    property: String,
    declared_type: Option<DeclaredType>,
    required: bool,
    /// Per-key nullability override (Issue #3378 FIX-4). `None` inherits the
    /// builder-wide `default_nullable`; `Some(b)` pins this key's nullability
    /// regardless of the default, so one key can be nullable and another
    /// non-nullable in the same chain.
    nullable: Option<bool>,
}

/// Builder for declaring and enabling property-type / required-key schema
/// constraints on a `(kind, label)` pair (Issue #3378).
///
/// # Example
///
/// ```rust,no_run
/// # use aletheiadb::AletheiaDB;
/// # use aletheiadb::core::constraint::DeclaredType;
/// # use aletheiadb::core::EntityKind;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = AletheiaDB::new()?;
/// db.schema_constraint(EntityKind::Node, "Person")
///     .require("name")
///     .require_typed("age", DeclaredType::Integer)
///     .typed("email", DeclaredType::String)
///     .enable()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "call .enable() to activate the schema constraints"]
pub struct SchemaConstraintBuilder<'a> {
    db: &'a AletheiaDB,
    kind: EntityKind,
    label: String,
    specs: Vec<PropertySpec>,
    default_nullable: bool,
    dry_run: bool,
}

impl<'a> SchemaConstraintBuilder<'a> {
    pub(crate) fn new(db: &'a AletheiaDB, kind: EntityKind, label: impl Into<String>) -> Self {
        Self {
            db,
            kind,
            label: label.into(),
            specs: Vec::new(),
            default_nullable: true,
            dry_run: false,
        }
    }

    /// Require `property` to be present with a non-null value (any type).
    pub fn require(mut self, property: impl Into<String>) -> Self {
        self.specs.push(PropertySpec {
            property: property.into(),
            declared_type: None,
            required: true,
            nullable: None,
        });
        self
    }

    /// Require `property` to be present with a non-null value of the given type.
    pub fn require_typed(mut self, property: impl Into<String>, ty: DeclaredType) -> Self {
        self.specs.push(PropertySpec {
            property: property.into(),
            declared_type: Some(ty),
            required: true,
            nullable: None,
        });
        self
    }

    /// Constrain `property` to the given type when present (optional key).
    pub fn typed(mut self, property: impl Into<String>, ty: DeclaredType) -> Self {
        self.specs.push(PropertySpec {
            property: property.into(),
            declared_type: Some(ty),
            required: false,
            nullable: None,
        });
        self
    }

    /// Constrain `property` to the given type when present (optional key) with
    /// an explicit per-key nullability (Issue #3378 FIX-4), overriding the
    /// builder-wide [`nullable`](Self::nullable) default for THIS key only.
    /// `nullable == false` means a present explicit `Null` violates the
    /// constraint; `true` permits it. Lets one key be nullable and another
    /// non-nullable in the same chain.
    pub fn typed_nullable(
        mut self,
        property: impl Into<String>,
        ty: DeclaredType,
        nullable: bool,
    ) -> Self {
        self.specs.push(PropertySpec {
            property: property.into(),
            declared_type: Some(ty),
            required: false,
            nullable: Some(nullable),
        });
        self
    }

    /// Set the builder-wide default for whether an explicit `Null` value is
    /// permitted on optional keys (default `true`). `false` means a present
    /// `Null` violates the constraint. A per-key
    /// [`typed_nullable`](Self::typed_nullable) override takes precedence over
    /// this default for that key.
    pub fn nullable(mut self, nullable: bool) -> Self {
        self.default_nullable = nullable;
        self
    }

    /// Compute (but do not apply) the conformance report against current state.
    /// The returned report's `conforms` reflects whether the current state
    /// satisfies the constraints; nothing is declared.
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
    }

    /// Scan current state for conformance and, unless `dry_run`, activate the
    /// constraints (updating the registry and persisting the sidecar).
    ///
    /// - `dry_run` → returns the report without applying.
    /// - non-conforming (non-dry-run) → returns
    ///   [`ConstraintError::NonConformingOnEnable`]; nothing is declared.
    /// - conforming (non-dry-run) → declares + persists, returns the report.
    pub fn enable(self) -> Result<ConformanceReport> {
        self.db.enable_schema_constraints(
            self.kind,
            &self.label,
            self.specs,
            self.default_nullable,
            self.dry_run,
        )
    }
}

impl AletheiaDB {
    /// Begin declaring property-type / required-key schema constraints for a
    /// `(kind, label)` pair (Issue #3378). Call `.enable()` on the returned
    /// builder to scan current state and activate.
    #[must_use = "call .enable() to activate the schema constraints"]
    pub fn schema_constraint(
        &self,
        kind: EntityKind,
        label: impl Into<String>,
    ) -> SchemaConstraintBuilder<'_> {
        SchemaConstraintBuilder::new(self, kind, label)
    }

    /// List all declared schema constraints as serializable descriptors.
    pub fn list_schema_constraints(&self) -> Vec<SchemaConstraintDescriptor> {
        self.constraint_registry.export_schema_constraints()
    }

    /// Drop the schema constraints declared on `(kind, label)`. Returns `true`
    /// if constraints were removed. Persists the change to the sidecar.
    pub fn drop_schema_constraint(&self, kind: EntityKind, label: &str) -> Result<bool> {
        let label_id = match GLOBAL_INTERNER.get_id(label) {
            Some(id) => id,
            None => return Ok(false), // never declared — nothing to do
        };

        // Serialize the whole check→persist→drop read-modify-write against
        // concurrent enable/drop so the sidecar file and the in-memory registry
        // stay consistent (Issue #3378 FIX-1). Leaf lock; released on return.
        let _ddl = self.constraint_registry.lock_schema_ddl();

        if self
            .constraint_registry
            .schema_constraints_for(kind, label_id)
            .is_none()
        {
            return Ok(false);
        }

        // Persist the post-drop descriptor set first, then mutate memory, so a
        // failed write leaves the active set unchanged.
        let remaining: Vec<SchemaConstraintDescriptor> = self
            .constraint_registry
            .export_schema_constraints()
            .into_iter()
            .filter(|d| !(d.label == label && d.entity_kind == kind.as_str()))
            .collect();
        if let Some(ref path) = self.schema_constraint_path {
            write_sidecar_descriptors(path, &remaining)?;
        }
        Ok(self
            .constraint_registry
            .drop_schema_constraints(kind, label_id))
    }

    /// Internal: scan + (optionally) apply schema constraints. See
    /// [`SchemaConstraintBuilder::enable`].
    fn enable_schema_constraints(
        &self,
        kind: EntityKind,
        label: &str,
        specs: Vec<PropertySpec>,
        default_nullable: bool,
        dry_run: bool,
    ) -> Result<ConformanceReport> {
        let label_id = GLOBAL_INTERNER.intern(label).record_error_metric()?;

        // Build the in-memory constraints (interning each property key).
        let mut constraints: Vec<PropertyConstraint> = Vec::with_capacity(specs.len());
        let mut descriptors: Vec<PropertyConstraintDescriptor> = Vec::with_capacity(specs.len());
        for spec in &specs {
            let prop_id = GLOBAL_INTERNER
                .intern(&spec.property)
                .record_error_metric()?;
            // Per-key nullability override (FIX-4), else the builder-wide default.
            let nullable = spec.nullable.unwrap_or(default_nullable);
            constraints.push(PropertyConstraint {
                property: prop_id,
                declared_type: spec.declared_type,
                required: spec.required,
                nullable,
            });
            descriptors.push(PropertyConstraintDescriptor {
                property: spec.property.clone(),
                declared_type: spec.declared_type,
                required: spec.required,
                nullable,
            });
        }

        // Scan current state for conformance.
        let report = self.scan_conformance(kind, label, label_id, &constraints);

        if dry_run {
            return Ok(report);
        }
        if !report.conforms {
            return Err(ConstraintError::NonConformingOnEnable {
                entity_kind: kind.as_str().to_string(),
                label: label.to_string(),
                violations: report.violations.clone(),
                total_non_conforming: report.total_non_conforming,
                sample_ids: collect_sample_ids(&report.violations),
            }
            .into());
        }

        // Serialize the export→persist→declare read-modify-write against
        // concurrent enable/drop (Issue #3378 FIX-1): without this, two enables
        // on different labels each export the (pre-other) set, then the second
        // sidecar write clobbers the first label from disk (in-memory stays
        // correct, but the constraint vanishes on restart). Leaf lock; held
        // across the whole critical section below and released on return.
        let _ddl = self.constraint_registry.lock_schema_ddl();

        // Persist the new descriptor set first (durable before active), then
        // update the registry so an in-flight write observes it.
        let mut target = self.constraint_registry.export_schema_constraints();
        target.retain(|d| !(d.label == label && d.entity_kind == kind.as_str()));
        target.push(SchemaConstraintDescriptor {
            entity_kind: kind.as_str().to_string(),
            label: label.to_string(),
            properties: descriptors,
        });
        if let Some(ref path) = self.schema_constraint_path {
            write_sidecar_descriptors(path, &target)?;
        }
        self.constraint_registry
            .declare_schema_constraints(kind, label_id, constraints);

        Ok(report)
    }

    /// Scan current-state entities of `(kind, label)` against `constraints`,
    /// aggregating a [`ConformanceReport`].
    fn scan_conformance(
        &self,
        kind: EntityKind,
        label: &str,
        label_id: crate::core::interning::InternedString,
        constraints: &[PropertyConstraint],
    ) -> ConformanceReport {
        // Aggregate: (property, reason) -> sample ids (bounded).
        let mut agg: BTreeMap<(Option<String>, String), Vec<u64>> = BTreeMap::new();
        let mut total_checked = 0usize;
        let mut total_non_conforming = 0usize;

        let mut record = |id: u64, violations: Vec<(Option<String>, String)>| {
            total_checked += 1;
            if violations.is_empty() {
                return;
            }
            total_non_conforming += 1;
            for v in violations {
                let ids = agg.entry(v).or_default();
                if ids.len() < MAX_CONFORMANCE_SAMPLE_IDS {
                    ids.push(id);
                }
            }
        };

        match kind {
            EntityKind::Node => {
                for node in self.current.get_nodes_by_label(label) {
                    let vios =
                        ConstraintRegistry::evaluate_conformance(constraints, &node.properties);
                    record(node.id.as_u64(), vios);
                }
            }
            EntityKind::Edge => {
                self.current.visit_edges(|edge| {
                    if edge.label != label_id {
                        return;
                    }
                    let vios =
                        ConstraintRegistry::evaluate_conformance(constraints, &edge.properties);
                    record(edge.id.as_u64(), vios);
                });
            }
        }

        let violations: Vec<ConformanceViolation> = agg
            .into_iter()
            .map(|((property, reason), sample_ids)| ConformanceViolation {
                entity_kind: kind.as_str().to_string(),
                label: label.to_string(),
                property,
                reason,
                sample_ids,
            })
            .collect();

        ConformanceReport {
            conforms: total_non_conforming == 0,
            total_checked,
            total_non_conforming,
            violations,
        }
    }
}

/// Collect a bounded, de-duplicated sample of offending ids across violations.
fn collect_sample_ids(violations: &[ConformanceViolation]) -> Vec<u64> {
    let mut seen = std::collections::BTreeSet::new();
    for v in violations {
        for id in &v.sample_ids {
            seen.insert(*id);
            if seen.len() >= MAX_CONFORMANCE_SAMPLE_IDS {
                break;
            }
        }
    }
    seen.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIX-3: a sidecar whose `version` is not [`SIDECAR_VERSION`] must be
    /// quarantined aside (not silently imported) and the registry left empty.
    #[test]
    fn future_version_sidecar_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path());

        // Write a well-formed sidecar (valid bitcode + CRC) that merely claims
        // a newer format version than this build supports.
        let future = SchemaConstraintSidecar {
            version: SIDECAR_VERSION + 1,
            constraints: Vec::new(),
        };
        crate::storage::index_persistence::common::save_encoded_with_crc(&future, &path).unwrap();
        assert!(path.exists());

        let registry = ConstraintRegistry::new();
        load_sidecar(&path, &registry);

        // Quarantined aside (renamed), registry left empty — no misread import.
        assert!(
            !path.exists(),
            "future-version sidecar must be renamed aside, not left in place"
        );
        assert!(registry.export_schema_constraints().is_empty());
    }

    /// A current-version sidecar round-trips normally through the same path.
    #[test]
    fn current_version_sidecar_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = sidecar_path(dir.path());

        let sidecar = SchemaConstraintSidecar {
            version: SIDECAR_VERSION,
            constraints: vec![SchemaConstraintDescriptor {
                entity_kind: "node".to_string(),
                label: "Person".to_string(),
                properties: vec![PropertyConstraintDescriptor {
                    property: "name".to_string(),
                    declared_type: None,
                    required: true,
                    nullable: true,
                }],
            }],
        };
        crate::storage::index_persistence::common::save_encoded_with_crc(&sidecar, &path).unwrap();

        let registry = ConstraintRegistry::new();
        load_sidecar(&path, &registry);

        assert!(path.exists(), "a valid current-version sidecar is not moved");
        let listed = registry.export_schema_constraints();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Person");
    }
}
