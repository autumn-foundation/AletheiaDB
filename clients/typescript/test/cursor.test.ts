import { describe, expect, it } from 'vitest';
import { AletheiaClient, type NodeListResponse } from '../src/index.js';
import { mockFetch, nodeFixture } from './fixtures.js';

describe('cursor continuation (#3360)', () => {
  it('round-trips: use_cursor:true returns a cursor; continuation passes only cursor', async () => {
    const page1: NodeListResponse = {
      nodes: [nodeFixture(1, 'Person', { name: 'A' })] as unknown as NodeListResponse['nodes'],
      count: 1,
      has_more: true,
      cursor: 'CURSOR_TOKEN_1',
      snapshot_valid_time: '2024-01-01T00:00:00Z',
      snapshot_transaction_time: '2024-01-01T00:00:00Z',
      paging: 'cursor',
    };
    const page2: NodeListResponse = {
      nodes: [nodeFixture(2, 'Person', { name: 'B' })] as unknown as NodeListResponse['nodes'],
      count: 1,
      has_more: false,
      snapshot_valid_time: '2024-01-01T00:00:00Z',
      snapshot_transaction_time: '2024-01-01T00:00:00Z',
      paging: 'cursor',
    };
    const { fetch, calls } = mockFetch((_req, i) => ({ body: i === 0 ? page1 : page2 }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });

    // First page: request a cursor.
    const first = await db.listNodes({ label: 'Person', useCursor: true });
    expect(calls[0]!.query.get('use_cursor')).toBe('true');
    expect(first.cursor).toBe('CURSOR_TOKEN_1');
    expect(first.has_more).toBe(true);
    // Typed snapshot coordinate surfaced.
    expect(first.snapshot_valid_time).toBe('2024-01-01T00:00:00Z');

    // Continuation: pass ONLY the cursor.
    const second = await db.listNodes({ cursor: first.cursor });
    const q = calls[1]!.query;
    expect(q.get('cursor')).toBe('CURSOR_TOKEN_1');
    expect(q.has('label')).toBe(false);
    expect(q.has('use_cursor')).toBe(false);
    expect(second.has_more).toBe(false);
  });

  it('cursor round-trips on the adjacency reads too', async () => {
    const { fetch, calls } = mockFetch((_req, i) => ({
      body:
        i === 0
          ? { edges: [], count: 0, has_more: true, cursor: 'ADJ_1', paging: 'cursor' }
          : { edges: [], count: 0, has_more: false, paging: 'cursor' },
    }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const first = await db.getOutgoingEdges(5, { useCursor: true });
    expect(calls[0]!.query.get('use_cursor')).toBe('true');
    expect(first.cursor).toBe('ADJ_1');
    await db.getOutgoingEdges(5, { cursor: first.cursor });
    expect(calls[1]!.query.get('cursor')).toBe('ADJ_1');
  });
});
