import { describe, expect, it } from 'vitest';
import { AletheiaClient, NotImplementedError } from '../src/index.js';
import { mockFetch } from './fixtures.js';

describe('not-yet-merged endpoint stubs', () => {
  const { fetch } = mockFetch(() => ({ body: {} }));
  const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });

  const stubs: Array<[string, () => Promise<unknown>]> = [
    ['findSimilar', () => db.findSimilar()],
    ['hybridQuery', () => db.hybridQuery()],
    ['query', () => db.query()],
    ['enableVectorIndex', () => db.enableVectorIndex()],
    ['listVectorIndexes', () => db.listVectorIndexes()],
    ['getSchema', () => db.getSchema()],
    ['databaseStats', () => db.databaseStats()],
    ['temporalExtent', () => db.temporalExtent()],
    ['applyBatch', () => db.applyBatch()],
    ['lineageUpstream', () => db.lineageUpstream()],
    ['lineageDownstream', () => db.lineageDownstream()],
  ];

  for (const [name, call] of stubs) {
    it(`${name} throws NotImplementedError referencing the follow-up`, async () => {
      const err = await call().catch((e: unknown) => e);
      expect(err).toBeInstanceOf(NotImplementedError);
      expect((err as Error).message).toMatch(/not merged server-side/);
    });
  }
});
