//! The evaluation pipeline (Issue #3366).
//!
//! `load dataset → index into an in-memory AletheiaDB → run a fixed query set
//! under a declarative retrieval config → score against gold labels`.
//!
//! Everything here is deterministic given `(dataset, config)`: the embeddings
//! are seeded (see [`crate::embedding`]), retrieval ties are broken by node id,
//! and the metrics are pure (see [`crate::metrics`]). Two runs with the same
//! inputs therefore produce byte-identical [`RunMetrics`].

use std::collections::{BTreeMap, BTreeSet};

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::{
    AletheiaDB, NodeId, PropertyMapBuilder, Provenance, SimilarityQuery, Timestamp, WriteOps, time,
};

use crate::config::RetrievalConfig;
use crate::dataset::{Dataset, ScalarValue};
use crate::embedding::{DEFAULT_DIM, embed};
use crate::metrics;
use crate::timeutil::parse_anchor;

/// The embedding property indexed for vector retrieval.
const EMBEDDING_PROPERTY: &str = "embedding";

/// A dataset indexed into a live database, with key↔node maps.
pub struct IndexedGraph {
    key_to_node: BTreeMap<String, NodeId>,
    node_to_key: BTreeMap<u64, String>,
}

impl IndexedGraph {
    /// The node id for an entity key, if present.
    #[must_use]
    pub fn node(&self, key: &str) -> Option<NodeId> {
        self.key_to_node.get(key).copied()
    }

    /// The entity key for a node id, if present.
    #[must_use]
    pub fn key(&self, node: NodeId) -> Option<&str> {
        self.node_to_key.get(&node.as_u64()).map(String::as_str)
    }
}

/// Aggregate, dataset-level metrics for one retrieval config.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunMetrics {
    /// Mean precision@k over all questions.
    pub precision_at_k: f64,
    /// Mean recall@k over all questions.
    pub recall_at_k: f64,
    /// Mean grounding precision over all questions.
    pub grounding_precision: f64,
    /// Temporal accuracy over time-anchored questions (`null`-equivalent when
    /// there are none: reported as `0.0` with `num_temporal_questions == 0`).
    pub temporal_accuracy: f64,
    /// Citation validity over all citations produced.
    pub citation_validity: f64,
    /// Total questions scored.
    pub num_questions: usize,
    /// Number of time-anchored questions.
    pub num_temporal_questions: usize,
}

/// Per-question detail, retained in the report for auditability.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuestionResult {
    /// Question id.
    pub id: String,
    /// Ranked retrieved entity keys (post-filter).
    pub retrieved: Vec<String>,
    /// Gold evidence keys.
    pub gold_evidence: Vec<String>,
    /// precision@k for this question.
    pub precision_at_k: f64,
    /// recall@k for this question.
    pub recall_at_k: f64,
    /// grounding precision for this question.
    pub grounding_precision: f64,
    /// `Some(true/false)` for time-anchored questions, `None` otherwise.
    pub temporal_correct: Option<bool>,
    /// Predicted answer value (for anchored questions), if an answer node was
    /// retrieved.
    pub predicted_answer: Option<String>,
    /// Whether every citation for this question resolved to a version that
    /// actually supports the answer (for an answer-bearing question, one
    /// carrying `answer_property == gold_answer`; for a structural question, a
    /// real reconstructed gold-evidence version).
    pub citations_valid: bool,
}

/// The full result of scoring one config against a dataset.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RunResult {
    /// Aggregate metrics.
    pub metrics: RunMetrics,
    /// Per-question breakdown.
    pub per_question: Vec<QuestionResult>,
}

/// Errors from running the harness.
#[derive(Debug)]
pub enum HarnessError {
    /// A database operation failed.
    Db(String),
    /// A dataset time anchor failed to parse.
    Time(String),
    /// The dataset referenced an unknown entity key.
    UnknownEntity(String),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HarnessError::Db(m) => write!(f, "database error: {m}"),
            HarnessError::Time(m) => write!(f, "time anchor error: {m}"),
            HarnessError::UnknownEntity(k) => write!(f, "unknown entity key '{k}'"),
        }
    }
}

impl std::error::Error for HarnessError {}

fn db_err<E: std::fmt::Display>(e: E) -> HarnessError {
    HarnessError::Db(e.to_string())
}

/// Build the property map for an entity/update, attaching an embedding vector.
fn build_properties(
    scalars: &BTreeMap<String, ScalarValue>,
    embedding: Option<&[f32]>,
) -> aletheiadb::PropertyMap {
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in scalars {
        builder = match value {
            ScalarValue::Bool(b) => builder.insert(key, *b),
            ScalarValue::Int(i) => builder.insert(key, *i),
            ScalarValue::Float(f) => builder.insert(key, *f),
            ScalarValue::Str(s) => builder.insert(key, s.as_str()),
        };
    }
    if let Some(vec) = embedding {
        builder = builder.insert_vector(EMBEDDING_PROPERTY, vec);
    }
    builder.build()
}

fn provenance_for(source: Option<&str>) -> Result<Option<Provenance>, HarnessError> {
    match source {
        Some(s) => Provenance::builder()
            .source(s)
            .build()
            .map(Some)
            .map_err(db_err),
        None => Ok(None),
    }
}

/// Index a dataset into a fresh in-memory database, returning the live db and
/// the key↔node maps. The vector index is enabled before any node is created
/// so every entity is indexed on write.
pub fn index_dataset(
    dataset: &Dataset,
    seed: u64,
) -> Result<(AletheiaDB, IndexedGraph), HarnessError> {
    let db = AletheiaDB::new().map_err(db_err)?;

    db.vector_index(EMBEDDING_PROPERTY)
        .hnsw(HnswConfig::new(DEFAULT_DIM, DistanceMetric::Cosine))
        .enable()
        .map_err(db_err)?;

    let mut key_to_node = BTreeMap::new();
    let mut node_to_key = BTreeMap::new();

    // 1. Entities (with embeddings + provenance + optional valid_from).
    for entity in &dataset.entities {
        let embedding = embed(&entity.text, DEFAULT_DIM, seed);
        let props = build_properties(&entity.properties, Some(&embedding));
        let mut opts = WriteRequestOptions::new();
        if let Some(vt) = &entity.valid_from {
            opts = opts.with_valid_from(parse_anchor(vt).map_err(HarnessError::Time)?);
        }
        if let Some(prov) = provenance_for(entity.source.as_deref())? {
            opts = opts.with_provenance(prov);
        }
        let label = entity.label.clone();
        let node_id = db
            .write(|tx| tx.create_node_with_options(&label, props.clone(), opts.clone()))
            .map_err(db_err)?;
        key_to_node.insert(entity.key.clone(), node_id);
        node_to_key.insert(node_id.as_u64(), entity.key.clone());

        // Optionally close this entity's valid interval right away (bounded
        // valid-time era, e.g. a past CEO tenure), so an AS OF query
        // reconstructs the single fact valid at the anchor.
        //
        // Loop-ordering invariant: a `retract_at` entity MUST be retracted here,
        // in the entity pass, BEFORE any edges are created (step 3). `retract_node`
        // refuses on a node with connected edges (the #3209/#3230 safe-by-default
        // contract), so retracting after wiring edges would error. Datasets that
        // need to retract an edge-connected node must model that edge's closure
        // via edge valid-time instead.
        if let Some(rt) = &entity.retract_at {
            let valid_to = parse_anchor(rt).map_err(HarnessError::Time)?;
            db.retract_node(node_id, valid_to).map_err(db_err)?;
        }
    }

    // 2. Point-in-time updates (PATCH; embedding is left untouched).
    for update in &dataset.updates {
        let node_id = *key_to_node
            .get(&update.entity)
            .ok_or_else(|| HarnessError::UnknownEntity(update.entity.clone()))?;
        let props = build_properties(&update.properties, None);
        let valid_from = parse_anchor(&update.valid_from).map_err(HarnessError::Time)?;
        let mut opts = WriteRequestOptions::new().with_valid_from(valid_from);
        if let Some(prov) = provenance_for(update.source.as_deref())? {
            opts = opts.with_provenance(prov);
        }
        db.write(|tx| tx.update_node_with_options(node_id, props.clone(), opts.clone()))
            .map_err(db_err)?;
    }

    // 3. Edges.
    for edge in &dataset.edges {
        let source = *key_to_node
            .get(&edge.source)
            .ok_or_else(|| HarnessError::UnknownEntity(edge.source.clone()))?;
        let target = *key_to_node
            .get(&edge.target)
            .ok_or_else(|| HarnessError::UnknownEntity(edge.target.clone()))?;
        let props = build_properties(&edge.properties, None);
        let valid_from = match &edge.valid_from {
            Some(vt) => Some(parse_anchor(vt).map_err(HarnessError::Time)?),
            None => None,
        };
        let label = edge.label.clone();
        db.write(|tx| {
            tx.create_edge_with_valid_time(source, target, &label, props.clone(), valid_from)
        })
        .map_err(db_err)?;
    }

    Ok((
        db,
        IndexedGraph {
            key_to_node,
            node_to_key,
        },
    ))
}

/// A retrieved candidate node (a vector hit or a traversal-reached node).
struct Candidate {
    node: NodeId,
}

/// Run retrieval + scoring for a whole dataset under one config.
pub fn run(
    db: &AletheiaDB,
    graph: &IndexedGraph,
    dataset: &Dataset,
    config: &RetrievalConfig,
) -> Result<RunResult, HarnessError> {
    let tx_time = time::now();
    let mut per_question = Vec::with_capacity(dataset.questions.len());

    let mut precisions = Vec::new();
    let mut recalls = Vec::new();
    let mut groundings = Vec::new();
    let mut temporal_flags = Vec::new();
    let mut citation_flags = Vec::new();

    for question in &dataset.questions {
        let anchor = match &question.valid_time {
            Some(vt) => Some(parse_anchor(vt).map_err(HarnessError::Time)?),
            None => None,
        };

        // --- Vector retrieval (current-state candidates) ---
        let query_embedding = embed(&question.text, DEFAULT_DIM, config.seed);
        let mut raw = db
            .similarity_search(SimilarityQuery::from_embedding(query_embedding).k(config.k.max(1)))
            .map_err(db_err)?;
        // Deterministic order: score desc, then node id asc for ties.
        raw.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.as_u64().cmp(&b.0.as_u64()))
        });
        let mut ranked: Vec<Candidate> = raw
            .into_iter()
            .take(config.k)
            .map(|(node, _score)| Candidate { node })
            .collect();

        // --- Hybrid graph expansion ---
        // The 2-hop gold evidence is unreachable by similarity, so put the
        // traversal-reached nodes (seed first, then its neighbours) at the
        // FRONT of the ranked list, ahead of the remaining vector hits. This
        // lands the evidence inside the top-k budget the @k metrics score.
        if config.hybrid {
            let seed = question
                .seed_entity
                .as_deref()
                .and_then(|k| graph.node(k))
                .or_else(|| ranked.first().map(|c| c.node));
            if let Some(seed) = seed {
                let reached = traverse(db, seed, config.max_hops, anchor, tx_time)?;
                let mut reordered: Vec<Candidate> = Vec::new();
                let mut seen: BTreeSet<u64> = BTreeSet::new();
                let push = |node: NodeId, out: &mut Vec<Candidate>, seen: &mut BTreeSet<u64>| {
                    if seen.insert(node.as_u64()) {
                        out.push(Candidate { node });
                    }
                };
                push(seed, &mut reordered, &mut seen);
                for node in reached {
                    push(node, &mut reordered, &mut seen);
                }
                for c in ranked.drain(..) {
                    push(c.node, &mut reordered, &mut seen);
                }
                ranked = reordered;
            }
        }

        // --- Provenance filter ---
        if config.provenance_filter {
            let trusted: BTreeSet<&str> =
                config.trusted_sources.iter().map(String::as_str).collect();
            ranked.retain(|c| {
                let source = db
                    .get_node_provenance(c.node)
                    .ok()
                    .flatten()
                    .and_then(|p| p.source().map(str::to_string));
                match source {
                    Some(s) => trusted.contains(s.as_str()),
                    None => false,
                }
            });
        }

        let retrieved_keys: Vec<String> = ranked
            .iter()
            .filter_map(|c| graph.key(c.node).map(str::to_string))
            .collect();

        let gold: BTreeSet<String> = question.gold_evidence.iter().cloned().collect();

        let precision = metrics::precision_at_k(&retrieved_keys, &gold, config.k);
        let recall = metrics::recall_at_k(&retrieved_keys, &gold, config.k);
        let grounding = metrics::grounding_precision(&retrieved_keys, &gold);
        precisions.push(precision);
        recalls.push(recall);
        groundings.push(grounding);

        // --- Temporal accuracy (anchored questions only) ---
        // The answer-bearing fact is resolved by a point-in-time find on the
        // fact label as of the anchor (full) or current state (baseline). The
        // ONLY difference between the two configs is the valid-time coordinate
        // used, so temporal accuracy isolates the value of temporal anchoring:
        // a fact that changed over valid time is answered correctly by the
        // anchored query and wrongly by the current-state one.
        let (temporal_correct, predicted_answer, answer_fact_node) =
            if let (Some(anchor), Some(prop), Some(expected)) = (
                anchor,
                question.answer_property.as_deref(),
                question.gold_answer.as_deref(),
            ) {
                let valid_coord = if config.temporal_anchoring {
                    anchor
                } else {
                    tx_time
                };
                let (predicted, fact_node) =
                    resolve_temporal_answer(db, question, prop, valid_coord, tx_time);
                let correct = predicted.as_deref() == Some(expected);
                temporal_flags.push(correct);
                (Some(correct), predicted, fact_node)
            } else {
                (None, None, None)
            };

        // --- Citation validity ---
        // A citation is the fact a caller would point at to justify the answer.
        // We validate that the cited version, reconstructed at the citation
        // coordinate, actually SUPPORTS the answer -- not merely that it
        // reconstructs to *some* version (which made the old check
        // near-tautological, a constant 1.0 that could never fail).
        //
        // For an answer-bearing (temporal) question the citation is the answer
        // fact node we resolved; it is valid iff that reconstructed version
        // carries `answer_property == gold_answer`. A citation that resolves to
        // a version NOT supporting the answer -- the current-state tenure for a
        // past-era question under a non-anchoring config, or a retracted-away
        // era -- scores INVALID, which is what gives the metric discriminating
        // power. For a structural question with no gold answer, the citations
        // are the retrieved gold-evidence nodes, valid iff each reconstructs to
        // a real version at the coordinate.
        let coord = anchor.filter(|_| config.temporal_anchoring);
        let mut citations_valid = true;
        match (
            question.answer_property.as_deref(),
            question.gold_answer.as_deref(),
        ) {
            (Some(prop), Some(expected)) => {
                // Cite the answer fact node (if one was resolved). No fact node
                // means no citation was made -- nothing to validate.
                if let Some(node) = answer_fact_node {
                    let supports =
                        citation_supports_answer(db, node, coord, tx_time, prop, expected);
                    citation_flags.push(supports);
                    citations_valid &= supports;
                }
            }
            _ => {
                // Structural question: cite surfaced gold evidence, validated by
                // point-in-time reconstruction. Restricting to surfaced gold
                // evidence keeps the metric about "do the citations we make point
                // at real facts", not penalising unrelated candidates.
                for c in &ranked {
                    let key = graph.key(c.node);
                    let is_gold = key.map(|k| gold.contains(k)).unwrap_or(false);
                    if !is_gold {
                        continue;
                    }
                    let resolves = match coord {
                        Some(vt) => db.get_node_at_time(c.node, vt, tx_time).is_ok(),
                        None => db.get_node(c.node).is_ok(),
                    };
                    citation_flags.push(resolves);
                    citations_valid &= resolves;
                }
            }
        }

        per_question.push(QuestionResult {
            id: question.id.clone(),
            retrieved: retrieved_keys,
            gold_evidence: question.gold_evidence.clone(),
            precision_at_k: precision,
            recall_at_k: recall,
            grounding_precision: grounding,
            temporal_correct,
            predicted_answer,
            citations_valid,
        });
    }

    let metrics = RunMetrics {
        precision_at_k: metrics::mean(&precisions),
        recall_at_k: metrics::mean(&recalls),
        grounding_precision: metrics::mean(&groundings),
        temporal_accuracy: metrics::temporal_accuracy(&temporal_flags),
        citation_validity: metrics::citation_validity(&citation_flags),
        num_questions: dataset.questions.len(),
        num_temporal_questions: dataset.num_temporal_questions(),
    };

    Ok(RunResult {
        metrics,
        per_question,
    })
}

/// Resolve a temporal question's answer via a point-in-time find on the fact
/// label as of `valid_coord` (the anchor for the full config, or the current
/// time for the baseline). The one fact valid at that coordinate supplies the
/// answer property. A fact that changed over valid time is answered correctly
/// only when `valid_coord` is the anchor — which is exactly the signal the
/// temporal-accuracy metric captures.
///
/// Returns both the predicted answer string and the [`NodeId`] of the fact node
/// it was read from, so the citation-validity check can re-reconstruct that same
/// node at the citation coordinate and confirm it supports the answer.
fn resolve_temporal_answer(
    db: &AletheiaDB,
    question: &crate::dataset::Question,
    property: &str,
    valid_coord: Timestamp,
    tx_time: Timestamp,
) -> (Option<String>, Option<NodeId>) {
    let Some(label) = question.answer_label.as_deref() else {
        return (None, None);
    };
    let (Some(key), Some(value)) = (
        question.answer_filter_key.as_deref(),
        question.answer_filter_value.as_deref(),
    ) else {
        return (None, None);
    };
    let filter = aletheiadb::PropertyValue::string(value);
    let Ok(found) = db.find_nodes_by_property_at(label, key, &filter, valid_coord, tx_time) else {
        return (None, None);
    };
    // At most one fact is valid at any coordinate (eras are disjoint), so the
    // first (lowest-id) match is the answer.
    found
        .nodes
        .iter()
        .find_map(|n| {
            n.get_property(property)
                .map(|v| (property_value_to_string(v), n.id))
        })
        .map_or((None, None), |(answer, node)| (Some(answer), Some(node)))
}

/// Whether a citation to `node` at `coord` SUPPORTS the answer: the node
/// reconstructs to a real version at that bi-temporal coordinate AND that
/// version carries `answer_property == gold_answer`.
///
/// This is the discriminating core of citation validity. A citation that
/// resolves to *some* version but one that does NOT carry the gold answer — the
/// current-state tenure recalled for a past-era question, or a version whose
/// valid interval was retracted away before `coord` (so reconstruction fails) —
/// scores `false`. Contrast the old check, which only asked whether the node
/// reconstructed to any version at all and so was a constant `true`.
fn citation_supports_answer(
    db: &AletheiaDB,
    node: NodeId,
    coord: Option<Timestamp>,
    tx_time: Timestamp,
    answer_property: &str,
    gold_answer: &str,
) -> bool {
    let reconstructed = match coord {
        Some(vt) => db.get_node_at_time(node, vt, tx_time).ok(),
        None => db.get_node(node).ok(),
    };
    reconstructed
        .and_then(|n| {
            n.get_property(answer_property)
                .map(property_value_to_string)
        })
        .as_deref()
        == Some(gold_answer)
}

/// Breadth-first traversal from `seed` following outgoing edges up to
/// `max_hops`, returning reached nodes (excluding the seed) in deterministic
/// (sorted, hop-order) sequence. When `anchor` is set the point-in-time
/// adjacency is used.
fn traverse(
    db: &AletheiaDB,
    seed: NodeId,
    max_hops: usize,
    anchor: Option<Timestamp>,
    tx_time: Timestamp,
) -> Result<Vec<NodeId>, HarnessError> {
    let mut visited: BTreeSet<u64> = BTreeSet::new();
    visited.insert(seed.as_u64());
    let mut frontier = vec![seed];
    let mut reached = Vec::new();

    for _ in 0..max_hops {
        let mut next: BTreeSet<u64> = BTreeSet::new();
        for &node in &frontier {
            let edge_ids = match anchor {
                Some(vt) => db.get_outgoing_edges_at_time(node, vt, tx_time),
                None => db.get_outgoing_edges(node),
            };
            for edge_id in edge_ids {
                let target = match anchor {
                    Some(vt) => db
                        .get_edge_at_time(edge_id, vt, tx_time)
                        .ok()
                        .map(|e| e.target),
                    None => db.get_edge(edge_id).ok().map(|e| e.target),
                };
                if let Some(target) = target
                    && !visited.contains(&target.as_u64())
                {
                    next.insert(target.as_u64());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier.clear();
        for id in next {
            visited.insert(id);
            // `id` comes straight from a live adjacency edge, so it is always a
            // valid, in-range node id; surface any violation as an error rather
            // than panicking.
            let node = NodeId::new(id).map_err(|e| {
                HarnessError::Db(format!("invalid node id {id} from adjacency: {e}"))
            })?;
            reached.push(node);
            frontier.push(node);
        }
    }
    Ok(reached)
}

fn property_value_to_string(value: &aletheiadb::PropertyValue) -> String {
    use aletheiadb::PropertyValue as V;
    match value {
        V::String(s) => s.to_string(),
        V::Int(i) => i.to_string(),
        V::Float(f) => f.to_string(),
        V::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::Dataset;
    use std::path::Path;

    /// One company, two disjoint CEO eras closed by retraction:
    /// Alice [2015, 2020), Carol [2020, ∞). Used to prove citation validity
    /// discriminates a supporting version from a non-supporting one.
    fn indexed_two_era() -> (AletheiaDB, IndexedGraph) {
        let json = r#"
        {
          "version": "0.0.1-test",
          "name": "two_era",
          "license": "CC0-1.0",
          "entities": [
            {"key": "acme_t1", "label": "Tenure", "text": "leadership record",
             "properties": {"company": "Acme", "ceo": "Alice"},
             "source": "curated", "valid_from": "2015-01-01", "retract_at": "2020-01-01"},
            {"key": "acme_t2", "label": "Tenure", "text": "leadership record",
             "properties": {"company": "Acme", "ceo": "Carol"},
             "source": "curated", "valid_from": "2020-01-01"}
          ],
          "questions": []
        }
        "#;
        let ds = Dataset::from_json_str(json, Path::new("two_era.json")).unwrap();
        index_dataset(&ds, 42).unwrap()
    }

    #[test]
    fn citation_supports_answer_discriminates_valid_from_invalid() {
        let (db, graph) = indexed_two_era();
        let t1 = graph.node("acme_t1").expect("t1 indexed");
        let now = time::now();
        let y2017 = parse_anchor("2017-06-01").unwrap();
        let y2021 = parse_anchor("2021-06-01").unwrap();

        // VALID: at 2017 the t1 version carries ceo=Alice → supports "Alice".
        assert!(
            citation_supports_answer(&db, t1, Some(y2017), now, "ceo", "Alice"),
            "citation anchored inside t1's era must support its CEO"
        );

        // INVALID (value != gold): same node/coordinate, different expected answer.
        assert!(
            !citation_supports_answer(&db, t1, Some(y2017), now, "ceo", "Carol"),
            "a citation whose reconstructed value != gold answer must be invalid"
        );

        // INVALID (retracted-away era): at 2021 t1's valid interval is closed, so
        // reconstruction finds no supporting version.
        assert!(
            !citation_supports_answer(&db, t1, Some(y2021), now, "ceo", "Alice"),
            "a citation to a retracted-away version must be invalid"
        );

        // INVALID (non-existent node): a node id that was never created cannot
        // reconstruct to any version.
        let ghost = NodeId::new(9_999_999).expect("in-range id");
        assert!(
            !citation_supports_answer(&db, ghost, Some(y2017), now, "ceo", "Alice"),
            "a citation to a non-existent node must be invalid"
        );
    }
}
