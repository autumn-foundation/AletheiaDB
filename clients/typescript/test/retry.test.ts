import { describe, expect, it } from 'vitest';
import { AletheiaClient, ConflictError, NotFoundError } from '../src/index.js';
import { mcpError, mockFetch, nodeFixture } from './fixtures.js';

const noSleep = () => Promise.resolve();
const fixedRandom = () => 0.5;

describe('retry-with-backoff policy', () => {
  it('is OFF by default: a retriable CONFLICT is thrown after ONE attempt', async () => {
    const { fetch, calls } = mockFetch(() => mcpError('CONFLICT', 'conflict', true));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    await expect(db.getNode(1)).rejects.toBeInstanceOf(ConflictError);
    expect(calls.length).toBe(1);
  });

  it('retries a retriable CONFLICT when enabled, then succeeds', async () => {
    const { fetch, calls } = mockFetch((_req, i) => {
      if (i < 2) return mcpError('CONFLICT', 'conflict', true);
      return { body: nodeFixture(1, 'Person', { name: 'Alice' }) };
    });
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      retry: { enabled: true, maxAttempts: 3, sleep: noSleep, random: fixedRandom },
    });
    const node = await db.getNode(1);
    expect(node.label).toBe('Person');
    expect(calls.length).toBe(3);
  });

  it('NEVER retries a non-retriable NOT_FOUND, even with retries enabled', async () => {
    const { fetch, calls } = mockFetch(() => mcpError('NOT_FOUND', 'missing', false));
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      retry: { enabled: true, maxAttempts: 5, sleep: noSleep, random: fixedRandom },
    });
    await expect(db.getNode(1)).rejects.toBeInstanceOf(NotFoundError);
    expect(calls.length).toBe(1);
  });

  it('gives up after maxAttempts on a persistently retriable error', async () => {
    const { fetch, calls } = mockFetch(() => mcpError('UNAVAILABLE', 'busy', true));
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      retry: { enabled: true, maxAttempts: 4, sleep: noSleep, random: fixedRandom },
    });
    await expect(db.listNodes()).rejects.toMatchObject({ code: 'UNAVAILABLE' });
    expect(calls.length).toBe(4);
  });
});
