/**
 * Example 4 — semantic (vector) search.
 *
 * Enable an HNSW vector index on a property, then run k-NN `findSimilar` and a
 * combined graph + vector + temporal `hybridQuery`. These wrap the now-live
 * autumn-server routes (`/vector/indexes`, `/find_similar`, `/hybrid_query`).
 * Type-clean under `strict: true`.
 */
import { AletheiaClient } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  // 1. Enable a 3-dim cosine index on the `embedding` property.
  await db.enableVectorIndex({
    propertyName: 'embedding',
    dimensions: 3,
    distanceMetric: 'cosine',
  });

  // 2. k-NN: the 5 nodes whose `embedding` is closest to the query vector.
  const similar = await db.findSimilar({
    propertyName: 'embedding',
    embedding: [0.1, 0.2, 0.3],
    k: 5,
  });
  for (const { node, score } of similar.results) {
    console.log(`similar node ${node.id} (${node.label}) score=${score.toFixed(3)}`);
  }

  // 3. Hybrid: graph traversal + vector ranking + temporal filter in one call.
  const hybrid = await db.hybridQuery({
    startNodeId: 1,
    traverseEdge: 'KNOWS',
    vectorProperty: 'embedding',
    queryEmbedding: [0.1, 0.2, 0.3],
    topK: 10,
  });
  console.log(`hybrid returned ${hybrid.results.length} result(s)`);

  // 4. Inspect the active vector indexes.
  const indexes = await db.listVectorIndexes();
  console.log(`active vector indexes: ${indexes.count}`);
}

main().catch((err: unknown) => {
  console.error('semantic search example failed:', err);
  process.exitCode = 1;
});
