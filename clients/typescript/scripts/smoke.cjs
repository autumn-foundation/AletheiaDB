// CJS entry-point smoke test: require the built package and assert key exports.
const { AletheiaClient, NotFoundError, toWireTime } = require('../dist/index.cjs');

if (typeof AletheiaClient !== 'function') {
  throw new Error('CJS smoke: AletheiaClient is not a constructor');
}
if (typeof NotFoundError !== 'function') {
  throw new Error('CJS smoke: NotFoundError is not exported');
}
if (typeof toWireTime !== 'function') {
  throw new Error('CJS smoke: toWireTime is not exported');
}
const db = new AletheiaClient({ baseUrl: 'http://localhost:9999', apiKey: 'k' });
if (!db) throw new Error('CJS smoke: failed to construct client');
console.log('CJS smoke OK');
