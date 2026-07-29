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
