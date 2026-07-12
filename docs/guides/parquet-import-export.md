# Parquet Import / Export (Issue #3364)

AletheiaDB can import and export nodes and edges as [Apache Parquet](https://parquet.apache.org/)
columnar files for analytics interoperability — load a graph from a data-engineering
pipeline, or export the current graph (or its full bi-temporal history) for downstream
analysis in DuckDB, pandas/pyarrow, Spark, or any Parquet-aware tool.

The columnar schema is **stable and documented** (below), and the export/import halves
share it, so an **export → import round-trip reproduces the graph** for scalars,
timestamps, and dense-vector embeddings.

## Feature flag

Both directions live behind the optional `parquet` feature (off by default, so the
`arrow`/`parquet` dependency stays out of default builds):

```toml
[dependencies]
aletheiadb = { version = "0.3", features = ["parquet"] }
```

The `parquet` feature implies the `import` feature (Issue #3211), whose
`NodeMapping` / `EdgeMapping` / `ColumnType` types drive the import side.

---

## Rust API

### Import

Import reuses the #3211 [`Importer`](../../src/api/import/mod.rs) with two extra methods:

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::api::import::{ColumnType, EdgeMapping, LabelSource, NodeMapping};

let db = AletheiaDB::new()?;
let mut importer = db.import();

let nodes = NodeMapping::new(LabelSource::column("label"), "id")
    .property_same("name", ColumnType::String)
    .property_same("age", ColumnType::Int)
    .property_same("embedding", ColumnType::Embedding) // list<float32>, exact bits
    .valid_time_column("valid_time");
importer.nodes_from_parquet("nodes.parquet", nodes)?;

let edges = EdgeMapping::new(LabelSource::column("edge_type"), "source_key", "target_key")
    .property_same("since", ColumnType::Int);
importer.edges_from_parquet("edges.parquet", edges)?;
```

Column values are decoded **natively** from their Arrow types — integers, floats,
booleans, timestamps, and `list<float32>` embeddings never round-trip through a string.
A SQL null cell yields an **absent** property (not a stored null). All #3211
abort/skip-and-report semantics, precise `row N:` errors, and per-row `valid_time`
backfill apply. `ColumnType` values: `String`, `Int`, `Float`, `Bool`, `Timestamp`,
`Embedding`.

### Export

Export is driven by the [`Exporter`](../../src/api/export/mod.rs), obtained via
`db.export()`:

```rust
use aletheiadb::api::export::{ExportConfig, Exporter};

// Current-state export
db.export().nodes_to_parquet("nodes.parquet")?;
db.export().edges_to_parquet("edges.parquet")?;

// Full bi-temporal history export (one row per version)
db.export().node_history_to_parquet("node_history.parquet")?;
db.export().edge_history_to_parquet("edge_history.parquet")?;

// Tune the row-group / batch size (bounds peak memory)
db.export()
    .batch_size(16_384)
    .nodes_to_parquet("nodes.parquet")?;
```

Each method returns an `ExportReport` with `nodes_exported` / `edges_exported` (current
state) or `node_versions_exported` / `edge_versions_exported` (history).

---

## CLI

```bash
# Import nodes (and optionally edges) from Parquet
aletheia import nodes.parquet --format parquet --label Person --key id \
    --property name:string --property age:int --valid-time-column valid_time \
    --edges edges.parquet --edge-label KNOWS --source-key source_key --target-key target_key

# Export the current graph (writes <prefix>.nodes.parquet and <prefix>.edges.parquet)
aletheia export mygraph --format parquet --mode current

# Export full bi-temporal history
# (writes <prefix>.node_history.parquet and <prefix>.edge_history.parquet)
aletheia export mygraph --format parquet --mode history
```

`--label` sets a fixed label; use `--label-column COL` to read the label from a column.
Repeated `--property name:type` map columns to same-named properties
(`type` ∈ `string|int|float|bool|timestamp|embedding`). The export `<out_prefix>`
positional is a **path prefix**; the two files are derived from it. Both commands open
the database via `ALETHEIADB_CONFIG` / `ALETHEIADB_DATA_DIR` (like `backup`/`restore`).

---

## Schema

All timestamp columns are `timestamp(microseconds, UTC)`. Property columns are the
**union of all keys** discovered by scanning the database; each key gets one native
typed column decided by its value type. A key whose type **conflicts across rows**, or
whose value has no natural columnar form (bytes, heterogeneous array, sparse vector), is
routed to the `properties_json` overflow column instead (see below). An absent property
is written as a Parquet **null**.

File-level metadata records `aletheiadb.schema_version` (`"1"`), `aletheiadb.mode`
(`current` | `history`), and `aletheiadb.entity` (`node` | `edge`).

### Current-state node file

| Column | Type | Nullable | Notes |
|--------|------|----------|-------|
| `id` | int64 | no | Node id (business key). |
| `label` | string | no | Node label. |
| `valid_time` | timestamp(µs, UTC) | yes | Current version's valid-from. |
| *(one per property key)* | int64 / double / bool / string / list&lt;float32&gt; | yes | Scalars, or a dense embedding as `list<float32>`. |
| `properties_json` | string | yes | Overflow column (present only when needed). |

### Current-state edge file

| Column | Type | Nullable | Notes |
|--------|------|----------|-------|
| `edge_type` | string | no | Edge type / label. |
| `source_key` | int64 | no | Source node id (matches a node file's `id`). |
| `target_key` | int64 | no | Target node id. |
| `valid_time` | timestamp(µs, UTC) | yes | Current version's valid-from. |
| *(one per property key)* | int64 / double / bool / string / list&lt;float32&gt; | yes | |
| `properties_json` | string | yes | Overflow column (present only when needed). |

### History files (one row per version)

Node history: `id` (int64), `label` (string), the property columns, then the shared
temporal + provenance tail. Edge history is the same with `id`, `edge_type`,
`source_key` (nullable), `target_key` (nullable) leading, then the property columns and
the tail.

Shared tail columns:

| Column | Type | Nullable | Notes |
|--------|------|----------|-------|
| `version` | int64 | no | Sequential version number (1, 2, 3, …). |
| `valid_from` | timestamp(µs, UTC) | yes | Start of the version's valid interval. |
| `valid_to` | timestamp(µs, UTC) | yes | **Null = still-open interval** (never a sentinel). |
| `transaction_time` | timestamp(µs, UTC) | yes | When the version was recorded. |
| `tombstone` | bool | no | `true` when the version's valid interval is closed (the fact was deleted or retracted, ending its validity). |
| `provenance_source` | string | yes | #3224 provenance. |
| `provenance_confidence` | double | yes | |
| `provenance_note` | string | yes | |
| `provenance_correlation_id` | string | yes | |
| `provenance_principal` | string | yes | #3350 authenticated principal. |
| `properties_json` | string | yes | Overflow column (present only when needed). |

**Open-interval convention:** an open (still-valid or still-current) interval is written
as a JSON/Parquet **null** `valid_to`, never the internal `TIMESTAMP_MAX` sentinel, so a
downstream reader never mistakes it for a real far-future timestamp.

---

## The `properties_json` overflow column

Values that have no natural single-column form — `bytes`, heterogeneous `array`, and
`sparse_vector` — and any property **key whose observed type conflicts across rows** are
written to a single `properties_json` string column as a reversible **tagged-JSON**
object, e.g.:

```json
{
  "blob":   {"t": "bytes",  "v": [1, 2, 3, 255]},
  "tags":   {"t": "array",  "v": [{"t": "int", "v": 7}, {"t": "str", "v": "mixed"}]},
  "vec":    {"t": "sparse", "indices": [1, 5], "values": [0.5, -0.25], "dim": 16}
}
```

The default importer decodes the **native** columns; re-importing the overflow column
requires custom decoding of this tagged JSON (a documented follow-up). The encoding is
lossless and reversible.

---

## Streaming and memory

Both directions stream. Export scans the database once to discover the property schema,
then a second time to emit rows in `batch_size`-row Arrow `RecordBatch`es (default
8192), each flushed as one Parquet row group before the next is built. Import decodes
one `RecordBatch` at a time. Peak memory is therefore bounded to roughly one batch of
cells plus the set of distinct property keys — not the whole graph (target: 1M nodes in
well under 1 GB).

---

## Documented lossy items

- The HLC **logical counter** (the sub-microsecond tiebreaker inside a timestamp) is
  dropped; only the microsecond wallclock is written.
- The `properties_json` **overflow** column preserves bytes / arrays / sparse vectors /
  type-conflicting keys losslessly, but the default importer does not auto-expand it —
  custom decoding is required to re-import those values.
- **History enumeration** uses the whole-window changefeed, so deleted/retracted
  entities *are* exported with their full version history and tombstone. A deleted
  **edge**'s endpoints are not carried on a historical version, so `source_key` /
  `target_key` are written null for an edge no longer in current state.

---

## Reading the files: DuckDB and pandas

The files are ordinary Parquet — no AletheiaDB dependency needed to read them.

**pandas / pyarrow:**

```python
import pandas as pd
nodes = pd.read_parquet("nodes.parquet")
history = pd.read_parquet("node_history.parquet")
```

**DuckDB** — a worked bi-temporal query over an exported history file, *"how many facts
of each label were valid on 2024-01-01?"*:

```sql
SELECT label, count(*) AS facts_valid
FROM 'node_history.parquet'
WHERE valid_from <= TIMESTAMP '2024-01-01 00:00:00+00'
  AND (valid_to IS NULL OR valid_to > TIMESTAMP '2024-01-01 00:00:00+00')
GROUP BY label
ORDER BY facts_valid DESC;
```

Because open intervals are `NULL` (not a sentinel), the `valid_to IS NULL OR valid_to >
...` predicate reads naturally as "still valid, or closed after the probe instant".

---

## Round-trip

Exporting the current state and re-importing into a fresh database reproduces an
equivalent graph — same node/edge counts and equal property values, including dense
embeddings by exact bits. Point the import's `valid_time_column` at the exported
`valid_time` column to preserve valid-time so `AS OF VALID_TIME` answers match. See the
export tests in [`src/api/export/tests.rs`](../../src/api/export/tests.rs) for the
verified round-trip invariants.
