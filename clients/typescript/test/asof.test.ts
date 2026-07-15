import { describe, expect, it } from 'vitest';
import { AletheiaClient, toWireTime } from '../src/index.js';
import { mockFetch } from './fixtures.js';

const traverseBody = { results: [], count: 0, has_more: false };

function traverseClient() {
  const { fetch, calls } = mockFetch(() => ({ body: traverseBody }));
  return { db: new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch }), calls };
}

describe('asOf temporal ergonomics (#3225)', () => {
  const vt = '2024-01-01T00:00:00Z';
  const tt = '2024-06-01T00:00:00Z';

  it('injects BOTH as_of_valid_time and as_of_transaction_time on traverse', async () => {
    const { db, calls } = traverseClient();
    await db.asOf({ validTime: vt, transactionTime: tt }).traverse({
      startNodeId: 1,
      edgeLabel: 'KNOWS',
    });
    const q = calls[0]!.query;
    expect(q.get('as_of_valid_time')).toBe(String(toWireTime(vt)));
    expect(q.get('as_of_transaction_time')).toBe(String(toWireTime(tt)));
    expect(q.get('start_node_id')).toBe('1');
    expect(q.get('edge_label')).toBe('KNOWS');
  });

  it('valid-time only: sends as_of_valid_time, omits as_of_transaction_time', async () => {
    const { db, calls } = traverseClient();
    await db.asOf({ validTime: vt }).traverse({ startNodeId: 1, edgeLabel: 'KNOWS' });
    const q = calls[0]!.query;
    expect(q.get('as_of_valid_time')).toBe(String(toWireTime(vt)));
    expect(q.has('as_of_transaction_time')).toBe(false);
  });

  it('transaction-time only: sends as_of_transaction_time, omits as_of_valid_time', async () => {
    const { db, calls } = traverseClient();
    await db.asOf({ transactionTime: tt }).traverse({ startNodeId: 1, edgeLabel: 'KNOWS' });
    const q = calls[0]!.query;
    expect(q.get('as_of_transaction_time')).toBe(String(toWireTime(tt)));
    expect(q.has('as_of_valid_time')).toBe(false);
  });

  it('explicit per-call coordinate wins over the view coordinate', async () => {
    const { db, calls } = traverseClient();
    const override = '2020-01-01T00:00:00Z';
    await db
      .asOf({ validTime: vt })
      .traverse({ startNodeId: 1, edgeLabel: 'KNOWS', asOfValidTime: override });
    expect(calls[0]!.query.get('as_of_valid_time')).toBe(String(toWireTime(override)));
  });

  it('a plain traverse (no asOf) sends neither coordinate', async () => {
    const { db, calls } = traverseClient();
    await db.traverse({ startNodeId: 1, edgeLabel: 'KNOWS' });
    const q = calls[0]!.query;
    expect(q.has('as_of_valid_time')).toBe(false);
    expect(q.has('as_of_transaction_time')).toBe(false);
  });
});
