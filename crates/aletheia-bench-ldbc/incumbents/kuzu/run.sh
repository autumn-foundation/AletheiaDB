#!/usr/bin/env bash
# Comparative run stub for kuzu (Issue #3373). NOT executed in the sandbox that
# produced the committed AletheiaDB results. Prints the exact reproduction steps
# and the query file to time. It intentionally SYNTHESIZES NO NUMBERS.
set -euo pipefail
echo "[kuzu] This is a documented stub, not a live runner."
echo "[kuzu] Reproduction steps:"
echo "  1. docker compose -f ../docker-compose.yml up -d kuzu"
echo "  2. Load the SHARED generated graph (same data as AletheiaDB) into kuzu."
echo "     See ../README.md for the logical schema and data-interchange notes."
echo "  3. Time each mapped query in: kuzu/queries.*"
echo "     Use warmup + N measured iterations; report p50/p95/p99 (match METHODOLOGY.md)."
echo "  4. Emit results/kuzu.json in the same shape as the AletheiaDB report."
echo "[kuzu] Wiring this to a live driver is a follow-up; it is left unrun rather"
echo "[kuzu] than fabricating numbers. See ../README.md capability table."
exit 0
