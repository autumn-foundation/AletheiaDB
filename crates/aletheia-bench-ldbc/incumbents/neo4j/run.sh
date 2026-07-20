#!/usr/bin/env bash
# ============================================================================
# neo4j/run.sh — comparative driver for Neo4j (Issue #3628 / #3373).
#
# NOT executed in the sandbox that produced the committed AletheiaDB results
# (no Docker there). Run this ON THE SELF-HOSTED BOX with the Neo4j container up.
# It executes the mapped queries (see neo4j/queries.cypher) against the running
# container via `cypher-shell`, times warmup + N measured iterations each,
# computes p50/p95/p99 (nearest-rank, matching the AletheiaDB harness), and
# writes results/neo4j_results.json via emit_results.py.
#
# It SYNTHESIZES NO NUMBERS: every latency is a real timed `cypher-shell`
# invocation against loaded data. A stopped container, missing tooling, or an
# empty graph is a HARD ERROR (we never fabricate a result). Bi-temporal
# reconstruction and single-query graph+vector hybrid are recorded as
# {not_expressible:true} capability facts (README footnotes 1 & 5), never timed.
#
# REQUIRED TOOLING (on the box):
#   * docker + docker compose v2  (the neo4j service in ../docker-compose.yml)
#   * bash 5+                     ($EPOCHREALTIME µs timing; date fallback)
#   * python3                    (percentile math / JSON emit)
#   cypher-shell runs INSIDE the container, so no host install is needed.
#
# DATA: provide the graph before running. Either
#   (a) --datagen-dir already loaded into Neo4j (see README data-interchange), or
#   (b) a Neo4j DB already populated with the logical schema in README.md.
# This driver does NOT bulk-load for you (ETL is engine-and-dataset specific);
# it verifies the graph is non-empty and fails loudly otherwise.
#
# ENV (exported by run_all.sh; overridable):
#   ITERATIONS (default 200)  WARMUP (default 20)  NEO4J_SERVICE (default neo4j)
#   NEO4J_USER (default neo4j)  NEO4J_PASSWORD (default benchbench)
# ============================================================================
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib.sh"

ITERATIONS="${ITERATIONS:-200}"
WARMUP="${WARMUP:-20}"
NEO4J_SERVICE="${NEO4J_SERVICE:-neo4j}"
NEO4J_USER="${NEO4J_USER:-neo4j}"
NEO4J_PASSWORD="${NEO4J_PASSWORD:-benchbench}"

require_cmd docker
require_cmd python3
require_container "${NEO4J_SERVICE}"

COMPOSE=(docker compose -f "${INCUMBENTS_DIR}/docker-compose.yml")

# Run one Cypher statement inside the container. Non-zero exit propagates so a
# failed query is never silently timed as a fast one.
neo4j_cypher() {
    "${COMPOSE[@]}" exec -T "${NEO4J_SERVICE}" \
        cypher-shell -u "${NEO4J_USER}" -p "${NEO4J_PASSWORD}" --format plain "$1"
}

# Fetch a single scalar (first data cell) from a query; empty if no rows.
neo4j_scalar() {
    neo4j_cypher "$1" 2>/dev/null | sed -n '2p' | tr -d '"' | tr -d '\r'
}

echo "[neo4j] verifying the graph is loaded..."
person_count="$(neo4j_scalar 'MATCH (p:Person) RETURN count(p);')"
[ -n "${person_count}" ] && [ "${person_count}" -gt 0 ] 2>/dev/null \
    || die "Neo4j has no :Person nodes. Load the shared graph first (see README.md). Refusing to emit numbers over an empty DB."
echo "[neo4j] :Person count = ${person_count}"

# Sample real ids to parametrize the queries (must come from the loaded data).
PERSON_ID="${PERSON_ID:-$(neo4j_scalar 'MATCH (p:Person) RETURN p.id ORDER BY p.id LIMIT 1;')}"
POST_ID="$(neo4j_scalar 'MATCH (post:Post) RETURN post.id ORDER BY post.id LIMIT 1;')"
[ -n "${PERSON_ID}" ] || die "could not sample a Person id from Neo4j"
echo "[neo4j] sampled personId=${PERSON_ID} postId=${POST_ID:-<none>}"

SAMPLE_DIR="$(mktemp -d)"
trap 'rm -rf "${SAMPLE_DIR}"' EXIT

# Time a keyed query: WARMUP discarded, then ITERATIONS measured samples.
# Usage: measure <key> <description> <cypher-statement>
measure() {
    local key="$1" desc="$2" stmt="$3" i
    printf '%s' "${desc}" > "${SAMPLE_DIR}/${key}.desc"
    for ((i = 0; i < WARMUP; i++)); do
        neo4j_cypher "${stmt}" >/dev/null 2>&1 || die "[neo4j] warmup query failed for ${key}"
    done
    : > "${SAMPLE_DIR}/${key}.samples"
    for ((i = 0; i < ITERATIONS; i++)); do
        time_once_us neo4j_cypher "${stmt}" >> "${SAMPLE_DIR}/${key}.samples"
    done
    echo "[neo4j] measured ${key} (${ITERATIONS} iters)"
}

# Record a capability fact (never a fabricated latency).
not_expressible() {
    local key="$1" reason="$2"
    printf '%s' "${reason}" > "${SAMPLE_DIR}/${key}.notexpressible"
    echo "[neo4j] ${key}: not expressible (${reason})"
}

# ---- SNB short reads (IS) ---------------------------------------------------
measure is1_person_profile "IS1 person profile" \
    "MATCH (p:Person {id: ${PERSON_ID}}) RETURN p;"
measure is2_person_recent_messages "IS2 messages authored by a person" \
    "MATCH (p:Person {id: ${PERSON_ID}})<-[:HAS_CREATOR]-(m) RETURN m;"
measure is3_person_friends "IS3 direct friends" \
    "MATCH (p:Person {id: ${PERSON_ID}})-[:KNOWS]->(f:Person) RETURN f;"
if [ -n "${POST_ID}" ]; then
    measure is5_message_creator "IS5 creator of a post" \
        "MATCH (post:Post {id: ${POST_ID}})-[:HAS_CREATOR]->(c:Person) RETURN c;"
    measure is6_forum_of_message "IS6 forum of a post" \
        "MATCH (post:Post {id: ${POST_ID}})<-[:CONTAINER_OF]-(forum:Forum) RETURN forum;"
fi

# ---- SNB complex reads (IC) -------------------------------------------------
measure ic1_friends_within_2_hops "IC1 friends within 2 hops (node-distinct)" \
    "MATCH (p:Person {id: ${PERSON_ID}})-[:KNOWS*1..2]->(f:Person) WHERE f <> p RETURN DISTINCT f;"
measure ic2_recent_messages_by_friends "IC2 messages authored by direct friends" \
    "MATCH (p:Person {id: ${PERSON_ID}})-[:KNOWS]->(f:Person)<-[:HAS_CREATOR]-(m) RETURN m;"
measure ic9_messages_by_friends_of_friends "IC9 messages by friends-of-friends" \
    "MATCH (p:Person {id: ${PERSON_ID}})-[:KNOWS*1..2]->(f:Person)<-[:HAS_CREATOR]-(m) WHERE f <> p RETURN m;"

# ---- Temporal extension: NOT expressible on Neo4j (README footnote 1) -------
not_expressible ext_temporal_reconstruction \
    "Neo4j has no native bi-temporal (valid-time + transaction-time) reconstruction; would require modeling validity intervals as ordinary properties (a different, hand-built workload)."
not_expressible ext_temporal_as_of_traversal \
    "Neo4j has no native AS OF traversal over valid/transaction time; app-side interval modeling only."

# ---- Vector extension -------------------------------------------------------
# k-NN is expressible via a Neo4j 5.x vector index. Measure only if the index
# exists AND a query embedding is available; otherwise skip honestly (never
# fabricate). The single-query graph+vector hybrid is not expressible.
has_vec_index="$(neo4j_scalar "SHOW INDEXES YIELD name WHERE name = 'post_embedding' RETURN count(*);")"
if [ "${has_vec_index:-0}" -gt 0 ] 2>/dev/null && [ -n "${QUERY_EMBEDDING:-}" ]; then
    measure ext_vector_knn "EXT-VECTOR k-NN (k=10) via Neo4j vector index" \
        "CALL db.index.vector.queryNodes('post_embedding', 10, ${QUERY_EMBEDDING}) YIELD node, score RETURN node, score;"
else
    echo "[neo4j] ext_vector_knn: skipped (no 'post_embedding' vector index or no QUERY_EMBEDDING); not recorded (neither timed nor fabricated)."
fi
not_expressible ext_vector_hybrid_graph_vector \
    "Single planned graph-traversal + ANN-rank operation is AletheiaDB-specific; Neo4j needs two stitched queries (README footnote 5)."

emit_results_json neo4j "neo4j:5.23-community" "${SAMPLE_DIR}"
echo "[neo4j] done."
