#!/usr/bin/env bash
# ============================================================================
# run_all.sh — drive the incumbent-engine comparison (Issue #3628 / #3373).
#
# NOT executed in the sandbox that produced the committed AletheiaDB results.
# Run this ON THE SELF-HOSTED BOX, where Docker and the engine images are
# available. It brings up the selected engine(s) via docker-compose, then runs
# each mapped query with a warmup + N timed iterations, computes p50/p95/p99
# (nearest-rank, matching the AletheiaDB harness), and writes
# results/<engine>_results.json. It does NOT bulk-load the dataset — each driver
# assumes the shared SNB graph has already been loaded into its engine (see the
# per-driver README notes) and fails loudly against an empty store.
#
# REQUIRED TOOLING (fail loud if missing):
#   * docker + docker compose v2   (engine containers)
#   * python3                      (percentile math / JSON emit)
#   * bash 5+                      ($EPOCHREALTIME for µs timing; falls back to
#                                   `date +%s%6N`)
#   * the engine images in docker-compose.yml (neo4j, kuzu, xtdb)
#
# It emits NO synthetic numbers. A missing engine/container is a hard error.
# Queries an engine cannot express natively are recorded as
# {not_expressible:true} (a capability fact), never as a fabricated latency.
#
# USAGE:
#   ./run_all.sh [--engines "neo4j kuzu xtdb"] [--iterations N] [--warmup N]
#                [--datagen-dir /abs/path/to/ldbc/sfN] [--person-id ID]
# ============================================================================
set -euo pipefail

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

ENGINES="neo4j kuzu xtdb"
ITERATIONS=200
WARMUP=20
DATAGEN_DIR=""
PERSON_ID=""

while [ "$#" -gt 0 ]; do
    case "$1" in
        --engines)      ENGINES="$2"; shift 2 ;;
        --iterations)   ITERATIONS="$2"; shift 2 ;;
        --warmup)       WARMUP="$2"; shift 2 ;;
        --datagen-dir)  DATAGEN_DIR="$2"; shift 2 ;;
        --person-id)    PERSON_ID="$2"; shift 2 ;;
        -h|--help)
            grep -E '^#( |$)' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done

require_cmd docker
require_cmd python3

echo "[run_all] engines=[${ENGINES}] iterations=${ITERATIONS} warmup=${WARMUP}"
if [ -n "${DATAGEN_DIR}" ]; then
    [ -d "${DATAGEN_DIR}" ] || die "--datagen-dir '${DATAGEN_DIR}' is not a directory"
    echo "[run_all] dataset source: official Datagen dir ${DATAGEN_DIR}"
else
    echo "[run_all] dataset source: shared generated graph (see README.md data-interchange)"
fi

mkdir -p "${RESULTS_DIR}"

export ITERATIONS WARMUP DATAGEN_DIR PERSON_ID

status=0
for engine in ${ENGINES}; do
    driver="${INCUMBENTS_DIR}/${engine}/run.sh"
    [ -x "${driver}" ] || die "no driver script at ${driver}"
    echo "=============================================================="
    echo "[run_all] driving ${engine}"
    echo "=============================================================="
    # Do not let one engine's failure abort the others, but track overall status
    # so the run reports non-zero if any engine failed.
    if ! "${driver}"; then
        echo "[run_all] engine ${engine} FAILED (see message above)" >&2
        status=1
    fi
done

echo "[run_all] results written under ${RESULTS_DIR}/ :"
ls -1 "${RESULTS_DIR}" 2>/dev/null || true
exit "${status}"
