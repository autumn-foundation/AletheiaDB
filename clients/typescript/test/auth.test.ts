import { describe, expect, it } from 'vitest';
import {
  AletheiaClient,
  PermissionDeniedError,
  UnauthenticatedError,
  type AletheiaError,
} from '../src/index.js';
import { httpError, mockFetch, nodeFixture } from './fixtures.js';

describe('authentication headers + auth errors', () => {
  it('sends Authorization: Bearer by default', async () => {
    const { fetch, calls } = mockFetch(() => ({ body: nodeFixture(1, 'Person', {}) }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'secret-key', fetch });
    await db.getNode(1);
    expect(calls[0]!.headers['Authorization']).toBe('Bearer secret-key');
    expect(calls[0]!.headers['x-api-key']).toBeUndefined();
  });

  it('sends x-api-key when authScheme is x-api-key', async () => {
    const { fetch, calls } = mockFetch(() => ({ body: nodeFixture(1, 'Person', {}) }));
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'secret-key',
      authScheme: 'x-api-key',
      fetch,
    });
    await db.getNode(1);
    expect(calls[0]!.headers['x-api-key']).toBe('secret-key');
    expect(calls[0]!.headers['Authorization']).toBeUndefined();
  });

  it('maps HTTP 401 to UnauthenticatedError', async () => {
    const { fetch } = mockFetch(() => httpError(401, 'unauthorized', 'UNAUTHENTICATED', false));
    const db = new AletheiaClient({ baseUrl: 'http://x', fetch });
    const err = await db.status().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(UnauthenticatedError);
    expect((err as AletheiaError).retriable).toBe(false);
  });

  it('maps HTTP 403 to PermissionDeniedError with details.required_class', async () => {
    const { fetch } = mockFetch(() =>
      httpError(403, 'forbidden', 'PERMISSION_DENIED', false, {
        required_class: 'write',
        principal_role: 'reader',
      }),
    );
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'reader-key', fetch });
    const err = await db.createNode({ label: 'Person' }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(PermissionDeniedError);
    expect((err as AletheiaError).details).toMatchObject({ required_class: 'write' });
  });

  it('maps a bare 401 (no envelope body) to UnauthenticatedError via status', async () => {
    const { fetch } = mockFetch(() => ({ status: 401, raw: '' }));
    const db = new AletheiaClient({ baseUrl: 'http://x', fetch });
    await expect(db.status()).rejects.toBeInstanceOf(UnauthenticatedError);
  });
});
