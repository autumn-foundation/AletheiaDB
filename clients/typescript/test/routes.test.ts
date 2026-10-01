import { describe, expect, it } from 'vitest';
import { AletheiaClient, toWireTime, type RequestSpec } from '../src/index.js';
import {
  edgeFixture,
  mockFetch,
  nodeFixture,
  type RecordedRequest,
} from './fixtures.js';

/** Capture the single request a call makes. */
async function capture(
  responseBody: unknown,
  run: (db: AletheiaClient) => Promise<unknown>,
): Promise<RecordedRequest> {
  const { fetch, calls } = mockFetch(() => ({ body: responseBody }));
  const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
  await run(db);
  return calls[0]!;
}

describe('route + wire param fidelity (mirrors *_tools.rs)', () => {
  it('createNode -> POST /nodes with label/properties/valid_time/provenance/derived_from', async () => {
    const req = await capture(nodeFixture(1, 'Person', { name: 'Alice' }), (db) =>
      db.createNode({
        label: 'Person',
        properties: { name: 'Alice' },
        validTime: '2024-01-15T10:00:00Z',
        provenance: { source: 'hr', confidence: 0.9 },
        derivedFrom: [{ entity_kind: 'node', id: 5, version: 2 }],
      }),
    );
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/nodes');
    expect(req.body).toMatchObject({
      label: 'Person',
      properties: { name: 'Alice' },
      valid_time: toWireTime('2024-01-15T10:00:00Z'),
      provenance: { source: 'hr', confidence: 0.9 },
      derived_from: [{ entity_kind: 'node', id: 5, version: 2 }],
    });
  });

  it('updateNode -> POST /nodes/update with node_id', async () => {
    const req = await capture(nodeFixture(1, 'Person', {}), (db) =>
      db.updateNode({ nodeId: 1, properties: { name: 'Bob' } }),
    );
    expect(req.path).toBe('/nodes/update');
    expect(req.body).toMatchObject({ node_id: 1, properties: { name: 'Bob' } });
  });

  it('deleteNode -> POST /nodes/delete with detach', async () => {
    const req = await capture({ deleted: true }, (db) =>
      db.deleteNode({ nodeId: 3, detach: true }),
    );
    expect(req.path).toBe('/nodes/delete');
    expect(req.body).toMatchObject({ node_id: 3, detach: true });
  });

  it('deleteNodeCascade -> POST /nodes/delete_cascade', async () => {
    const req = await capture({ deleted: true }, (db) => db.deleteNodeCascade({ nodeId: 3 }));
    expect(req.path).toBe('/nodes/delete_cascade');
    expect(req.body).toEqual({ node_id: 3 });
  });

  it('retractNode -> POST /nodes/retract with valid_time + detach', async () => {
    const req = await capture({ retracted: true }, (db) =>
      db.retractNode({ nodeId: 3, validTime: '2024-01-15T10:00:00Z', detach: true }),
    );
    expect(req.path).toBe('/nodes/retract');
    expect(req.body).toMatchObject({
      node_id: 3,
      valid_time: toWireTime('2024-01-15T10:00:00Z'),
      detach: true,
    });
  });

  it('findNodesAtTime -> POST /nodes/find_at_time with valid_time/transaction_time', async () => {
    const req = await capture({ nodes: [], count: 0 }, (db) =>
      db.findNodesAtTime({
        label: 'Person',
        propertyKey: 'name',
        propertyValue: 'Alice',
        validTime: '2024-01-01',
        transactionTime: '2024-06-01',
      }),
    );
    expect(req.path).toBe('/nodes/find_at_time');
    expect(req.body).toMatchObject({
      label: 'Person',
      property_key: 'name',
      property_value: 'Alice',
      valid_time: toWireTime('2024-01-01'),
      transaction_time: toWireTime('2024-06-01'),
    });
  });

  it('createEdge -> POST /edges with source_id/target_id/label', async () => {
    const req = await capture(edgeFixture(1, 'KNOWS', 1, 2), (db) =>
      db.createEdge({ sourceId: 1, targetId: 2, label: 'KNOWS', properties: { since: 2020 } }),
    );
    expect(req.path).toBe('/edges');
    expect(req.body).toMatchObject({
      source_id: 1,
      target_id: 2,
      label: 'KNOWS',
      properties: { since: 2020 },
    });
  });

  it('getOutgoingEdges -> GET /nodes/{id}/edges/outgoing', async () => {
    const req = await capture({ edges: [], count: 0 }, (db) =>
      db.getOutgoingEdges(7, { label: 'KNOWS' }),
    );
    expect(req.method).toBe('GET');
    expect(req.path).toBe('/nodes/7/edges/outgoing');
    expect(req.query.get('label')).toBe('KNOWS');
  });

  it('getIncomingEdges -> GET /nodes/{id}/edges/incoming', async () => {
    const req = await capture({ edges: [], count: 0 }, (db) => db.getIncomingEdges(7));
    expect(req.path).toBe('/nodes/7/edges/incoming');
  });

  it('getNodeAtTime -> POST /nodes/at_time', async () => {
    const req = await capture(nodeFixture(1, 'Person', {}), (db) =>
      db.getNodeAtTime({ nodeId: 1, validTime: '2024-01-01', transactionTime: '2024-06-01' }),
    );
    expect(req.path).toBe('/nodes/at_time');
    expect(req.body).toMatchObject({
      node_id: 1,
      valid_time: toWireTime('2024-01-01'),
      transaction_time: toWireTime('2024-06-01'),
    });
  });

  it('getEdgeAtTime -> POST /edges/at_time', async () => {
    const req = await capture(edgeFixture(1, 'KNOWS', 1, 2), (db) =>
      db.getEdgeAtTime({ edgeId: 1, validTime: '2024-01-01' }),
    );
    expect(req.path).toBe('/edges/at_time');
    expect(req.body).toMatchObject({ edge_id: 1, valid_time: toWireTime('2024-01-01') });
  });

  it('diffNodeVersions -> POST /nodes/diff with from_version/to_version', async () => {
    const req = await capture({ diff: {} }, (db) =>
      db.diffNodeVersions({ nodeId: 1, fromVersion: 2, toVersion: 5 }),
    );
    expect(req.path).toBe('/nodes/diff');
    expect(req.body).toEqual({ node_id: 1, from_version: 2, to_version: 5 });
  });

  it('listChanges -> POST /changes with tx_from/tx_to', async () => {
    const req = await capture({ changes: [] }, (db) =>
      db.listChanges({ txFrom: '2024-01-01', txTo: '2024-12-31' }),
    );
    expect(req.path).toBe('/changes');
    expect(req.body).toMatchObject({
      tx_from: toWireTime('2024-01-01'),
      tx_to: toWireTime('2024-12-31'),
    });
  });

  it('getNodeHistory -> GET /nodes/{id}/history', async () => {
    const req = await capture({ node_id: 1, results: [] }, (db) => db.getNodeHistory(1));
    expect(req.path).toBe('/nodes/1/history');
  });

  it('getEdgeHistory -> GET /edges/{id}/history', async () => {
    const req = await capture({ edge_id: 1, results: [] }, (db) => db.getEdgeHistory(1));
    expect(req.path).toBe('/edges/1/history');
  });

  it('countNodes -> GET /nodes/count', async () => {
    const req = await capture({ count: 42 }, (db) => db.countNodes());
    expect(req.path).toBe('/nodes/count');
  });

  it('status -> GET /status', async () => {
    const req = await capture({ status: 'healthy' }, (db) => db.status());
    expect(req.path).toBe('/status');
  });

  it('createKey -> POST /admin/keys and unwraps {success,data}', async () => {
    const { fetch, calls } = mockFetch(() => ({
      body: {
        success: true,
        data: { id: 'p1', name: 'ci', role: 'writer', key_prefix: 'aletheia_sk_x', created_at: 't', key: 'aletheia_sk_secret' },
      },
    }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'admin', fetch });
    const created = await db.createKey({ name: 'ci', role: 'writer' });
    expect(calls[0]!.path).toBe('/admin/keys');
    expect(created.key).toBe('aletheia_sk_secret');
    expect(created.role).toBe('writer');
  });

  it('revokeKey -> POST /admin/keys/revoke and unwraps data', async () => {
    const { fetch, calls } = mockFetch(() => ({
      body: { success: true, data: { revoked: true, id: 'p1' } },
    }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'admin', fetch });
    const res = await db.revokeKey({ id: 'p1' });
    expect(calls[0]!.path).toBe('/admin/keys/revoke');
    expect(res).toEqual({ revoked: true, id: 'p1' });
  });

  it('exposes RequestSpec as a public type', () => {
    const spec: RequestSpec = { method: 'GET', path: '/status' };
    expect(spec.path).toBe('/status');
  });
});
