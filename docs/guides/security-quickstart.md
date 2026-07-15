# Security Quickstart: Authentication & RBAC

How to go from an open development database to an authenticated,
multi-role AletheiaDB deployment (Issue #3350). Covers both serving
surfaces: the HTTP server (`aletheia-server`) and the MCP server
(`aletheia-mcp`).

The per-operation authorization matrix (which role may call which
endpoint/tool) lives in
[docs/guides/access-control-matrix.md](access-control-matrix.md) and is
kept in lockstep with the code by CI conformance tests.

## The security model in one minute

- **Authentication is ON by default.** Both servers start in
  `required` mode and **refuse to start** with zero credentials — there
  is no accidentally-open deployment. Anonymous access is an explicit,
  loudly-warned opt-in.
- **API keys, four roles.** Credentials are bearer API keys
  (`aletheia_sk_` + 43 random characters, 256 bits from the OS CSPRNG).
  Each key maps to a principal with one role:

  | Role      | read | write | metrics | admin |
  |-----------|------|-------|---------|-------|
  | `admin`   | ✅   | ✅    | ✅      | ✅    |
  | `writer`  | ✅   | ✅    | ✅      | ❌    |
  | `reader`  | ✅   | ❌    | ✅      | ❌    |
  | `metrics` | ❌   | ❌    | ✅      | ❌    |

- **Keys are never stored in plaintext.** The store keeps SHA-256
  digests only, verified in constant time; the plaintext is returned
  exactly once at creation. The persisted file is
  `{data_dir}/auth/keys.json`, written atomically with mode `0600`.
- **Failures don't leak.** A missing, malformed, unknown, or revoked
  credential produces a byte-identical `401 UNAUTHENTICATED` — there is
  no way to probe whether a key exists. Authenticated-but-insufficient
  roles get `403 PERMISSION_DENIED` naming the required class.
- **Revocation is immediate.** Credentials are re-verified on every
  call (HTTP request / MCP tool call); revoking a key takes effect on
  the very next call, with no cached sessions to wait out.
- **Writes are attributed — on the structured write paths.** A write
  made with an authenticated key stamps the principal's name into that
  version's provenance (`provenance.principal`), composing with any
  caller-supplied provenance `source` — "who wrote this fact" stays
  answerable from bi-temporal history. The stamped paths are the
  structured create/update operations: MCP `create_node` /
  `update_node` / `create_edge` / `update_edge`, and HTTP `/query`
  `create_node` / `bulk_create_nodes` / `bulk_update_nodes`.
  **Not stamped (known gap, tracked as Issue #3427)**: deletes and
  retracts — the delete/retract WAL entries carry no provenance slot,
  so a stamp would not survive WAL-replay crash recovery; durable
  destructive-op attribution needs a WAL payload extension and is
  deliberately deferred rather than shipped half-durable — and
  mutating AQL statements via HTTP `execute_query` /
  `bulk_execute_query` (identity is not yet threaded into the query
  executor; also noted in #3427). Those writes are still authorized by
  role; they just aren't attributed in provenance.

## Step 1 — Bootstrap the first admin credential

In `required` mode the server needs at least one credential to start.
Supply an operator-chosen bootstrap admin key via the environment:

```bash
# Generate a strong secret (any high-entropy string works).
export ALETHEIADB_BOOTSTRAP_ADMIN_KEY="$(openssl rand -base64 32)"

# HTTP server
ALETHEIADB_DATA_DIR=/var/lib/aletheiadb \
  cargo run --bin aletheia-server --features http-server
```

Without it, startup is refused with a message naming both the variable
and the anonymous opt-out:

```
aletheia-server failed: authentication is required (the default) but no
credentials are available: set a bootstrap admin key
(ALETHEIADB_BOOTSTRAP_ADMIN_KEY or
ServerConfig::builder().bootstrap_admin_key(...)), point the server at an
existing persisted key store, or explicitly opt into anonymous mode
(ALETHEIADB_AUTH_MODE=anonymous)
```

The bootstrap key is **memory-only**: it never enters the persisted
key file, and you re-supply it via the environment on every start. It
authenticates as the principal `bootstrap-admin` with role `admin`.
Treat it as a break-glass credential — use it to mint real keys, then
prefer those day to day.

Docker note: `docker-compose.yml` requires
`ALETHEIADB_BOOTSTRAP_ADMIN_KEY` to be set on the host (compose refuses
to start otherwise) and passes it through to the container and its
health check.

## Step 2 — Mint role-scoped keys over the admin API

The HTTP admin endpoints (all `admin`-class) manage the key lifecycle.
Keys minted here are persisted to `{data_dir}/auth/keys.json` and are
valid on **both** surfaces when the HTTP and MCP servers share a data
dir.

Create a writer, a reader, and a metrics key:

```bash
BASE=http://127.0.0.1:1963
AUTH="Authorization: Bearer $ALETHEIADB_BOOTSTRAP_ADMIN_KEY"

curl -sS -X POST "$BASE/admin/keys" -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"name": "ingest-service", "role": "writer"}'
# -> { "success": true, "data": {
#        "id": "...", "name": "ingest-service", "role": "writer",
#        "key_prefix": "aletheia_sk_AbCdEfGh",
#        "key": "aletheia_sk_..."        <- plaintext, returned EXACTLY ONCE
#      } }

curl -sS -X POST "$BASE/admin/keys" -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"name": "dashboard", "role": "reader"}'

curl -sS -X POST "$BASE/admin/keys" -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"name": "prometheus", "role": "metrics"}'
```

Store the returned `key` values in your secret manager immediately —
the server keeps only their hashes and cannot show them again.

## Step 3 — Use the keys

**HTTP** accepts either header form:

```bash
# Authorization: Bearer
curl -sS -X POST "$BASE/query" \
  -H "Authorization: Bearer $WRITER_KEY" -H "Content-Type: application/json" \
  -d '{"operation": "create_node", "label": "Person", "properties": {"name": "Alice"}}'

# x-api-key
curl -sS "$BASE/status" -H "x-api-key: $METRICS_KEY"
```

**MCP** is a stdio transport, so the credential is session-scoped:
supply it at process start and it is re-verified on every tool call.

```bash
ALETHEIADB_DATA_DIR=/var/lib/aletheiadb \
ALETHEIADB_MCP_API_KEY="$WRITER_KEY" \
  cargo run --bin aletheia-mcp --features mcp-server
```

Pointing `ALETHEIADB_DATA_DIR` at the same directory as the HTTP server
makes keys minted in Step 2 work here too (shared
`{data_dir}/auth/keys.json`). The MCP server also honors
`ALETHEIADB_BOOTSTRAP_ADMIN_KEY`.

A request denied for role reasons returns a structured error the caller
can act on — see the
[error-code contract](mcp-query-tool.md#structured-error-codes-and-the-retriable-contract)
(`UNAUTHENTICATED` / `PERMISSION_DENIED`, both `retriable: false`).

> **Breaking change (Issue #3234):** the **HTTP** error body now uses the same
> nested envelope as the MCP surface —
> `{"error":{"code","message","retriable","details"?}}`, with `trace_id` (when
> present) a top-level sibling of `error`. The legacy flat HTTP body
> (`{"success":false,"error":"<msg>","code":…}`) has been removed; read
> `error.code` / `error.message` / `error.retriable` / `error.details`. A
> `403 PERMISSION_DENIED` now also carries
> `error.details:{required_class, principal_role}` on the HTTP surface, matching
> MCP exactly. Success responses are unchanged (`{"success":true,"data":…}`).

## Step 4 — Audit and revoke

Listing is **masked by construction** — the response can only carry the
display prefix, never key material:

```bash
curl -sS "$BASE/admin/keys" -H "$AUTH"
# -> { "success": true, "data": { "keys": [
#        { "id": "...", "name": "ingest-service", "role": "writer",
#          "key_prefix": "aletheia_sk_AbCdEfGh", "created_at": ... }, ... ] } }

curl -sS -X POST "$BASE/admin/keys/revoke" -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"id": "<principal-id-from-the-list>"}'
# -> { "success": true, "data": { "revoked": true, "id": "..." } }
```

Revocation is durable (persisted immediately) and effective on the next
call on both surfaces — including MCP sessions already running with
that key.

## Framework endpoints that bypass API-key auth (HTTP server)

The HTTP server is built on the autumn-web framework, which mounts a
set of **its own** routes outside AletheiaDB's authentication layer.
These respond **without any AletheiaDB credential**, in every auth
mode — so "every route requires a credential" is true of AletheiaDB's
routes (`/query`, `/status`, `/admin/keys*`), not of the whole port:

- Health probes: `GET /health`, `/live`, `/ready`, `/startup`
- Actuator (health/metadata group): `GET /actuator/health`,
  `/actuator/info`, `/actuator/metrics`, `/actuator/a11y`,
  `/actuator/ui`, `/actuator/ui/metrics`

These expose service liveness, framework version/profile, and request
metrics — no database contents and no credentials. The framework's
**sensitive** actuator group (`/actuator/env` — a full config dump —
`/actuator/configprops`, an unauthenticated `PUT
/actuator/loggers/{name}`, `/actuator/tasks`, `/actuator/jobs`,
`/actuator/prometheus`), which autumn's dev profile would otherwise
enable, is **force-disabled by AletheiaDB in every profile** (the
server pins `actuator.sensitive = false` through its config loader; an
`autumn.toml` cannot re-enable it). The server prints the exact list of
unauthenticated framework routes at startup.

**Recommendation**: if these paths must not be publicly reachable,
block `/actuator` and the probe paths (`/health`, `/live`, `/ready`,
`/startup`) at your reverse proxy, allowing them only from your
orchestrator/monitoring networks.

## Anonymous mode (explicit opt-in only)

For local development you can disable authentication entirely:

```bash
ALETHEIADB_AUTH_MODE=anonymous cargo run --bin aletheia-server --features http-server
```

> **WARNING**: anonymous mode gives every caller full, unauthenticated
> access to the database. Both servers print a prominent warning at
> startup when it is enabled. Never expose an anonymous-mode server to
> untrusted networks. An **invalid** `ALETHEIADB_AUTH_MODE` value falls
> back to `required` (fail-closed), never to anonymous.

Anonymous-mode writes record no provenance principal (the field is
absent, not an empty string).

## Environment variable reference

| Variable | Surface | Meaning |
|----------|---------|---------|
| `ALETHEIADB_AUTH_MODE` | both | `required` (default) or `anonymous`; invalid values warn and fail closed to `required` |
| `ALETHEIADB_BOOTSTRAP_ADMIN_KEY` | both | Memory-only admin credential (`bootstrap-admin`); never persisted |
| `ALETHEIADB_MCP_API_KEY` | MCP | The stdio session's credential, re-verified per tool call |
| `ALETHEIADB_DATA_DIR` | both | Derives the shared key-store path `{data_dir}/auth/keys.json`; no data dir ⇒ memory-only store |

## What this does — and does not — include

Included (Issue #3350): API-key authentication, four fixed roles,
per-operation enforcement on HTTP and MCP, admin key lifecycle,
persisted hashed key store, provenance principal stamping, uniform
non-leaking auth errors.

**Not** included — deliberate follow-ups, not gaps to work around:

- **TLS**: transport encryption is deployment-level. Terminate TLS at
  your reverse proxy / service mesh; API keys are bearer secrets and
  must not cross untrusted networks in the clear.
- **OIDC / SAML / OAuth** identity-provider integration.
- **Per-label / per-property grants** (row- or field-level
  authorization); roles are database-wide.
- Rate limiting on authentication attempts.

## Operational notes

- Per-call auth overhead is a SHA-256 hash plus a constant-time scan of
  the key store — measured at ~3 µs per call in the repo's evidence
  test (budget: <0.5 ms), so re-verification-per-call costs nothing
  noticeable.
- The key file is small JSON (`version: 1`); back it up with the data
  dir. Bootstrap keys are intentionally absent from it.
- The HTTP `GET /status` health check is `metrics`-class: in `required`
  mode probes need a credential (any role). The shipped Docker health
  check passes the bootstrap key via `x-api-key`.
- **`ALETHEIADB_CONFIG`-only deployments get a memory-only key store**:
  the persisted key-store path derives from `ALETHEIADB_DATA_DIR` only,
  so set `ALETHEIADB_DATA_DIR` (or the explicit auth persist path) too
  if minted keys must survive restarts.
- **If a revoke returns a 5xx**, the revocation still holds in memory
  (fail-secure) but may not have reached disk: verify with
  `GET /admin/keys` and re-issue the revoke after fixing the disk
  problem, or the key can come back on the next restart.
- **Docker health checks on persisted-store deployments**: the shipped
  `HEALTHCHECK` sends the bootstrap key; if you run without
  `ALETHEIADB_BOOTSTRAP_ADMIN_KEY` (relying on the persisted store
  alone), override the health check with a real credential — ideally a
  dedicated `metrics`-role key.
- **Release builds run under autumn's `prod` profile and require
  `AUTUMN_SECURITY__SIGNING_SECRET`** (≥32 bytes; the server exits at
  startup without it). AletheiaDB's API is token-based and stateless —
  the framework uses this secret for its session/CSRF machinery — but
  it must still be set; the shipped Dockerfile/compose require it
  alongside the bootstrap key (`openssl rand -hex 32`).
