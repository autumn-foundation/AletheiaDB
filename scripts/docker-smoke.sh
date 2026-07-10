#!/usr/bin/env bash
#
# Smoke + durability test for the AletheiaDB container image.
#
# Exercises the acceptance criteria of issue #3381 against a *built* image:
#   1. Refuses to boot with no bootstrap key (auth is on by default, #3350).
#   2. Boots with a bootstrap key + signing secret; healthcheck reports healthy.
#   3. create node -> query it -> AS OF query over the HTTP API.
#   4. Volume persists across a clean restart (SIGTERM) and a hard kill
#      (SIGKILL -> WAL recovery) with zero data loss.
#
# Usage: scripts/docker-smoke.sh [IMAGE]   (default: aletheiadb:ci)
set -euo pipefail

IMAGE="${1:-aletheiadb:ci}"
PORT="${SMOKE_PORT:-19630}"
VOL="aletheiadb_smoke_$$"
NAME="aletheiadb_smoke_$$"
ADMIN_KEY="smoke-admin-$(date +%s)-$RANDOM"
SIGNING_SECRET="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
BASE="http://127.0.0.1:${PORT}"

log()  { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }
fail() { printf '\n\033[1;31mFAIL: %s\033[0m\n' "$*" >&2; exit 1; }

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker rm -f "${NAME}_nokey" >/dev/null 2>&1 || true
  docker volume rm "$VOL" >/dev/null 2>&1 || true
}
trap cleanup EXIT

api() { # api METHOD PATH [JSON]
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -fsS -X "$method" "$BASE$path" \
      -H "x-api-key: $ADMIN_KEY" -H 'content-type: application/json' -d "$body"
  else
    curl -fsS -X "$method" "$BASE$path" -H "x-api-key: $ADMIN_KEY"
  fi
}

json_field() { python3 -c 'import sys,json; d=json.load(sys.stdin);
k=sys.argv[1].split(".")
for part in k:
    d=d[int(part)] if isinstance(d,list) else d[part]
print(d)' "$1"; }

wait_healthy() { # wait_healthy NAME TIMEOUT
  local name="$1" timeout="${2:-40}" i=0 st
  while [ "$i" -lt "$timeout" ]; do
    st="$(docker inspect -f '{{.State.Health.Status}}' "$name" 2>/dev/null || echo missing)"
    [ "$st" = "healthy" ] && return 0
    if [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null)" = "false" ]; then
      docker logs "$name" 2>&1 | tail -20; return 1
    fi
    i=$((i+1)); sleep 1
  done
  docker logs "$name" 2>&1 | tail -20; return 1
}

# ── 1. Refuse to boot with no bootstrap key ────────────────────────────────
log "1. Refuses to boot with no bootstrap key"
docker rm -f "${NAME}_nokey" >/dev/null 2>&1 || true
docker run -d --name "${NAME}_nokey" \
  -e AUTUMN_SECURITY__SIGNING_SECRET="$SIGNING_SECRET" \
  "$IMAGE" >/dev/null
sleep 6
running="$(docker inspect -f '{{.State.Running}}' "${NAME}_nokey" 2>/dev/null || echo missing)"
if [ "$running" = "true" ]; then
  docker logs "${NAME}_nokey" 2>&1 | tail -20
  fail "container stayed up without a bootstrap key (auth-off regression)"
fi
echo "OK: container exited without a bootstrap key"
docker logs "${NAME}_nokey" 2>&1 | tail -3 || true
docker rm -f "${NAME}_nokey" >/dev/null 2>&1 || true

# ── 2. Boots with key; healthcheck healthy ─────────────────────────────────
log "2. Boots with a bootstrap key; healthcheck -> healthy"
docker volume create "$VOL" >/dev/null
start_ts=$(date +%s.%N)
docker run -d --name "$NAME" -p "${PORT}:1963" \
  -e ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$ADMIN_KEY" \
  -e AUTUMN_SECURITY__SIGNING_SECRET="$SIGNING_SECRET" \
  -v "$VOL:/var/lib/aletheiadb" \
  "$IMAGE" >/dev/null
wait_healthy "$NAME" 60 || fail "container did not become healthy"
end_ts=$(date +%s.%N)
printf 'OK: healthy in %.1fs\n' "$(echo "$end_ts - $start_ts" | bc)"

# ── 3. create node -> query it -> AS OF query ──────────────────────────────
log "3. create node -> query it -> AS OF query"
create_resp="$(api POST /query '{"operation":"create_node","label":"Person","properties":{"name":"Alice"}}')"
echo "create: $create_resp"
NODE_ID="$(echo "$create_resp" | json_field data.id)"
[ -n "$NODE_ID" ] || fail "no node id in create response"
echo "OK: created node $NODE_ID"

get_resp="$(api POST /query "{\"operation\":\"get_node\",\"node_id\":$NODE_ID}")"
echo "$get_resp" | grep -q '"Alice"' || fail "get_node did not return Alice"
echo "OK: queried node back"

# AS OF takes microseconds since epoch; "now" must contain the node just made.
NOW_US=$(( $(date +%s) * 1000000 ))
asof_resp="$(api POST /query "{\"operation\":\"execute_query\",\"query\":\"AS OF '$NOW_US' MATCH (n:Person) RETURN n\"}")"
echo "as-of: $asof_resp"
echo "$asof_resp" | grep -q '"Alice"' || fail "AS OF query did not return Alice"
echo "OK: AS OF query returned the node"

# ── 4a. Clean restart (SIGTERM) persists data ──────────────────────────────
log "4a. Clean restart (SIGTERM) preserves data"
docker stop "$NAME" >/dev/null
docker start "$NAME" >/dev/null
wait_healthy "$NAME" 60 || fail "container did not become healthy after restart"
api POST /query "{\"operation\":\"get_node\",\"node_id\":$NODE_ID}" | grep -q '"Alice"' \
  || fail "node lost after clean restart"
echo "OK: node survived clean restart"

# ── 4b. Hard kill (SIGKILL) -> WAL recovery ────────────────────────────────
log "4b. Hard kill (SIGKILL) preserves data via WAL recovery"
docker kill "$NAME" >/dev/null
docker start "$NAME" >/dev/null
wait_healthy "$NAME" 60 || fail "container did not become healthy after kill"
api POST /query "{\"operation\":\"get_node\",\"node_id\":$NODE_ID}" | grep -q '"Alice"' \
  || fail "node lost after hard kill"
echo "OK: node survived hard kill (WAL recovery)"

log "ALL SMOKE + DURABILITY CHECKS PASSED"
