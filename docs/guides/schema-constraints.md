# Schema Constraints: Property Types & Required Keys (Issue #3378)

AletheiaDB is schemaless by default. **Schema constraints** let you *opt in* to
declaring, per node-label or edge-type, that certain properties must be present
and/or must hold a specific type. Once declared, every write is validated at
commit time; a violation aborts the whole transaction with **zero** partial
application.

This is the Rust-API surface. Declaring constraints via MCP/CLI tools and via
AQL/Cypher DDL is a follow-up (Lane 1 / Issue #560); the MCP error surface
already classifies constraint failures with structured `#3234` codes (see
[Error contract](#error-contract-3234)).

## Quick start

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::core::EntityKind;
use aletheiadb::core::constraint::DeclaredType;

let db = AletheiaDB::new()?;

db.schema_constraint(EntityKind::Node, "Person")
    .require("name")                              // required, any type
    .require_typed("age", DeclaredType::Integer)  // required + typed
    .typed("email", DeclaredType::String)         // optional but typed
    .enable()?;                                    // scans current state, activates

// Rejected: age is a string, not an int -> ConstraintError::TypeViolation
db.create_node("Person", properties!{ "name" => "Alice", "age" => "thirty" })?;

// Rejected: missing required key `name` -> ConstraintError::MissingRequiredKey
db.create_node("Person", properties!{ "age" => 30 })?;
```

A label with **no** declaration is fully schemaless — zero behavior change,
zero overhead on the write path.

## Declared types

`DeclaredType` maps each declarable type to the concrete `PropertyValue` it
accepts:

| `DeclaredType`             | Accepts `PropertyValue`         |
|----------------------------|---------------------------------|
| `String`                   | `String`                        |
| `Integer`                  | `Int`                           |
| `Float`                    | `Float`                         |
| `Boolean`                  | `Bool`                          |
| `Bytes`                    | `Bytes`                         |
| `Temporal`                 | `Int` (micros since epoch)      |
| `Vector { dim: None }`     | `Vector` (any dimension)        |
| `Vector { dim: Some(d) }`  | `Vector` of length exactly `d`  |

**Temporal note:** AletheiaDB has no dedicated temporal `PropertyValue` — a
timestamp is stored as a microseconds-since-epoch `Int`. `DeclaredType::Temporal`
therefore matches `Int`. (If genuinely ambiguous with a plain integer, prefer
`Integer`; `Temporal` exists to document intent.)

## Required / nullable / type semantics

For each declared property constraint, given the entity's **effective** property
map:

- **`required`** — the key must be present with a **non-null** value. A missing
  key *or* an explicit `Null` value yields `MissingRequiredKey`.
- **`nullable`** (builder default `true`, set via `.nullable(false)`) — governs
  whether an explicit `Null` is permitted on an *optional* key. With
  `nullable(false)`, a present `Null` yields `MissingRequiredKey`.
- **`declared_type`** — a present, non-null value whose type does not match the
  declared type yields `TypeViolation`. `Null`/absent values skip the type check
  (presence is governed by `required`).

`Null` is treated as "absent" for presence purposes, consistent with the
uniqueness-constraint convention.

## Update (PATCH) semantics

`update_node` / `update_edge` are **PATCH** updates: the new properties are
merged onto the entity's existing map, and constraints are validated against the
**effective post-write** map. So:

- A patch that doesn't touch a required key does **not** falsely fail.
- A patch that sets a required key to `Null` **does** fail (`MissingRequiredKey`).
- A patch that changes a typed key to the wrong type **does** fail
  (`TypeViolation`).

## Declaring on a populated label: the conformance report

`enable()` first scans **current-state** entities of the target `(kind, label)`
and returns a `ConformanceReport`:

```rust
pub struct ConformanceReport {
    pub conforms: bool,
    pub total_checked: usize,
    pub total_non_conforming: usize,
    pub violations: Vec<ConformanceViolation>, // aggregated, with sample ids
}
```

- **All current entities conform** → constraints are activated and the report is
  returned (`conforms: true`).
- **Some don't conform** → `enable()` returns
  `ConstraintError::NonConformingOnEnable { total_non_conforming, sample_ids,
  violations, .. }` and **nothing is declared** (atomic). Fix or retract the
  offending data, then re-issue.
- **`.dry_run()`** → returns the report without applying anything (works for both
  conforming and non-conforming states), so you can preview impact before
  committing to a declaration.

```rust
let report = db.schema_constraint(EntityKind::Node, "Person")
    .require_typed("age", DeclaredType::Integer)
    .dry_run()
    .enable()?;
if !report.conforms {
    // inspect report.violations (each carries a bounded sample of offending ids)
}
```

## Listing, dropping, and `get_schema`

```rust
let all = db.list_schema_constraints();               // Vec<SchemaConstraintDescriptor>
let removed = db.drop_schema_constraint(EntityKind::Node, "Person")?; // bool
```

`get_schema()` / `schema_as_of()` now carry a `declared_constraints` field on
each `LabelSchema` / `EdgeTypeSchema`, so callers can distinguish **declared**
keys from merely **observed** ones (`property_keys`). A key can be declared but
not yet observed (nothing written with it), or observed but not declared.

## Forward-only temporal semantics

Constraints are **forward-only**:

- `enable()` scans **current state** only; pre-existing history is **never**
  re-scanned or invalidated. A superseded version that would violate a
  newly-declared constraint is untouched, and time-travel reads of it keep
  working. Constraints never block *reads*.
- Enforcement applies only to **new writes** buffered after the declaration.

### Backdated (`valid_time`) writes — AC7

A backdated write (one with a past `valid_time`) is still **recorded at
transaction-time = now**, so it is validated against the constraint set active
*now*. A backdated write that violates a currently-active constraint is
**rejected**. Valid-time is about *when a fact was true*; the constraint set that
applies is the one in force at the moment you record it.

## Atomicity

The constraint check runs in the commit path **before** timestamp allocation,
WAL append, and apply — at the same hook as uniqueness (#3218). A multi-op
transaction with a single violating op therefore applies **nothing**.

## Error contract (#3234)

Constraint failures surface as structured MCP/HTTP errors:

| `ConstraintError`         | MCP code                | `retriable` |
|---------------------------|-------------------------|-------------|
| `TypeViolation`           | `CONSTRAINT_VIOLATION`  | `false`     |
| `MissingRequiredKey`      | `CONSTRAINT_VIOLATION`  | `false`     |
| `NonConformingOnEnable`   | `FAILED_PRECONDITION`   | `false`     |

Recovery: a `CONSTRAINT_VIOLATION` is repaired from the message + details (fix
the value / add the key) and re-issued; a `FAILED_PRECONDITION` on enable means
fix the offending data first. All are non-retriable.

## Durability & backup

Active constraints are persisted to a sidecar under the data directory —
`schema_constraints.dat` (bitcode + CRC, written atomically temp→fsync→rename,
mirroring index persistence). It is loaded at startup (after WAL/index replay has
restored the string interner), tolerant of a missing file (empty) and of a
corrupt file (quarantined aside, warn, start empty — never bricks startup).

An **ephemeral** `AletheiaDB::new()` keeps constraints in memory only (no file).

Schema constraints are also folded into the `.albk` backup payload, so a
**backup → restore** round-trip preserves them.

> **Residue note.** The pre-existing **uniqueness** constraints (Issue #3218)
> are persisted via the WAL and are **not** captured in `.albk` today — a
> backup→restore loses them. The schema constraints added here **are** captured.
> This asymmetry is intentional for this change and tracked as a uniqueness
> follow-up.

## Not in this change (follow-ups)

- MCP/CLI tools to declare/drop/list schema constraints (Lane 1).
- AQL/Cypher DDL (`CREATE CONSTRAINT ...`, Issue #560).
- Serializing `declared_constraints` through the MCP `get_schema` response
  (the struct field exists; the MCP serializer wiring is a Lane 1 follow-up).
