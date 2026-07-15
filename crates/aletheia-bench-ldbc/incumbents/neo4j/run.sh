#!/usr/bin/env bash
# Comparative run stub for neo4j (Issue #3373). NOT executed in the sandbox that
# produced the committed AletheiaDB results. Prints the exact reproduction steps
# and the query file to time. It intentionally SYNTHESIZES NO NUMBERS.
set -euo pipefail
echo "[neo4j] This is a documented stub, not a live runner."
echo "[neo4j] Reproduction steps:"
echo "  1. docker compose -f ../docker-compose.yml up -d neo4j"
echo "  2. Load the SHARED generated graph (same data as AletheiaDB) into neo4j."
echo "     See ../README.md for the logical schema and data-interchange notes."
echo "  3. Time each mapped query in: neo4j/queries.*"
echo "     Use warmup + N measured iterations; report p50/p95/p99 (match METHODOLOGY.md)."
echo "  4. Emit results/neo4j.json in the same shape as the AletheiaDB report."
echo "[neo4j] Wiring this to a live driver is a follow-up; it is left unrun rather"
echo "[neo4j] than fabricating numbers. See ../README.md capability table."
exit 0
