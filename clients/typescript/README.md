# `@aletheiadb/client`

The official **TypeScript SDK** for [AletheiaDB](https://github.com/madmax983/AletheiaDB) — a bi-temporal graph + vector database. Fully typed nodes/edges/properties, ergonomic `asOf()` temporal calls, vector-elision-aware types, and the structured error contract surfaced as a typed error hierarchy with an opt-in retry policy.

- Runs on **Node ≥ 18** and standard-`fetch` edge runtimes (no Node-only deps in the core).
- **ESM + CJS** dual build with full type declarations.
- **0 `any`** in the published type surface (lint-enforced).

> **Status:** wraps the full autumn-server HTTP surface — node/edge/traverse/temporal + admin/health **and** the vector/hybrid/query/schema/stats/batch/lineage tools (all 46 tools, Issue #3627). See [Coverage](#coverage) and [`COMPATIBILITY.md`](./COMPATIBILITY.md).

## Install

```bash
npm install @aletheiadb/client
```

## Quickstart

```ts
import { AletheiaClient } from '@aletheiadb/client';

const db = new AletheiaClient({
  baseUrl: 'http://localhost:8080',
  apiKey: process.env.ALETHEIA_API_KEY, // omit only against an anonymous-mode server
});

// Health + size
console.log((await db.status()).status); // "healthy"

// Create nodes and an edge
const alice = await db.createNode({ label: 'Person', properties: { name: 'Alice' } });
const bob = await db.createNode({ label: 'Person', properties: { name: 'Bob' } });
await db.createEdge({ sourceId: alice.id, targetId: bob.id, label: 'KNOWS' });

// Traverse
const friends = await db.traverse({ startNodeId: alice.id, edgeLabel: 'KNOWS', depth: 2 });
for (const row of friends.results) console.log(row.node.properties.name);
```

Time-to-first-query target: **< 5 minutes** from `npm install` given a running server.

## Bi-temporal `asOf` usage

AletheiaDB tracks two independent time dimensions on every fact:

- **valid time** — when the fact was true *in reality* (you control it).
- **transaction time** — when the fact was *recorded* (system-assigned; read-only).

Every temporal parameter accepts a `Date`, an ISO 8601 string, **or** a number of epoch-**microseconds** — all coerced to the same wire value (millisecond precision for `Date`/string; supply a number for finer resolution).

```ts
// "Who did Alice know on 2024-01-01?" — a point-in-time traversal.
const asOf2024 = db.asOf({ validTime: '2024-01-01T00:00:00Z' });
const knownThen = await asOf2024.traverse({ startNodeId: alice.id, edgeLabel: 'KNOWS' });

// Each dimension is independent — set one, the other, or both:
db.asOf({ transactionTime: new Date('2024-06-01') });          // tx-time only
db.asOf({ validTime: 1704067200000000, transactionTime: '…' }); // both

// Back-date a write with valid_time:
await db.createEdge({
  sourceId: alice.id, targetId: bob.id, label: 'KNOWS',
  validTime: new Date('2020-06-01T00:00:00Z'),
});

// Point-in-time reads:
await db.getNodeAtTime({ nodeId: alice.id, validTime: '2024-01-01', transactionTime: '2024-06-01' });
await db.findNodesAtTime({ label: 'Person', propertyKey: 'name', propertyValue: 'Alice', validTime: '2024-01-01' });
```

## Vector elision (typed)

By default, embedding/vector properties come back as a compact **elided descriptor** rather than the raw float array (Issue #3220). The type is a discriminated union, so you cannot misread a descriptor as data:

```ts
import { isElidedVector, isFullVector } from '@aletheiadb/client';

const node = await db.getNode(id);                       // elided by default
const emb = node.properties.embedding;
if (isElidedVector(emb)) console.log('dim', emb.dim);     // { type:'vector', dim, elided:true }

const full = await db.getNode(id, { includeVectors: true });
if (isFullVector(full.properties.embedding)) { /* number[] */ }
```

## Errors and retries

Every error maps to a typed subclass of `AletheiaError`, carrying `code`, `message`, `retriable`, and structured `details` (Issue #3234):

```ts
import { NotFoundError, PermissionDeniedError, ConflictError, AletheiaError } from '@aletheiadb/client';

try {
  await db.getNode(999_999);
} catch (err) {
  if (err instanceof NotFoundError) { /* ... */ }
  else if (err instanceof AletheiaError) { console.log(err.code, err.retriable, err.details); }
}
```

| Code | Class | Retriable |
|------|-------|-----------|
| `NOT_FOUND` | `NotFoundError` | no |
| `INVALID_ARGUMENT` | `InvalidArgumentError` | no |
| `CONSTRAINT_VIOLATION` | `ConstraintViolationError` | no |
| `FAILED_PRECONDITION` | `FailedPreconditionError` | no |
| `CONFLICT` | `ConflictError` | usually |
| `UNAVAILABLE` | `UnavailableError` | yes |
| `INTERNAL` | `InternalError` | no |
| `UNAUTHENTICATED` | `UnauthenticatedError` | no |
| `PERMISSION_DENIED` | `PermissionDeniedError` | no |
| `RESOURCE_EXHAUSTED` | `ResourceExhaustedError` | sometimes |

An **unknown** code degrades to the base `AletheiaError` with `retriable === false`.

The built-in retry policy is **off by default**. When enabled it retries **only** `retriable` errors — never a non-retriable code — with bounded attempts and jittered exponential backoff:

```ts
const db = new AletheiaClient({
  baseUrl, apiKey,
  retry: { enabled: true, maxAttempts: 3, baseDelayMs: 100, maxDelayMs: 2000 },
});
```

## Auth & configuration

```ts
new AletheiaClient({
  baseUrl: 'http://localhost:8080',
  apiKey: 'aletheia_sk_…',
  authScheme: 'bearer',   // default; or 'x-api-key'
  fetch: customFetch,     // inject a fetch for older Node / tests / polyfills
  headers: { 'x-tenant': 'acme' },
});
```

## Pagination & completeness

Reads that support it accept `limit`/`offset` (#3226), `useCursor`/`cursor` (#3360), and the token budget `maxResponseTokens`/`maxResponseBytes`/`priorityProperties` (#3353). Responses surface `count`, `has_more`, `next_offset`, `truncated`, `sampled`, `cursor`, `snapshot_valid_time`/`snapshot_transaction_time`, and `budget`:

```ts
const page1 = await db.listNodes({ label: 'Person', useCursor: true });
if (page1.has_more) {
  const page2 = await db.listNodes({ cursor: page1.cursor }); // pass cursor alone
}
```

## Coverage

**Wrapped:**

- **Nodes:** `getNode`, `listNodes`, `countNodes`, `createNode`, `updateNode`, `deleteNode`, `deleteNodeCascade`, `retractNode`, `findNodesAtTime`
- **Edges:** `getEdge`, `listEdges`, `countEdges`, `getOutgoingEdges`, `getIncomingEdges`, `createEdge`, `updateEdge`, `deleteEdge`, `retractEdge`
- **Traversal / temporal:** `traverse`, `getNodeHistory`, `getEdgeHistory`, `getNodeAtTime`, `getEdgeAtTime`, `getNodeAtValidTime`, `getNodeAtTransactionTime`, `getEdgeAtValidTime`, `getEdgeAtTransactionTime`, `diffNodeVersions`, `diffEdgeVersions`, `listChanges`
- **Vector:** `findSimilar`, `enableVectorIndex`, `listVectorIndexes`
- **Hybrid / query:** `hybridQuery`, `query`
- **Schema / stats / extent:** `getSchema`, `databaseStats`, `temporalExtent`
- **Batch / lineage:** `applyBatch`, `lineageUpstream`, `lineageDownstream`
- **Admin / health:** `status`, `createKey`, `listKeys`, `revokeKey`

All 46 tools are now live on the autumn HTTP surface (Issue #3627). `NotImplementedError` is retained as an exported type for backward compatibility but is no longer thrown by any client method.

## Development

```bash
npm install
npm run lint       # eslint (bans `any` in the public surface)
npm run typecheck  # tsc --noEmit (strict) for src + examples
npm run test       # vitest (record-replay fetch fixtures)
npm run build      # tsup -> dist (ESM + CJS + d.ts)
npm run smoke      # import the built ESM and CJS entry points
```

The unit tests use **record-replay fetch fixtures** (an injected mock `fetch` returning canned responses shaped to `tests/parity/inventory.json` and the autumn-server `*_tools.rs` handlers), not a live server binary. Integration against a real server binary is a separate CI job wired as the HTTP routes stabilize.

## License

MIT OR Apache-2.0
