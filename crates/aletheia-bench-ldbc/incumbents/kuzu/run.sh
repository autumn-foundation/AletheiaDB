#!/usr/bin/env bash
# ============================================================================
# kuzu/run.sh — comparative driver for KuzuDB (Issue #3628 / #3373).
#
# NOT executed in the sandbox that produced the committed AletheiaDB results.
# Run this ON THE SELF-HOSTED BOX with the kuzu container up. It executes the
# mapped queries (see kuzu/queries.cypher) against the running container by
# piping Cypher into the `kuzu` CLI on the container's database, times warmup +
# N measured iterations each, computes p50/p95/p99 (nearest-rank, matching the
# AletheiaDB harness), and writes results/kuzu_results.json via emit_results.py.
#
# It SYNTHESIZES NO NUMBERS: every latency is a real timed `kuzu` invocation
# against loaded data. A stopped container, missing tooling, or an empty graph
# is a HARD ERROR. Bi-temporal reconstruction and single-query graph+vector
# hybrid are recorded as {not_expressible:true} capability facts (README
# footnotes 1 & 5), never timed.
#
# REQUIRED TOOLING (on the box):
#   * docker + docker compose v2  (the kuzu service in ../docker-compose.yml)
#   * bash 5+                     ($EPOCHREALTIME µs timing; date fallback)
#   * python3                    (percentile math / JSON emit)
#   The `kuzu` CLI runs INSIDE the container.
#
# DATA: KuzuDB uses a typed schema. Create the node/rel tables (Person, KNOWS,
# Forum, CONTAINER_OF, Post, HAS_CREATOR, HAS_TAG, Tag, Comment, REPLY_OF) and
# COPY the shared graph in beforehand (see README data-interchange). This driver
# does NOT bulk-load; it verifies the graph is non-empty and fails loudly.
#
# ENV (exported by run_all.sh; overridable):
#   ITERATIONS (default 200)  WARMUP (default 20)  KUZU_SERVICE (default kuzu)
#   KUZU_DB (default /bench/db.kuzu — the on-container database path)
# ============================================================================
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib.sh"

ITERATIONS="${ITERATIONS:-200}"
WARMUP="${WARMUP:-20}"
KUZU_SERVICE="${KUZU_SERVICE:-kuzu}"
KUZU_DB="${KUZU_DB:-/bench/db.kuzu}"

require_cmd docker
require_cmd python3
require_container "${KUZU_SERVICE}"

COMPOSE=(docker compose -f "${INCUMBENTS_DIR}/docker-compose.yml")

# Run one Cypher statement through the kuzu CLI (statement read from stdin).
# Non-zero exit propagates so a failed query is never silently timed.
kuzu_cypher() {
    printf '%s\n' "$1" | "${COMPOSE[@]}" exec -T "${KUZU_SERVICE}" kuzu "${KUZU_DB}"
}

# Best-effort scalar extraction: last non-empty, non-separator output line.
kuzu_scalar() {
    kuzu_cypher "$1" 2>/dev/null \
        | grep -vE '^\||^-+$|^$|Query executed|tuples|Time' \
        | tail -n 1 | tr -d '|" \r'
}

echo "[kuzu] verifying the graph is loaded..."
person_count="$(kuzu_scalar 'MATCH (p:Person) RETURN count(*);')"
[ -n "${person_count}" ] && [ "${person_count}" -gt 0 ] 2>/dev/null \
    || die "KuzuDB has no Person rows (db=${KUZU_DB}). Load the shared graph first (see README.md). Refusing to emit numbers over an empty DB."
echo "[kuzu] Person count = ${person_count}"

PERSON_ID="${PERSON_ID:-$(kuzu_scalar 'MATCH (p:Person) RETURN p.id ORDER BY p.id LIMIT 1;')}"
POST_ID="$(kuzu_scalar 'MATCH (post:Post) RETURN post.id ORDER BY post.id LIMIT 1;')"
[ -n "${PERSON_ID}" ] || die "could not sample a Person id from KuzuDB"
echo "[kuzu] sampled personId=${PERSON_ID} postId=${POST_ID:-<none>}"

SAMPLE_DIR="$(mktemp -d)"
trap 'rm -rf "${SAMPLE_DIR}"' EXIT

measure() {
    local key="$1" desc="$2" stmt="$3" i
    printf '%s' "${desc}" > "${SAMPLE_DIR}/${key}.desc"
    for ((i = 0; i < WARMUP; i++)); do
        kuzu_cypher "${stmt}" >/dev/null 2>&1 || die "[kuzu] warmup query failed for ${key}"
    done
    : > "${SAMPLE_DIR}/${key}.samples"
    for ((i = 0; i < ITERATIONS; i++)); do
        time_once_us kuzu_cypher "${stmt}" >> "${SAMPLE_DIR}/${key}.samples"
    done
    echo "[kuzu] measured ${key} (${ITERATIONS} iters)"
}

not_expressible() {
    local key="$1" reason="$2"
    printf '%s' "${reason}" > "${SAMPLE_DIR}/${key}.notexpressible"
    echo "[kuzu] ${key}: not expressible (${reason})"
}

# ---- SNB short reads (IS) ---------------------------------------------------
measure is1_person_profile "IS1 person profile" \
    "MATCH (p:Person) WHERE p.id = ${PERSON_ID} RETURN p;"
measure is2_person_recent_messages "IS2 messages authored by a person" \
    "MATCH (p:Person)<-[:HAS_CREATOR]-(m) WHERE p.id = ${PERSON_ID} RETURN m;"
measure is3_person_friends "IS3 direct friends" \
    "MATCH (p:Person)-[:KNOWS]->(f:Person) WHERE p.id = ${PERSON_ID} RETURN f;"
if [ -n "${POST_ID}" ]; then
    measure is5_message_creator "IS5 creator of a post" \
        "MATCH (post:Post)-[:HAS_CREATOR]->(c:Person) WHERE post.id = ${POST_ID} RETURN c;"
    measure is6_forum_of_message "IS6 forum of a post" \
        "MATCH (post:Post)<-[:CONTAINER_OF]-(forum:Forum) WHERE post.id = ${POST_ID} RETURN forum;"
fi

# ---- SNB complex reads (IC) -------------------------------------------------
measure ic1_friends_within_2_hops "IC1 friends within 2 hops" \
    "MATCH (p:Person)-[:KNOWS*1..2]->(f:Person) WHERE p.id = ${PERSON_ID} AND f.id <> ${PERSON_ID} RETURN DISTINCT f;"
measure ic2_recent_messages_by_friends "IC2 messages by direct friends" \
    "MATCH (p:Person)-[:KNOWS]->(f:Person)<-[:HAS_CREATOR]-(m) WHERE p.id = ${PERSON_ID} RETURN m;"
measure ic9_messages_by_friends_of_friends "IC9 messages by friends-of-friends" \
    "MATCH (p:Person)-[:KNOWS*1..2]->(f:Person)<-[:HAS_CREATOR]-(m) WHERE p.id = ${PERSON_ID} AND f.id <> ${PERSON_ID} RETURN m;"

# ---- Temporal extension: NOT expressible on KuzuDB (README footnote 1) ------
not_expressible ext_temporal_reconstruction \
    "KuzuDB has no native bi-temporal reconstruction; app-side validity-interval modeling only."
not_expressible ext_temporal_as_of_traversal \
    "KuzuDB has no native AS OF traversal over valid/transaction time."

# ---- Vector extension -------------------------------------------------------
# k-NN is expressible via KuzuDB's vector extension when built + a query
# embedding is supplied; measure only then, else skip honestly. The single-query
# graph+vector hybrid is not expressible as one planned operation.
if [ -n "${KUZU_VECTOR_CALL:-}" ]; then
    measure ext_vector_knn "EXT-VECTOR k-NN (k=10) via KuzuDB vector extension" \
        "${KUZU_VECTOR_CALL}"
else
    echo "[kuzu] ext_vector_knn: skipped (set KUZU_VECTOR_CALL to the engine-specific k-NN call to measure); not recorded."
fi
not_expressible ext_vector_hybrid_graph_vector \
    "Single planned graph-traversal + ANN-rank operation is AletheiaDB-specific (README footnote 5)."

emit_results_json kuzu "kuzudb/kuzu:0.6.1" "${SAMPLE_DIR}"
echo "[kuzu] done."
