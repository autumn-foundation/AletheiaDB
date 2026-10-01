/**
 * Example 2 — CRUD with valid time (#3221).
 *
 * Create two people, relate them, then back-date when the relationship became
 * true in the real world. `validTime` accepts a Date, an ISO string, or
 * epoch-microseconds — all coerced to the same wire value.
 */
import { AletheiaClient } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  const alice = await db.createNode({
    label: 'Person',
    properties: { name: 'Alice' },
  });
  const bob = await db.createNode({
    label: 'Person',
    properties: { name: 'Bob' },
  });

  // The friendship became true on 2020-06-01 (a Date), even though we record it now.
  const edge = await db.createEdge({
    sourceId: alice.id,
    targetId: bob.id,
    label: 'KNOWS',
    properties: { context: 'university' },
    validTime: new Date('2020-06-01T00:00:00Z'),
    provenance: { source: 'address-book-import', confidence: 0.95 },
  });
  console.log('edge', edge.id, 'valid_from:', edge.temporal?.valid_from);

  // Update Alice's properties (records a new bi-temporal version).
  const updated = await db.updateNode({
    nodeId: alice.id,
    properties: { name: 'Alice', title: 'Dr' },
  });
  console.log('alice is_current:', updated.temporal?.is_current);
}

main().catch((err: unknown) => {
  console.error('crud example failed:', err);
  process.exitCode = 1;
});
