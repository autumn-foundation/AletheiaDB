/**
 * Example 3 — AS OF traversal (#3225).
 *
 * "Who did Alice know on 2024-01-01?" — a bi-temporal, point-in-time traversal.
 * `asOf` injects `as_of_valid_time` / `as_of_transaction_time`; each dimension
 * is independent.
 */
import { AletheiaClient, isElidedVector } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  const [alice] = (await db.findNodesAtTime({
    label: 'Person',
    propertyKey: 'name',
    propertyValue: 'Alice',
    validTime: '2024-01-01T00:00:00Z',
  })).nodes;

  if (!alice) {
    console.log('no Alice as of 2024-01-01');
    return;
  }

  const asOf2024 = db.asOf({ validTime: '2024-01-01T00:00:00Z' });
  const friends = await asOf2024.traverse({
    startNodeId: alice.id,
    edgeLabel: 'KNOWS',
    depth: 2,
  });

  for (const row of friends.results) {
    const emb = row.node.properties['embedding'];
    const note = emb !== undefined && isElidedVector(emb) ? ' (embedding elided)' : '';
    console.log('knows:', row.node.properties['name'], note);
  }
  console.log('total reachable:', friends.count, 'has_more:', friends.has_more);
}

main().catch((err: unknown) => {
  console.error('asof traverse example failed:', err);
  process.exitCode = 1;
});
