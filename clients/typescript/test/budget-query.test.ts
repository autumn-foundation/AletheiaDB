import { describe, expect, it } from 'vitest';
import { AletheiaClient } from '../src/index.js';
import { edgeFixture, mockFetch, nodeFixture, type RecordedRequest } from './fixtures.js';

/**
 * `priority_properties` on GET reads (server PR #3638).
 *
 * The server's GET routes now accept `priority_properties` as a single,
 * **comma-separated** query param (they split on `,` server-side), so the SDK
 * serializes the `priorityProperties: string[]` option comma-joined onto the
 * query string of the nine budgetable GET reads. This mirrors the POST-body
 * reads, whose JSON body already carries the raw array.
 *
 * Notes on encoding:
 *  - The value is joined with `,` then URL-encoded ONCE by the transport
 *    (`%2C`), so `URLSearchParams.get()` decodes it back to `"a,b"`.
 *  - It rides as ONE param, not repeated keys — `getAll()` has length 1 — so
 *    the server sees a single comma-separated string, never a `Vec` from
 *    repeated keys (which `serde_urlencoded` still cannot decode).
 *  - An empty array is omitted entirely (no bare `?priority_properties=`).
 *  - `getSchema` is one of the nine: its `GetSchemaQuery` carries the same
 *    `de_priority_properties` comma-split as the other GET reads.
 */
async function capture(
  responseBody: unknown,
  run: (db: AletheiaClient) => Promise<unknown>,
): Promise<RecordedRequest> {
  const { fetch, calls } = mockFetch(() => ({ body: responseBody }));
  const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
  await run(db);
  return calls[0]!;
}

const budget = {
  maxResponseTokens: 800,
  maxResponseBytes: 4096,
  priorityProperties: ['title', 'name'],
};

const nodeList = { nodes: [], count: 0 };
const edgeList = { edges: [], count: 0 };
const traverseBody = { results: [], count: 0 };

describe('priority_properties is comma-joined onto the nine budgetable GET reads (#3638)', () => {
  const getReads: Array<[string, (db: AletheiaClient) => Promise<unknown>, unknown]> = [
    ['getNode', (db) => db.getNode(1, budget), nodeFixture(1, 'Doc', {})],
    ['listNodes', (db) => db.listNodes({ label: 'Doc', ...budget }), nodeList],
    ['getEdge', (db) => db.getEdge(1, budget), edgeFixture(1, 'KNOWS', 1, 2)],
    ['listEdges', (db) => db.listEdges({ label: 'KNOWS', ...budget }), edgeList],
    ['traverse', (db) => db.traverse({ startNodeId: 1, edgeLabel: 'KNOWS', ...budget }), traverseBody],
    ['getOutgoingEdges', (db) => db.getOutgoingEdges(1, budget), edgeList],
    ['getIncomingEdges', (db) => db.getIncomingEdges(1, budget), edgeList],
    ['getNodeHistory', (db) => db.getNodeHistory(1, budget), { node_id: 1, results: [] }],
    // `GET /schema` is budgetable too (#3353 `BUDGETABLE_READ_TOOLS`) and its
    // `GetSchemaQuery` carries the same `de_priority_properties` comma-split
    // (crates/aletheia-server/src/schema_batch_tools.rs).
    ['getSchema', (db) => db.getSchema(budget), { node_labels: [], edge_types: [] }],
  ];

  for (const [name, run, body] of getReads) {
    it(`${name}: emits comma-joined priority_properties + scalar budget`, async () => {
      const req = await capture(body, run);
      expect(req.method).toBe('GET');
      // Comma-joined, decoded back through URLSearchParams.
      expect(req.query.get('priority_properties')).toBe('title,name');
      // ONE param, not repeated keys.
      expect(req.query.getAll('priority_properties')).toHaveLength(1);
      // The scalar budget still rides.
      expect(req.query.get('max_response_tokens')).toBe('800');
      expect(req.query.get('max_response_bytes')).toBe('4096');
    });
  }
});

describe('priority_properties GET serialization — edge cases', () => {
  it('empty array is omitted entirely (no bare ?priority_properties=)', async () => {
    const req = await capture(nodeList, (db) =>
      db.listNodes({ label: 'Doc', priorityProperties: [] }),
    );
    expect(req.query.has('priority_properties')).toBe(false);
  });

  it('undefined is omitted (unchanged prior behavior)', async () => {
    const req = await capture(nodeList, (db) => db.listNodes({ label: 'Doc' }));
    expect(req.query.has('priority_properties')).toBe(false);
  });

  it('single element rides as a bare value', async () => {
    const req = await capture(nodeList, (db) =>
      db.listNodes({ label: 'Doc', priorityProperties: ['name'] }),
    );
    expect(req.query.get('priority_properties')).toBe('name');
    expect(req.query.getAll('priority_properties')).toHaveLength(1);
  });

  it('property names with special chars are URL-encoded (single param survives)', async () => {
    const req = await capture(nodeList, (db) =>
      db.listNodes({ label: 'Doc', priorityProperties: ['a b', 'c&d'] }),
    );
    // Decoded round-trip: one param, comma-joined, special chars intact.
    expect(req.query.getAll('priority_properties')).toHaveLength(1);
    expect(req.query.get('priority_properties')).toBe('a b,c&d');
  });

  it('getSchema omits priority_properties when the caller does not supply it', async () => {
    const req = await capture({ node_labels: [], edge_types: [] }, (db) =>
      db.getSchema({ maxResponseTokens: 1000 }),
    );
    expect(req.query.has('priority_properties')).toBe(false);
    expect(req.query.get('max_response_tokens')).toBe('1000');
  });

  it('getSchema merges asOf* + budget + priority_properties onto ONE query string', async () => {
    // The bi-temporal coordinates and the budget fragment are spread into the
    // same object literal; this pins that neither clobbers the other.
    const req = await capture({ node_labels: [], edge_types: [] }, (db) =>
      db.getSchema({
        asOfValidTime: '2024-01-01T00:00:00Z',
        asOfTransactionTime: '2024-06-01T00:00:00Z',
        ...budget,
      }),
    );
    expect(req.path).toBe('/schema');
    expect(req.query.has('as_of_valid_time')).toBe(true);
    expect(req.query.has('as_of_transaction_time')).toBe(true);
    expect(req.query.get('priority_properties')).toBe('title,name');
    expect(req.query.get('max_response_tokens')).toBe('800');
    expect(req.query.get('max_response_bytes')).toBe('4096');
  });

  it('the budget rides alongside a #3360 cursor continuation', async () => {
    // #3353 and #3360 compose: the cursor page is produced first, then the
    // budget shapes that page. Both must reach the server on one request.
    const req = await capture(nodeList, (db) =>
      db.listNodes({ cursor: 'opaque-token', ...budget }),
    );
    expect(req.query.get('cursor')).toBe('opaque-token');
    expect(req.query.get('priority_properties')).toBe('title,name');
    expect(req.query.get('max_response_tokens')).toBe('800');
  });
});

/**
 * Normalization is applied identically to the GET query form and the POST body
 * form, so one option object means the same thing on every read. Server-side
 * the GET routes trim + drop empties (`de_priority_properties`) while a POST
 * body deserializes verbatim, so without client-side normalization the same
 * input would protect different property sets on the two surfaces.
 */
describe('priority_properties normalization is identical on GET and POST', () => {
  it('drops empty and whitespace-only entries, and trims the rest (GET)', async () => {
    const req = await capture(nodeList, (db) =>
      db.listNodes({ label: 'Doc', priorityProperties: [' name ', '', '   ', 'title'] }),
    );
    expect(req.query.get('priority_properties')).toBe('name,title');
  });

  it('drops empty and whitespace-only entries, and trims the rest (POST)', async () => {
    const req = await capture({ nodes: [], count: 0 }, (db) =>
      db.findNodesAtTime({
        label: 'Person',
        validTime: '2024-01-01',
        priorityProperties: [' name ', '', '   ', 'title'],
      }),
    );
    expect((req.body as { priority_properties: unknown }).priority_properties).toEqual([
      'name',
      'title',
    ]);
  });

  it('an all-empty list is omitted entirely — never a bare ?priority_properties=', async () => {
    const req = await capture(nodeList, (db) =>
      db.listNodes({ label: 'Doc', priorityProperties: ['', '  '] }),
    );
    expect(req.query.has('priority_properties')).toBe(false);
    expect(req.url).not.toContain('priority_properties');
  });

  it('an all-empty list is omitted from a POST body too', async () => {
    const req = await capture({ nodes: [], count: 0 }, (db) =>
      db.findNodesAtTime({ label: 'Person', validTime: '2024-01-01', priorityProperties: [''] }),
    );
    expect((req.body as { priority_properties: unknown }).priority_properties).toBeUndefined();
  });

  it('a property name containing a comma throws instead of silently splitting', async () => {
    // The server splits the joined GET value on ',', so 'a,b' would protect two
    // properties that do not exist while the real one is elided — a silently
    // wrong response body. Fail loudly instead.
    const { fetch } = mockFetch(() => ({ body: nodeList }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const err = await db
      .listNodes({ label: 'Doc', priorityProperties: ['a,b'] })
      .catch((e: unknown) => e);
    expect((err as { code: string }).code).toBe('INVALID_ARGUMENT');
    expect((err as { retriable: boolean }).retriable).toBe(false);
  });

  it('the guard REJECTS the returned promise; it never throws synchronously', () => {
    // Argument validation happens while building the request spec, before the
    // transport is touched. The reads are `async` so that throw surfaces as a
    // rejection — a synchronous throw from a method typed `Promise<T>` would
    // escape every `.catch()` the caller wrote.
    const { fetch } = mockFetch(() => ({ body: nodeList }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    let threwSynchronously = false;
    let p: Promise<unknown> = Promise.resolve();
    try {
      p = db.listNodes({ priorityProperties: ['a,b'] });
    } catch {
      threwSynchronously = true;
    }
    expect(threwSynchronously).toBe(false);
    return expect(p).rejects.toMatchObject({ code: 'INVALID_ARGUMENT' });
  });

  it('the comma guard applies to POST-body reads too', async () => {
    const { fetch } = mockFetch(() => ({ body: { nodes: [], count: 0 } }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const err = await db
      .findNodesAtTime({ label: 'Person', validTime: '2024-01-01', priorityProperties: ['a,b'] })
      .catch((e: unknown) => e);
    expect((err as { code: string }).code).toBe('INVALID_ARGUMENT');
  });
});

describe('priority_properties IS preserved on POST-body reads (serde_json arrays)', () => {
  it('findNodesAtTime -> body carries priority_properties as a JSON array', async () => {
    const req = await capture({ nodes: [], count: 0 }, (db) =>
      db.findNodesAtTime({ label: 'Person', validTime: '2024-01-01', ...budget }),
    );
    expect(req.method).toBe('POST');
    const b = req.body as { priority_properties: unknown; max_response_tokens: unknown };
    expect(b.priority_properties).toEqual(['title', 'name']);
    expect(b.max_response_tokens).toBe(800);
  });

  it('findSimilar -> body carries priority_properties as a JSON array', async () => {
    const req = await capture({ results: [], count: 0 }, (db) =>
      db.findSimilar({ propertyName: 'embedding', embedding: [0.1, 0.2], ...budget }),
    );
    expect(req.method).toBe('POST');
    expect((req.body as { priority_properties: unknown }).priority_properties).toEqual([
      'title',
      'name',
    ]);
  });
});
