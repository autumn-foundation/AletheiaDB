# Retrieval Evaluation Harness (Issue #3366)

`aletheia-eval` is a deterministic, LLM-free harness that measures how much
AletheiaDB's **temporal**, **graph**, and **provenance** features improve
retrieval quality over a plain vector-only baseline. It exists so that a
feature's retrieval impact is a reproducible, gate-able number rather than a
claim.

- Crate: [`crates/aletheia-eval`](../../crates/aletheia-eval)
- Binary: `aletheia-eval run <manifest.toml>` (the shape the future
  `aletheia eval run` subcommand of the root CLI will take — this crate does not
  modify the root `aletheia` binary)

## Pipeline

```
load a versioned dataset (JSON)
  → index it into an in-memory AletheiaDB
  → run a fixed query set under a declarative retrieval config
  → score against gold labels
  → emit a JSON report + human summary
  → apply regression gates (exit non-zero on breach)
```

Every stage is deterministic given the reproducibility triple
`(dataset version, config, seed)`:

- **Embeddings** are a seeded [feature-hashing vectorizer](#deterministic-embeddings)
  — no external model, no API keys.
- **Retrieval** ties break by node id.
- **Metrics** are pure functions.

Two runs with the same inputs produce **byte-identical** metrics. This is
enforced by a determinism test (`tests/pipeline.rs`).

## Running it

```bash
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/temporal_qa/eval.toml
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/multihop_qa/eval.toml

# Write the machine report and gate CI on the exit code:
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/temporal_qa/eval.toml \
    --json report.json
echo $?   # 0 = all gates passed, 2 = a gate breached, 1 = error
```

Both bundled datasets run end-to-end in well under a second — comfortably inside
a 10-minute CI budget.

## Metrics

Let `R` be the ordered retrieved evidence keys for one question, `R_k` its first
`k`, and `G` the gold-evidence set. All aggregate (dataset-level) metrics are the
arithmetic mean of the per-question values. Formulas and boundary behaviour are
unit-tested in `crates/aletheia-eval/src/metrics.rs`.

| Metric | Formula | Boundary behaviour |
|---|---|---|
| **precision@k** | `|R_k ∩ G| / k` | `k == 0` → `0.0`; unfilled budget is penalised (denominator stays `k`) |
| **recall@k** | `|R_k ∩ G| / |G|` | empty gold → `1.0` (vacuously perfect) |
| **grounding precision** | `|R ∩ G| / |R|` | empty retrieval → `0.0`; no `k` cap — measures purity of what was grounded on |
| **temporal accuracy** | mean over time-anchored questions of `answer(anchor) == gold_answer` | no temporal questions → `0.0` |
| **citation validity** | fraction of returned citations that resolve to a real supporting version | no citations → `1.0` |

Duplicate retrieved keys are credited once (a retriever can't inflate a score by
repeating a relevant item). A citation to a non-existent version scores invalid.

## Configuration format

Two file-based layers, both TOML, both `#[serde(deny_unknown_fields)]` (a
misspelled or missing required key is a hard, clearly-worded error).

### `RetrievalConfig` (`full.toml` / `baseline.toml`)

```toml
k = 5                          # top-k cutoff for vector retrieval and @k metrics
hybrid = true                  # graph-traversal expansion of the vector seed(s)
temporal_anchoring = true      # resolve facts AS OF each question's time anchor
provenance_filter = true       # drop retrieved items whose source ∉ trusted_sources
max_hops = 2                   # traversal depth when hybrid is on (default 2)
trusted_sources = ["curated"]  # sources kept when provenance_filter is on
seed = 42                      # embedding vectorizer seed
```

The three feature toggles (`hybrid`, `temporal_anchoring`, `provenance_filter`)
are **required with no default**, so an incomplete config fails loudly. The
**baseline** flips all three off — vector-only k-NN, the pgvector-equivalent. A
feature's eval impact is therefore a one-line diff between `full.toml` and
`baseline.toml`.

### `EvalConfig` manifest (`eval.toml`)

```toml
dataset = "dataset.json"       # relative to the manifest's directory
full = "full.toml"
baseline = "baseline.toml"

[gates]                        # all optional; a breach exits non-zero
min_temporal_accuracy_delta = 0.25   # full − baseline temporal accuracy
min_full_temporal_accuracy = 0.99
min_recall_delta = 0.5               # full − baseline recall@k
min_full_citation_validity = 1.0
```

Every report is a **paired** full-vs-baseline comparison with per-metric deltas.

## Dataset format

A dataset is committed JSON describing a small bi-temporal graph plus gold
questions. Full schema: `crates/aletheia-eval/src/dataset.rs`.

```json
{
  "version": "1.0.0",
  "name": "temporal_qa",
  "license": "CC0-1.0",
  "description": "...",
  "entities": [
    { "key": "acme", "label": "Company", "text": "Acme semiconductor foundry",
      "properties": { "name": "Acme" }, "source": "curated",
      "valid_from": "2015-01-01" },
    { "key": "acme_t1", "label": "Tenure", "text": "executive leadership tenure record",
      "properties": { "company": "Acme", "ceo": "Alice" },
      "source": "curated", "valid_from": "2015-01-01", "retract_at": "2019-01-01" }
  ],
  "updates": [],
  "edges": [
    { "source": "alice", "target": "acme", "label": "WORKS_AT" }
  ],
  "questions": [
    { "id": "acme_2016", "text": "Who was the boss of Acme in 2016",
      "gold_evidence": ["acme"], "valid_time": "2016-06-01",
      "answer_label": "Tenure", "answer_filter_key": "company",
      "answer_filter_value": "Acme", "answer_property": "ceo",
      "gold_answer": "Alice" }
  ]
}
```

- **`entities[].text`** is embedded (deterministically) into the node's vector.
- **`entities[].source`** is recorded as write-time provenance and used by the
  provenance filter.
- **`entities[].valid_from` / `retract_at`** bound a fact's valid-time interval.
  `retract_at` closes the interval with a retraction (Issue #3230), so a
  point-in-time query reconstructs the single fact valid at an anchor.
- **`questions[].valid_time`** is the time anchor (RFC 3339 or bare
  `YYYY-MM-DD`).
- **`questions[].answer_label`/`answer_filter_key`/`answer_filter_value`/`answer_property`**
  tell the harness how to resolve the answer fact (a point-in-time
  `find_nodes_by_property_at` as of the anchor for the full config, or current
  state for the baseline).
- **`questions[].seed_entity`** is the traversal entry point for hybrid retrieval.

### Bundled datasets

Both are **CC0-1.0** synthetic data authored for this harness — no real persons
or organizations. Each ships `dataset.json`, `manifest.json`
(size/format/license/version), `full.toml`, `baseline.toml`, and `eval.toml`.

**`temporal_qa`** (15 questions, all time-anchored) — five fictional companies,
each with three CEO tenures across three valid-time eras (2015–2018, 2019–2022,
2023+). Each tenure is a fact node whose valid interval is closed by retraction
at the era boundary. Answering a question anchored to a past era requires
reconstructing the CEO valid at that anchor; a retriever that ignores the anchor
and reads current state returns the present CEO and is wrong for every past era.

**`multihop_qa`** (8 questions, 2-hop gold evidence) — each question names a
person and asks about the country of the company they work for; the gold
evidence (company and country) is two hops away along `WORKS_AT` then
`HEADQUARTERED_IN`. Vector similarity alone retrieves only the named person, so
answering requires graph traversal. Two untrusted "rumor"-sourced distractors
exercise the provenance filter.

## Reference results

Seed 42, k 5. Reproduce from a fresh clone with the run commands above.

### temporal_qa

| Metric | full | baseline | delta |
|---|---:|---:|---:|
| precision@k | 0.200 | 0.200 | +0.000 |
| recall@k | 1.000 | 1.000 | +0.000 |
| grounding_precision | 0.200 | 0.200 | +0.000 |
| **temporal_accuracy** | **1.000** | **0.333** | **+0.667** |
| citation_validity | 1.000 | 1.000 | +0.000 |

**Headline substantiated:** temporally-anchored retrieval scores **+0.667
(66.7 percentage points)** higher temporal accuracy than the vector-only
baseline — well past the 25pp bar.

### multihop_qa

| Metric | full | baseline | delta |
|---|---:|---:|---:|
| precision@k | 0.400 | 0.025 | +0.375 |
| **recall@k** | **1.000** | **0.062** | **+0.938** |
| grounding_precision | 0.304 | 0.025 | +0.279 |
| citation_validity | 1.000 | 1.000 | +0.000 |

## JSON report schema (version `1`)

```jsonc
{
  "schema_version": "1",
  "dataset": { "name", "version", "license", "num_questions" },
  "full":     { "config": {…}, "metrics": {…}, "per_question": [ {…} ] },
  "baseline": { "config": {…}, "metrics": {…}, "per_question": [ {…} ] },
  "deltas":   { "precision_at_k", "recall_at_k", "grounding_precision",
                "temporal_accuracy", "citation_validity" },
  "gates":        [ { "name", "threshold", "observed", "passed" } ],
  "gates_passed": true
}
```

`metrics` = `{ precision_at_k, recall_at_k, grounding_precision,
temporal_accuracy, citation_validity, num_questions, num_temporal_questions }`.
Each `per_question` entry = `{ id, retrieved, gold_evidence, precision_at_k,
recall_at_k, grounding_precision, temporal_correct, predicted_answer,
citations_valid }`.

## Deterministic embeddings

Instead of a learned model, the harness uses a **feature-hashing vectorizer** —
a documented, seeded, pure function from text to a fixed-dimension (256) unit
vector:

1. Lowercase and split the text into alphanumeric tokens.
2. Hash each `"{seed}:{token}"` with FNV-1a (a fixed, non-randomised hash — not
   the standard library's process-seeded hasher).
3. Map the hash to a bucket (`hash % dim`) and a sign (top bit), accumulate.
4. L2-normalise.

Cosine similarity then grows with shared tokens, so a question naming an entity
lands nearest that entity's node. Implementation: `src/embedding.rs`.

## LLM-optional answer-quality mode (documented, not implemented)

The core harness scores **retrieval** with no LLM. An optional answer-quality
mode would grade a *generated* answer against the gold answer using an LLM judge.
It is intentionally **not implemented** here; the design is:

- Add an optional `answer-quality` cargo feature and an `AnswerScorer` trait
  (`grade(question, retrieved_context, gold_answer) -> f64`).
- Provide a `NullScorer` (default; the deterministic core) and an
  `LlmJudgeScorer` behind the feature, reading an API key from the environment.
- Emit an extra optional `answer_quality` metric block, leaving every existing
  deterministic metric byte-identical when the feature is off.

Keeping it out of the deterministic core preserves reproducibility and the
"no API keys" guarantee for the metrics this harness gates on.

## Reproduce from a fresh clone

```bash
git clone <repo> && cd AletheiaDB
git checkout <branch>
cargo test -p aletheia-eval
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/temporal_qa/eval.toml
cargo run -p aletheia-eval -- run crates/aletheia-eval/datasets/multihop_qa/eval.toml
```
