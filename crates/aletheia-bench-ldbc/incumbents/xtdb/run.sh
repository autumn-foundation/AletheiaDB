#!/usr/bin/env bash
# ============================================================================
# xtdb/run.sh — comparative driver for XTDB (Issue #3628 / #3373).
#
# NOT executed in the sandbox that produced the committed AletheiaDB results.
# Run this ON THE SELF-HOSTED BOX with the xtdb container up. XTDB is a
# bi-temporal Datalog store (NOT a property graph), so this driver measures ONLY
# the TEMPORAL-EXTENSION axis — point-in-time reconstruction and AS OF traversal
# — by POSTing the mapped EDN queries (see xtdb/queries.edn) to the running
# container's HTTP API and timing warmup + N measured iterations each. It writes
# results/xtdb_results.json via emit_results.py.
#
# It SYNTHESIZES NO NUMBERS: every latency is a real timed HTTP query against
# loaded data. A stopped container, missing tooling, or an empty store is a HARD
# ERROR. The SNB property-graph reads (a different modeling exercise, README
# footnote 2) and the vector k-NN / hybrid (XTDB has no native ANN, footnotes 4
# & 5) are recorded as {not_expressible:true} capability facts, never timed.
#
# REQUIRED TOOLING (on the box):
#   * docker + docker compose v2  (the xtdb service in ../docker-compose.yml)
#   * bash 5+                     ($EPOCHREALTIME µs timing; date fallback)
#   * python3                    (percentile math / JSON emit)
#   * curl                       (HTTP query client, on the host)
#
# DATA: submit the shared graph as XTDB documents (with valid-time) beforehand
# (see README data-interchange). This driver does NOT load; it verifies the
# store is non-empty and fails loudly.
#
# ENV (exported by run_all.sh; overridable):
#   ITERATIONS (default 200)  WARMUP (default 20)
#   XTDB_URL (default http://localhost:3000)  XTDB_QUERY_PATH (default /_xtdb/query)
#   XTDB_VALID_TIME (default 2024-01-01T00:00:00Z)  XTDB_ID_ATTR (default :person/id)
# ============================================================================
set -euo pipefail

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/lib.sh"

ITERATIONS="${ITERATIONS:-200}"
WARMUP="${WARMUP:-20}"
XTDB_SERVICE="${XTDB_SERVICE:-xtdb}"
XTDB_URL="${XTDB_URL:-http://localhost:3000}"
XTDB_QUERY_PATH="${XTDB_QUERY_PATH:-/_xtdb/query}"
XTDB_VALID_TIME="${XTDB_VALID_TIME:-2024-01-01T00:00:00Z}"
XTDB_ID_ATTR="${XTDB_ID_ATTR:-:person/id}"

require_cmd docker
require_cmd python3
require_cmd curl
require_container "${XTDB_SERVICE}"

QUERY_ENDPOINT="${XTDB_URL}${XTDB_QUERY_PATH}"

# POST an EDN query to XTDB; body on stdout, non-zero exit on HTTP/transport
# failure so a failed query is never silently timed.
xtdb_query() {
    local edn="$1" code
    code="$(curl -s -o /tmp/xtdb_resp.$$ -w '%{http_code}' \
        -XPOST "${QUERY_ENDPOINT}" \
        -H 'Content-Type: application/edn' -H 'Accept: application/edn' \
        --data-binary "${edn}")" || return 1
    [ "${code}" = "200" ] || { cat "/tmp/xtdb_resp.$$" >&2; rm -f "/tmp/xtdb_resp.$$"; return 1; }
    cat "/tmp/xtdb_resp.$$"; rm -f "/tmp/xtdb_resp.$$"
}

echo "[xtdb] verifying the store is loaded and the HTTP API answers..."
count_edn="{:query {:find [(count e)] :where [[e ${XTDB_ID_ATTR} _]]}}"
count_out="$(xtdb_query "${count_edn}")" || die "XTDB HTTP query failed at ${QUERY_ENDPOINT}. Is the container up and the API on ${XTDB_URL}?"
echo "[xtdb] count(${XTDB_ID_ATTR}) response: ${count_out}"
echo "${count_out}" | grep -qE '\[[1-9]' \
    || die "XTDB store appears empty (no ${XTDB_ID_ATTR} entities). Load the shared graph first (see README.md). Refusing to emit numbers over an empty store."

# Sample a real entity id to parametrize the queries.
sample_edn="{:query {:find [id] :where [[e ${XTDB_ID_ATTR} id]] :limit 1}}"
PERSON_ID="${PERSON_ID:-$(xtdb_query "${sample_edn}" | tr -d '[]() ' | tr ',' '\n' | grep -m1 -E '.' || true)}"
[ -n "${PERSON_ID}" ] || die "could not sample an id from XTDB"
echo "[xtdb] sampled id=${PERSON_ID} valid-time=${XTDB_VALID_TIME}"

SAMPLE_DIR="$(mktemp -d)"
trap 'rm -rf "${SAMPLE_DIR}"' EXIT

measure() {
    local key="$1" desc="$2" edn="$3" i
    printf '%s' "${desc}" > "${SAMPLE_DIR}/${key}.desc"
    for ((i = 0; i < WARMUP; i++)); do
        xtdb_query "${edn}" >/dev/null 2>&1 || die "[xtdb] warmup query failed for ${key}"
    done
    : > "${SAMPLE_DIR}/${key}.samples"
    for ((i = 0; i < ITERATIONS; i++)); do
        time_once_us xtdb_query "${edn}" >> "${SAMPLE_DIR}/${key}.samples"
    done
    echo "[xtdb] measured ${key} (${ITERATIONS} iters)"
}

not_expressible() {
    local key="$1" reason="$2"
    printf '%s' "${reason}" > "${SAMPLE_DIR}/${key}.notexpressible"
    echo "[xtdb] ${key}: not expressible (${reason})"
}

# ---- Temporal extension: THIS is XTDB's native axis (bi-temporal) -----------
# Point-in-time reconstruction of an entity's state AS OF a historical valid time.
measure ext_temporal_reconstruction "EXT-TEMPORAL point-in-time reconstruction AS OF valid time" \
    "{:query {:find [(pull e [*])] :where [[e ${XTDB_ID_ATTR} ${PERSON_ID}]]} :valid-time #inst \"${XTDB_VALID_TIME}\"}"
# AS OF traversal: the entity's :knows neighbours as of a historical valid time.
measure ext_temporal_as_of_traversal "EXT-TEMPORAL AS OF traversal (:knows neighbours)" \
    "{:query {:find [friend] :where [[e ${XTDB_ID_ATTR} ${PERSON_ID}] [e :knows friend]]} :valid-time #inst \"${XTDB_VALID_TIME}\"}"

# ---- SNB property-graph reads: not expressed natively on XTDB (footnote 2) --
for k in is1_person_profile is2_person_recent_messages is3_person_friends \
         is5_message_creator is6_forum_of_message ic1_friends_within_2_hops \
         ic2_recent_messages_by_friends ic9_messages_by_friends_of_friends; do
    not_expressible "${k}" \
        "XTDB is not a property-graph engine; the SNB interactive graph subset is a separate modeling exercise, not the same native operation (README footnote 2)."
done

# ---- Vector extension: XTDB has no native ANN (footnotes 4 & 5) -------------
not_expressible ext_vector_knn "XTDB has no native ANN / vector index (README footnote 4)."
not_expressible ext_vector_hybrid_graph_vector \
    "Single planned graph+vector operation is AletheiaDB-specific; XTDB has no vector index (README footnotes 4 & 5)."

emit_results_json xtdb "juxt/xtdb:1.24.0" "${SAMPLE_DIR}"
echo "[xtdb] done."
