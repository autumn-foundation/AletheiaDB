import { describe, expect, it } from 'vitest';
import {
  AletheiaClient,
  isElidedVector,
  isFullVector,
  type PropertyValue,
} from '../src/index.js';
import { mockFetch, nodeFixture } from './fixtures.js';

describe('vector elision (#3220)', () => {
  it('includeVectors:false (default) yields an elided descriptor distinct from number[]', async () => {
    const { fetch, calls } = mockFetch(() => ({
      body: nodeFixture(1, 'Document', {
        title: 'Intro',
        embedding: { type: 'vector', dim: 8, elided: true },
      }),
    }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const node = await db.getNode(1);

    // Default request does not ask for full vectors.
    expect(calls[0]!.query.has('include_vectors')).toBe(false);

    const embedding: PropertyValue = node.properties['embedding']!;
    expect(isElidedVector(embedding)).toBe(true);
    expect(isFullVector(embedding)).toBe(false);
    if (isElidedVector(embedding)) {
      // Narrowed to the descriptor — typed distinctly from data.
      expect(embedding.dim).toBe(8);
      expect(embedding.elided).toBe(true);
    }
  });

  it('includeVectors:true sends the flag and returns the full array', async () => {
    const full = [0, 0.25, 0.5, 0.75];
    const { fetch, calls } = mockFetch(() => ({
      body: nodeFixture(1, 'Document', { embedding: full }),
    }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const node = await db.getNode(1, { includeVectors: true });

    expect(calls[0]!.query.get('include_vectors')).toBe('true');
    const embedding: PropertyValue = node.properties['embedding']!;
    expect(isFullVector(embedding)).toBe(true);
    expect(isElidedVector(embedding)).toBe(false);
    if (isFullVector(embedding)) {
      expect(embedding).toEqual(full);
    }
  });
});
