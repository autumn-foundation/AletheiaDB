/**
 * Example 5 — read-only Cypher/AQL query, schema discovery, and stats.
 *
 * `query`, `getSchema`, `databaseStats`, and `temporalExtent` wrap the now-live
 * autumn-server routes (`/query`, `/schema`, `/database_stats`,
 * `/temporal_extent`). The `query` tool is read-only: mutating clauses are
 * rejected server-side before execution. Type-clean under `strict: true`.
 */
import { AletheiaClient } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  // 1. A single declarative, read-only statement with a $param binding.
  const result = await db.query({
    language: 'cypher',
    query: 'MATCH (n:Person {name: $name})-[:KNOWS]->(friend) RETURN friend',
    params: { name: 'Alice' },
    limit: 25,
  });
  console.log(`columns: ${result.columns.join(', ')} — ${result.row_count} row(s)`);

  // 2. Discover the graph's schema (labels, edge types, property keys).
  const schema = await db.getSchema();
  console.log('schema:', JSON.stringify(schema.node_labels));

  // 3. A holistic database snapshot and the queryable bi-temporal extent.
  const stats = await db.databaseStats();
  console.log('current:', JSON.stringify(stats.current));

  const extent = await db.temporalExtent({ byLabel: true });
  console.log('valid-time extent:', JSON.stringify(extent.valid_time));
}

main().catch((err: unknown) => {
  console.error('cypher query example failed:', err);
  process.exitCode = 1;
});
