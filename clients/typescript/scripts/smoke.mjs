// ESM entry-point smoke test: import the built package and assert key exports.
import { AletheiaClient, NotFoundError, toWireTime } from '../dist/index.js';

if (typeof AletheiaClient !== 'function') {
  throw new Error('ESM smoke: AletheiaClient is not a constructor');
}
if (typeof NotFoundError !== 'function') {
  throw new Error('ESM smoke: NotFoundError is not exported');
}
if (typeof toWireTime !== 'function') {
  throw new Error('ESM smoke: toWireTime is not exported');
}
// Exercise a no-network code path.
const db = new AletheiaClient({ baseUrl: 'http://localhost:9999', apiKey: 'k' });
if (!db) throw new Error('ESM smoke: failed to construct client');
console.log('ESM smoke OK');
