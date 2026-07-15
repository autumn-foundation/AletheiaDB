import { describe, expect, it } from 'vitest';
import {
  AletheiaClient,
  AletheiaError,
  ConflictError,
  ConstraintViolationError,
  FailedPreconditionError,
  InvalidArgumentError,
  NotFoundError,
} from '../src/index.js';
import { mcpError, mockFetch, httpError } from './fixtures.js';

function client(handler: Parameters<typeof mockFetch>[0]) {
  const { fetch, calls } = mockFetch(handler);
  return { db: new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch }), calls };
}

describe('structured error mapping (#3234)', () => {
  it('maps NOT_FOUND (in-band, HTTP 200) to NotFoundError, non-retriable', async () => {
    const { db } = client(() => mcpError('NOT_FOUND', 'no such node', false));
    await expect(db.getNode(999)).rejects.toBeInstanceOf(NotFoundError);
    await db.getNode(1).catch((e: unknown) => {
      expect(e).toBeInstanceOf(NotFoundError);
      expect((e as AletheiaError).retriable).toBe(false);
      expect((e as AletheiaError).code).toBe('NOT_FOUND');
    });
  });

  it('maps INVALID_ARGUMENT to InvalidArgumentError', async () => {
    const { db } = client(() => mcpError('INVALID_ARGUMENT', 'bad id', false));
    await expect(db.getNode(1)).rejects.toBeInstanceOf(InvalidArgumentError);
  });

  it('maps CONFLICT to ConflictError with retriable true by default', async () => {
    const { db } = client(() => mcpError('CONFLICT', 'write conflict', true));
    const err = await db.getNode(1).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ConflictError);
    expect((err as AletheiaError).retriable).toBe(true);
  });

  it('carries details for FAILED_PRECONDITION (DETACH refusal #3209)', async () => {
    const { db } = client(() =>
      mcpError('FAILED_PRECONDITION', 'has edges', false, { connected_edges: 3 }),
    );
    const err = await db.deleteNode({ nodeId: 1 }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(FailedPreconditionError);
    expect((err as AletheiaError).details).toEqual({ connected_edges: 3 });
  });

  it('carries details for CONSTRAINT_VIOLATION (unique)', async () => {
    const { db } = client(() =>
      mcpError('CONSTRAINT_VIOLATION', 'unique', false, { existing_node_id: 7 }),
    );
    const err = await db.createNode({ label: 'Person' }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ConstraintViolationError);
    expect((err as AletheiaError).details).toEqual({ existing_node_id: 7 });
  });

  it('maps an unknown code to the base AletheiaError with retriable === false', async () => {
    const { db } = client(() => mcpError('SOME_FUTURE_CODE', 'weird', true));
    const err = await db.getNode(1).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AletheiaError);
    expect(err).not.toBeInstanceOf(NotFoundError);
    // Forward-compat rule: unknown codes are never treated as retriable.
    expect((err as AletheiaError).retriable).toBe(false);
    expect((err as AletheiaError).code).toBe('SOME_FUTURE_CODE');
  });

  it('honors an HTTP-envelope error on a non-2xx (RESOURCE_EXHAUSTED 413)', async () => {
    const { db } = client(() =>
      httpError(413, 'row cap', 'RESOURCE_EXHAUSTED', false, { dimension: 'result_rows' }),
    );
    const err = await db.listNodes().catch((e: unknown) => e);
    expect((err as AletheiaError).code).toBe('RESOURCE_EXHAUSTED');
    expect((err as AletheiaError).httpStatus).toBe(413);
  });
});
