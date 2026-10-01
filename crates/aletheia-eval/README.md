# aletheia-eval — retrieval evaluation harness + gold datasets (Issue #3366)

A deterministic, LLM-free harness that measures how much AletheiaDB's
**temporal**, **graph**, and **provenance** features improve retrieval quality
over a plain vector-only baseline — turning "our temporal features help RAG"
from a claim into a reproducible, gate-able number.

```
load a versioned dataset
  → index it into an in-memory AletheiaDB
  → run a fixed query set under a declarative retrieval config
  → score against gold labels
  → emit a JSON report + human summary (+ regression gates)
```

Everything is deterministic given `(dataset version, config, seed)`: embeddings
are a seeded feature-hashing vectorizer (no external model, no API keys),
retrieval ties break by node id, and the metrics are pure functions. Two runs
with the same inputs produce **byte-identical** metrics.

## Quick start

```bash
# From the repo root. (Isolate the build target dir if you're protecting disk.)
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/temporal_qa/eval.toml
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/multihop_qa/eval.toml

# Machine-readable report + exit non-zero on a gate breach (for CI):
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/temporal_qa/eval.toml \
    --json report.json
```

> The binary is `aletheia-eval run <manifest.toml>`. This is the shape the
> future `aletheia eval run <manifest.toml>` subcommand of the root CLI will
> take; this crate does **not** modify the root `aletheia` binary.

## Reference results

Measured on the bundled datasets (seed 42, k 5), full config vs. the vector-only
baseline. Reproduce from a fresh clone with the two commands above.

### temporal_qa (15 questions, all time-anchored)

| Metric | full | baseline | delta |
|---|---:|---:|---:|
| precision@k | 0.200 | 0.200 | +0.000 |
| recall@k | 1.000 | 1.000 | +0.000 |
| grounding_precision | 0.200 | 0.200 | +0.000 |
| **temporal_accuracy** | **1.000** | **0.333** | **+0.667** |
| citation_validity | 1.000 | 1.000 | +0.000 |

**Headline:** temporally-anchored retrieval scores **+0.667 (66.7 percentage
points)** higher temporal accuracy than the vector-only baseline — far past the
25pp bar. The signal is real, not gamed: the same database reconstructs a
different CEO at each valid-time era; the baseline, reading current state,
returns the present CEO and is wrong for every past era.

### multihop_qa (8 questions, 2-hop gold evidence)

| Metric | full | baseline | delta |
|---|---:|---:|---:|
| precision@k | 0.400 | 0.025 | +0.375 |
| **recall@k** | **1.000** | **0.062** | **+0.938** |
| grounding_precision | 0.304 | 0.025 | +0.279 |
| citation_validity | 1.000 | 1.000 | +0.000 |

The gold evidence sits two hops away (`WORKS_AT` then `HEADQUARTERED_IN`), which
vector similarity alone cannot reach; hybrid graph traversal recovers it.

Both datasets run end-to-end in well under a second (< 10-minute CI budget).

## Metrics

All deterministic; formulas and boundary behaviour are in
[`docs/guides/retrieval-eval.md`](../../docs/guides/retrieval-eval.md) and unit-tested in
`src/metrics.rs`.

- **precision@k** = `|retrieved_k ∩ gold| / k`
- **recall@k** = `|retrieved_k ∩ gold| / |gold|`
- **grounding precision** = `|retrieved ∩ gold| / |retrieved|`
- **temporal accuracy** = fraction of time-anchored questions answered from the
  fact valid at the anchor
- **citation validity** = fraction of returned citations that resolve to a real
  entity/version supporting the answer

## Configuration (paired, one-line diff)

Each dataset ships `full.toml` and `baseline.toml` — complete
`RetrievalConfig`s that differ only in the feature toggles — plus `eval.toml`, a
manifest pairing them with the dataset and regression gates.

```toml
# full.toml                    # baseline.toml
k = 5                          k = 5
hybrid = true                  hybrid = false
temporal_anchoring = true      temporal_anchoring = false
provenance_filter = true       provenance_filter = false
max_hops = 2                   max_hops = 2
trusted_sources = ["curated"]  trusted_sources = ["curated"]
seed = 42                      seed = 42
```

The baseline is vector-only k-NN with graph/temporal/provenance OFF — the
pgvector-equivalent. Every report is a paired full-vs-baseline comparison.

Unknown or missing required config fields are hard, clearly-worded errors
(`#[serde(deny_unknown_fields)]`), never silently ignored.

## Datasets

Bundled under `datasets/`. Each ships `dataset.json`, `manifest.json`
(size/format/license/version), `full.toml`, `baseline.toml`, and `eval.toml`.
Both are CC0-1.0 synthetic data authored for this harness — no real persons or
organizations. See each `manifest.json` and the guide for the schema.

## LLM-optional

The core scores **retrieval** with no LLM. An optional answer-quality mode
(grading a generated answer against the gold answer with an LLM judge) is
**documented but not implemented** — it would plug in as an extra per-question
scorer behind a feature flag, leaving the deterministic core untouched. See the
guide.

## Development

```bash
cargo test  -p aletheia-eval
cargo clippy -p aletheia-eval --all-targets -- -D warnings
cargo fmt --all
```
