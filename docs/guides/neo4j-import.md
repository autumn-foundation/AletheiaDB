# Neo4j Import Guide (Issue #3356)

Migrate a Neo4j graph into AletheiaDB from its **CSV export** — the
`neo4j-admin database import` / `apoc.export.csv` self-describing typed-header
convention. The importer reads the header, derives a coercion plan
automatically (you never hand-write property/type mappings), streams rows
through the same chunked ACID commit path as the generic bulk importer
(Issue #3211), and emits a machine-readable **fidelity report** with a
`zero_loss` boolean.

> **Scope.** CSV only. A binary Neo4j `.dump` archive and the APOC Cypher-script
> dump (`apoc.export.cypher.all`) are **not** supported — see
> [Unsupported inputs](#unsupported-inputs-and-follow-ups).

## Quick start (CLI)

```bash
# Export from Neo4j first, e.g.:
#   neo4j-admin database import full ... (produces typed-header CSVs)
#   or CALL apoc.export.csv.all('graph.csv', {})

export ALETHEIADB_DATA_DIR=./mydata

aletheia import --format neo4j-csv \
  --nodes people.csv --nodes companies.csv \
  --relationships works_at.csv \
  --report import-report.json
```

The command requires building the CLI with `--features import`. It prints the
fidelity report as JSON to stdout (and to `--report <path>` if given).

### Worked example

`people.csv`:

```csv
:ID,:LABEL,name,age:int,skills:string[],joined:datetime
alice,Person;Employee,Alice,30,rust;graphs,2020-03-01T00:00:00Z
bob,Person,Bob,25,sql,2021-06-15T00:00:00Z
```

`works_at.csv`:

```csv
:START_ID,:END_ID,:TYPE,since:int
alice,acme,WORKS_AT,2020
```

`companies.csv`:

```csv
:ID,:LABEL,name
acme,Company,Acme Inc
```

```bash
aletheia import --format neo4j-csv \
  --nodes people.csv --nodes companies.csv \
  --relationships works_at.csv \
  --valid-from-property joined
```

Here `--valid-from-property joined` names the **property** `joined` — the part
before the `:type` in the `joined:datetime` header of `people.csv` above — so
that column must exist in the node files. Result: `alice` becomes a `Person`
node (label `Person`, with `_labels = ["Person","Employee"]`), `skills` becomes
a string array, `joined` sets each node's `valid_from` (so
`AS OF VALID_TIME '2020-03-01'` sees Alice) and is consumed as valid time rather
than stored as a property, and every version carries
`provenance.source = "neo4j-import::people.csv"`.

## Rust API

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::api::import::{Neo4jCsvOptions, LabelStrategy};
use std::path::PathBuf;

let db = AletheiaDB::new()?;
let mut importer = db.import();

let opts = Neo4jCsvOptions::new()
    .array_delimiter(';')
    .vector_property("embedding")     // numeric[] -> Vector (opt-in, never silent)
    .valid_from_property("joined")    // date/datetime column -> valid_from
    .label_strategy(LabelStrategy::First);

let report = importer.neo4j_import_csv(
    &[PathBuf::from("people.csv"), PathBuf::from("companies.csv")],
    &[PathBuf::from("works_at.csv")],
    &opts,
)?;

assert!(report.zero_loss);
println!("{} nodes, {} rels", report.nodes_imported, report.relationships_imported);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Single-file entry points also exist:
`Importer::neo4j_nodes_from_csv(path, &opts)` and
`Importer::neo4j_edges_from_csv(path, &opts)` (import nodes first so edge
endpoints resolve).

## Supported vs. rejected header/type matrix

### Reserved headers

| Header | Meaning |
|---|---|
| `:ID` | Node business key (used to resolve relationship endpoints). Not stored as a property. |
| `name:ID` | Named id column — the id is **also** stored as property `name`. |
| `:ID(group)` | Id-space / group. Keys are namespaced internally (`group\0id`) so the same raw id in two groups never collides. |
| `:LABEL` | Node label(s); multiple labels separated by the array delimiter (`Person;Employee`). |
| `:START_ID` / `:START_ID(group)` | Relationship source endpoint. |
| `:END_ID` / `:END_ID(group)` | Relationship target endpoint. |
| `:TYPE` | Relationship type -> edge label. |
| `:IGNORE` | Column dropped entirely. |

### Property types (`name:type`, untyped defaults to `string`)

| Neo4j type | Maps to | Notes |
|---|---|---|
| `string`, untyped | `String` | |
| `int`, `long` | `Int` (i64) | |
| `short` (scalar) | `Int` (i64) | range-checked via `i16`; out-of-range is a clean row error. Recorded as a `short_to_int` coercion |
| `byte` (scalar) | `Int` (i64) | range-checked via `i8`; out-of-range is a clean row error. Recorded as a `byte_to_int` coercion |
| `float`, `double` | `Float` (f64) | a non-finite value (e.g. `1e400` overflowing to `inf`) is a clean row error, never stored as `inf` |
| `boolean` | `Bool` | matches Neo4j's `Boolean.parseBoolean`: `true` **only** when the value case-insensitively equals `"true"`; every other value (`1`, `yes`, `t`, ...) is `false`, never an error |
| `char` | 1-char `String` | requires **exactly one** character (`"AB"` is a clean row error); recorded as a `char_to_string` coercion |
| `date`, `datetime`, `localdatetime` | `Int` (epoch microseconds) | recorded as `temporal_to_micros`; a trailing bracketed IANA zone id (`...+01:00[Europe/London]`) is stripped before parsing |
| `time`, `localtime` | `String` | no epoch anchor; recorded as `time_to_string` |
| `T[]` (any supported scalar) | `Array` | split on `--array-delimiter` (default `;`), **quote-aware**: an element quoted with the CSV quote char may itself contain the delimiter (`"a;b";c` → `["a;b","c"]`) |
| numeric `T[]` **+ `--vector-property`** | `Vector` | explicit opt-in only, **never silent** |
| **`duration`** | **rejected** | reported under `unsupported` with a count |
| **`point` / spatial** | **rejected** | reported under `unsupported` with a count |
| **`byte[]`** | **rejected** | reported under `unsupported` with a count |

With `--strict-types`, an unsupported-type header is a **hard error** at
header-parse time (before any transaction opens) instead of a
reported-and-counted skip.

## Mapping decisions

- **Multi-label nodes.** AletheiaDB nodes have exactly one label. By default
  (`--label-strategy first`) the **first** label wins and the full set is
  preserved as a `_labels` array property (deterministic and lossless).
  `--label-strategy concat` produces a single synthetic label
  (`Person_Employee`, no `_labels`); `--label-strategy property` keeps the first
  label and **always** stores `_labels`. The retained-labels key defaults to
  `_labels`.
- **Id-spaces.** `:ID(group)` / `:START_ID(group)` / `:END_ID(group)` resolve
  within their group, so the same raw id string in two groups is two distinct
  nodes.
- **Arrays.** `name:type[]` splits on the array delimiter into a
  `PropertyValue::Array`. An empty array cell becomes an *absent* property.
- **Vectors.** A numeric array becomes a dense `PropertyValue::Vector` **only**
  when its property name is passed via `--vector-property` (Rust:
  `Neo4jCsvOptions::vector_property`). Otherwise it stays an `Array`.
- **Valid time.** By default a fact's `valid_from` is the import (transaction)
  time. `--valid-from-property <name>` designates a `date`/`datetime` column as
  each row's `valid_from`; that column is consumed as valid time and not also
  stored as a property.
- **Provenance.** Every imported node/edge version carries
  `provenance.source = "neo4j-import::<file>"` (the basename of the file it came
  from). Existing (non-Neo4j) importers are unchanged — they record no
  provenance.

## Failure modes and error reporting

The importer honors the #3211 contract:

- `--on-error abort` (default): the first malformed row / unresolved endpoint
  returns an error with a precise location; already-committed chunks persist.
- `--on-error skip`: malformed rows and unresolved endpoints are collected in
  the report (`skipped`, `unresolved_endpoints`) and the import continues.
- **Header / structural errors** (missing `:ID`, missing `:START_ID`/`:END_ID`/
  `:TYPE`, missing header row, a **duplicate header column**, an **unknown
  reserved header** like `:FOO`, a **malformed id-space** like `:ID(`, or a
  `--strict-types` unsupported type) are hard errors regardless of failure mode
  — they happen before any transaction opens.

**Multi-file error attribution.** Row numbering restarts at 1 for each file, so
across a multi-file import an error names the **source file** as well as the
row: `broken.csv row 3: id column ':ID' is empty` /
`links.csv row 5: unresolved target key 'ghost'`. Both `RowError` and
`UnresolvedEndpoint` carry an optional `file` field (serialized only when
present); the single-file generic CSV/JSONL importers leave it unset and render
as `row N: ...` exactly as before.

## Fidelity report

`Neo4jFidelityReport` (serde-serializable) is a superset of the generic
`ImportReport`:

| Field | Meaning |
|---|---|
| `rows_read`, `nodes_imported`, `relationships_imported`, `properties_imported` | counts |
| `label_mapping` | neo4j-label-set → AletheiaDB-label table with counts |
| `type_mapping` | neo4j-rel-type → edge-label table with counts |
| `skipped` | malformed rows (skip mode) with `row N:` messages |
| `unresolved_endpoints` | edges whose endpoints didn't resolve (skip mode) |
| `coerced` | value transformations (char, byte/short, temporal, multi-label) with counts (one per (row, column)) |
| `unsupported` | dropped columns (`duration`, `point`, `byte[]`) with counts |
| `zero_loss` | `true` iff `skipped`, `unresolved_endpoints`, and `unsupported` are all empty |

Counts in `label_mapping`, `type_mapping`, `coerced`, and `unsupported` reflect
**only rows that were actually imported** — in `--on-error skip` a row that is
later skipped (bad `:ID`) or has an unresolved endpoint contributes nothing to
these tables. A `coerced` entry counts **rows affected**, so an array column
whose every element is coerced still counts once per row.

### What `zero_loss` means (precisely)

`zero_loss` reports whether any **rows or columns were dropped or rejected** —
it is `true` iff `skipped`, `unresolved_endpoints`, and `unsupported` are all
empty. It is deliberately **not** affected by lossy *type* coercions: a `char`
stored as a 1-character string, a `date`/`datetime` stored as epoch
microseconds (`temporal_to_micros`), a `time` stored as a string
(`time_to_string`), and a `byte`/`short` widened to a 64-bit `Int`
(`byte_to_int` / `short_to_int`) are all surfaced in the `coerced` table for
auditability but do **not** flip `zero_loss` to `false`. Every input row and
column was imported; only its representation changed. Inspect `coerced` when you
need to know which type transformations were applied.

## Unsupported inputs and follow-ups

- **Binary Neo4j dump (`.dump`).** A proprietary, versioned page-store archive,
  not a documented interchange format. The CLI rejects it with a clear message
  pointing you to CSV export. This is not planned.
- **APOC Cypher-script dump (`apoc.export.cypher.all`).** A text file of
  `CREATE (...)` statements. Rejected in v1 with a message pointing you to CSV.
  A **documented follow-up** would reuse the `cypher` parser to route parsed
  `CREATE`s through the same prepared-chunk path and report constraint/index/
  procedure statements as skipped-with-count.
- **Constraints / indexes / procedures.** Out of scope (report-only in a future
  Cypher-script path); AletheiaDB does not import Neo4j schema objects.

## Known limitations

- **Array properties and durable index persistence.** AletheiaDB's index
  persistence (the fast-restart snapshot) does not yet serialize `Array`
  properties. When you import multi-label nodes (which create a `_labels` array)
  or array-typed columns into a **durable** data directory, index persistence
  logs a warning and skips the snapshot for those; the data is still durable via
  the WAL and reconstructed on restart by WAL replay. This is a pre-existing
  index-persistence limitation, independent of the importer. Use
  `--label-strategy concat` if you want to avoid `_labels` arrays entirely.
- **RFC-4180 quoting only.** Doubled-quote escaping (`""`); the non-RFC
  `\"`-style (`--legacy-style-quoting=true` in Neo4j) is not supported.
- **Header is row 1 of each data file.** Standalone header files are not yet
  supported.
