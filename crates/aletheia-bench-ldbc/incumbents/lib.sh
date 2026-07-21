#!/usr/bin/env bash
# Shared helpers for the incumbent driver scripts (Issue #3628 / #3373).
#
# NOT executed in the sandbox that produced the committed AletheiaDB results.
# These helpers assume they run on the self-hosted box WITH the engine
# containers up (see docker-compose.yml) and Docker available.
#
# The helpers deliberately emit NO synthetic numbers: every latency they record
# comes from an actual timed query invocation against a running engine. If the
# engine/container is missing, callers fail loudly (see require_container).
set -euo pipefail

# Directory this library lives in (the incumbents/ root), resolved absolutely.
INCUMBENTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export INCUMBENTS_DIR
RESULTS_DIR="${INCUMBENTS_DIR}/results"
export RESULTS_DIR

# Fail with a clear message and non-zero exit. Never continue past a hard error
# (we must not fabricate a result to paper over a missing engine).
die() {
    echo "ERROR: $*" >&2
    exit 1
}

# Assert a required command exists on PATH.
require_cmd() {
    local cmd="$1"
    command -v "${cmd}" >/dev/null 2>&1 \
        || die "required command '${cmd}' not found on PATH. See incumbents/README.md prerequisites."
}

# Assert a named docker compose service is running; loud failure otherwise so we
# never emit fake data for an engine that is not actually up.
require_container() {
    local service="$1"
    require_cmd docker
    # `docker compose ps` lists running services for the compose project rooted
    # at incumbents/docker-compose.yml.
    if ! docker compose -f "${INCUMBENTS_DIR}/docker-compose.yml" ps --status running \
        --services 2>/dev/null | grep -qx "${service}"; then
        die "engine container '${service}' is not running. Start it with:
    docker compose -f incumbents/docker-compose.yml up -d ${service}
Refusing to emit synthetic numbers for a stopped engine."
    fi
}

# Time a single shell command's wall-clock latency in MICROSECONDS, echoed to
# stdout as an integer. Uses the shell's nanosecond clock ($EPOCHREALTIME when
# available, else `date +%s.%N`). The command's own stdout/stderr are discarded
# so only the measurement is captured; a non-zero exit from the command is a
# hard error (a failed query must not be silently counted as a fast one).
time_once_us() {
    local start end
    if [ -n "${EPOCHREALTIME:-}" ]; then
        start="${EPOCHREALTIME/./}"   # seconds.microseconds -> microseconds
        "$@" >/dev/null 2>&1 || die "timed command failed: $*"
        end="${EPOCHREALTIME/./}"
    else
        start="$(date +%s%6N)"
        "$@" >/dev/null 2>&1 || die "timed command failed: $*"
        end="$(date +%s%6N)"
    fi
    # Strip any leading zeros to avoid octal interpretation in arithmetic.
    echo $(( 10#${end} - 10#${start} ))
}

# Given: engine name, version string, and a results directory that already
# contains one file per query key named "<key>.samples" (whitespace-separated
# integer microsecond samples), plus optional "<key>.notexpressible" marker
# files, write results/<engine>_results.json in a shape comparable to the
# AletheiaDB report. Percentiles use the nearest-rank method (matching
# crates/aletheia-bench-ldbc/src/stats.rs) so the numbers line up.
#
# Args: engine version sample_glob_dir
emit_results_json() {
    local engine="$1" version="$2" sample_dir="$3"
    mkdir -p "${RESULTS_DIR}"
    local out="${RESULTS_DIR}/${engine}_results.json"
    ENGINE="${engine}" VERSION="${version}" SAMPLE_DIR="${sample_dir}" OUT="${out}" \
        python3 "${INCUMBENTS_DIR}/emit_results.py"
    echo "[${engine}] wrote ${out}"
}
