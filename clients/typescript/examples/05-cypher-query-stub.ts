/**
 * Example 5 — read-only Cypher/AQL query.
 *
 * NOTE: `query` wraps the read-only Cypher/AQL REST route, which is NOT merged
 * server-side yet, so it currently throws {@link NotImplementedError}. This
 * example is type-clean under `strict: true` and will work unchanged once the
 * query route lands (see COMPATIBILITY.md).
 * TODO(#3369-followup): wire when the query REST route lands (PR5–PR8).
 */
import { AletheiaClient, NotImplementedError } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  try {
    // Intended usage once merged: a single declarative, read-only statement.
    await db.query();
  } catch (err: unknown) {
    if (err instanceof NotImplementedError) {
      console.log('Cypher/AQL query not yet available over HTTP:', err.message);
      return;
    }
    throw err;
  }
}

main().catch((err: unknown) => {
  console.error('cypher query example failed:', err);
  process.exitCode = 1;
});
