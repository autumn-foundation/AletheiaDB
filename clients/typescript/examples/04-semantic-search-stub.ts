/**
 * Example 4 — semantic (vector) search.
 *
 * NOTE: `findSimilar` / `hybridQuery` wrap REST routes that are NOT merged
 * server-side yet, so they currently throw {@link NotImplementedError}. This
 * example is type-clean under `strict: true` and shows the intended shape;
 * it will work unchanged once the vector routes land (see COMPATIBILITY.md).
 * TODO(#3369-followup): wire when the vector/hybrid REST routes land (PR5–PR8).
 */
import { AletheiaClient, NotImplementedError } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  try {
    // Intended usage once merged: find nodes similar to a query embedding.
    await db.findSimilar();
  } catch (err: unknown) {
    if (err instanceof NotImplementedError) {
      console.log('semantic search not yet available over HTTP:', err.message);
      return;
    }
    throw err;
  }
}

main().catch((err: unknown) => {
  console.error('semantic search example failed:', err);
  process.exitCode = 1;
});
