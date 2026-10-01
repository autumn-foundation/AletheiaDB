/**
 * Example 1 — connect and probe.
 *
 * Construct a client and hit the health + stats surface. Run against a local
 * AletheiaDB HTTP server (see the server quickstart to start one).
 */
import { AletheiaClient } from '../src/index.js';

async function main(): Promise<void> {
  const db = new AletheiaClient({
    baseUrl: process.env['ALETHEIA_URL'] ?? 'http://localhost:8080',
    apiKey: process.env['ALETHEIA_API_KEY'],
  });

  const health = await db.status();
  console.log('server status:', health.status);

  const nodeCount = await db.countNodes();
  console.log('nodes in graph:', nodeCount.count);
}

main().catch((err: unknown) => {
  console.error('connect example failed:', err);
  process.exitCode = 1;
});
