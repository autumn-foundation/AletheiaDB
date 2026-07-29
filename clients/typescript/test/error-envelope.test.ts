/**
 * The HTTP error **envelope shape** contract (Issues #3234 / #3629 / #3679).
 *
 * Since #3629 the server renders every HTTP error through
 * `AletheiaHttpError::into_response_with_trace` (`src/http/error.rs`) as the
 * nested envelope
 *
 * ```json
 * { "error": { "code", "message", "retriable", "details"? }, "trace_id"? }
 * ```
 *
 * byte-shape-identical to the MCP surface, with `trace_id` a **top-level**
 * sibling of `error`. The legacy flat `{ success: false, error, code, … }` body
 * was removed server-side.
 *
 * This file locks three things the fixture refresh is supposed to guarantee:
 *
 *  1. the canonical {@link httpError} fixture models the *current* wire shape —
 *     no `success` key, no top-level `error` string, `trace_id` outside `error`;
 *  2. the nested envelope normalizes correctly across the whole status/code
 *     vocabulary the server can emit, `details` and `trace_id` intact;
 *  3. the retained legacy-flat parse branch normalizes to an error
 *     **observably identical** to its nested twin — which is what makes
 *     swapping the fixtures a pure wire-shape refresh rather than a behavior
 *     change.
 */

import { describe, expect, it } from 'vitest';
import {
  AletheiaClient,
  ConflictError,
  InternalError,
  InvalidArgumentError,
  NotFoundError,
  PermissionDeniedError,
  ResourceExhaustedError,
  UnauthenticatedError,
  UnavailableError,
  type AletheiaError,
} from '../src/index.js';
import { httpError, legacyFlatHttpError, mockFetch, type CannedResponse } from './fixtures.js';

/** Drive one canned error response through a real client call and return the thrown error. */
async function thrown(canned: CannedResponse): Promise<AletheiaError> {
  const { fetch } = mockFetch(() => canned);
  const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
  return (await db.listNodes().catch((e: unknown) => e)) as AletheiaError;
}

/** The observable normalization of an error — what a caller can actually branch on. */
function normalized(e: AletheiaError): Record<string, unknown> {
  return {
    name: e.name,
    code: e.code,
    message: e.message,
    retriable: e.retriable,
    details: e.details,
    httpStatus: e.httpStatus,
    traceId: e.traceId,
  };
}

describe('the canonical error fixture models the nested #3629 wire shape', () => {
  it('emits { error: { … } } with no legacy flat keys', () => {
    const body = httpError(404, 'no such node', 'NOT_FOUND', false).body as Record<
      string,
      unknown
    >;
    // No remnant of the removed flat envelope.
    expect(body).not.toHaveProperty('success');
    expect(body).not.toHaveProperty('code');
    expect(body).not.toHaveProperty('retriable');
    expect(body).not.toHaveProperty('details');
    // The error is a nested OBJECT, never a top-level string.
    expect(typeof body['error']).toBe('object');
    expect(body['error']).toEqual({ code: 'NOT_FOUND', message: 'no such node', retriable: false });
  });

  it('places trace_id as a top-level sibling of error, never inside it', () => {
    const body = httpError(500, 'boom', 'INTERNAL', false, undefined, 'trace-abc').body as Record<
      string,
      unknown
    >;
    expect(body['trace_id']).toBe('trace-abc');
    expect(body['error']).not.toHaveProperty('trace_id');
  });
});

describe('nested envelope normalization across the server status vocabulary', () => {
  // (status, code, server-stated retriable, expected SDK error class) — mirrors
  // `AletheiaHttpError::code_str` / `status` / `retriable` in src/http/error.rs.
  const cases: Array<[number, string, boolean, new (...a: never[]) => AletheiaError]> = [
    [400, 'INVALID_ARGUMENT', false, InvalidArgumentError],
    [401, 'UNAUTHENTICATED', false, UnauthenticatedError],
    [403, 'PERMISSION_DENIED', false, PermissionDeniedError],
    [404, 'NOT_FOUND', false, NotFoundError],
    [409, 'CONFLICT', true, ConflictError],
    [413, 'RESOURCE_EXHAUSTED', false, ResourceExhaustedError],
    [422, 'RESOURCE_EXHAUSTED', false, ResourceExhaustedError],
    [429, 'RESOURCE_EXHAUSTED', true, ResourceExhaustedError],
    [500, 'INTERNAL', false, InternalError],
    [503, 'UNAVAILABLE', true, UnavailableError],
  ];

  for (const [status, code, retriable, Klass] of cases) {
    it(`${status} ${code} -> ${Klass.name} (retriable=${retriable}), details + trace_id intact`, async () => {
      const err = await thrown(
        httpError(status, `msg ${code}`, code, retriable, { k: 'v' }, `trace-${status}`),
      );
      expect(err).toBeInstanceOf(Klass);
      expect(err.code).toBe(code);
      expect(err.message).toBe(`msg ${code}`);
      expect(err.retriable).toBe(retriable);
      expect(err.httpStatus).toBe(status);
      expect(err.details).toEqual({ k: 'v' });
      expect(err.traceId).toBe(`trace-${status}`);
    });
  }

  it('a server-stated retriable=false on a 429 WINS over the status-aware default', async () => {
    const err = await thrown(httpError(429, 'byte cap', 'RESOURCE_EXHAUSTED', false));
    expect(err.retriable).toBe(false);
  });

  it('a 429 omitting retriable falls back to the status-aware default (retriable)', async () => {
    const err = await thrown(httpError(429, 'timed out', 'RESOURCE_EXHAUSTED'));
    expect(err.retriable).toBe(true);
  });

  it('a 413 omitting retriable stays non-retriable (byte cap, not a timeout)', async () => {
    const err = await thrown(httpError(413, 'byte cap', 'RESOURCE_EXHAUSTED'));
    expect(err.retriable).toBe(false);
  });
});

describe('legacy flat envelope still normalizes IDENTICALLY (back-compat, pre-#3629 servers)', () => {
  const cases: Array<[number, string, string, boolean | undefined]> = [
    [404, 'no such node', 'NOT_FOUND', false],
    [409, 'write conflict', 'CONFLICT', true],
    [403, 'forbidden', 'PERMISSION_DENIED', false],
    [503, 'busy', 'UNAVAILABLE', true],
    // Retriable omitted on both sides: both must reach the same default.
    [429, 'slow down', 'RESOURCE_EXHAUSTED', undefined],
    [500, 'boom', 'INTERNAL', undefined],
  ];

  for (const [status, message, code, retriable] of cases) {
    it(`${status} ${code}: flat and nested produce the same AletheiaError`, async () => {
      const details = { k: 'v' };
      const traceId = `trace-${status}`;
      const nested = await thrown(httpError(status, message, code, retriable, details, traceId));
      const flat = await thrown(
        legacyFlatHttpError(status, message, code, retriable, details, traceId),
      );
      expect(normalized(flat)).toEqual(normalized(nested));
    });
  }

  it('a flat body with no code falls back to the status-derived code', async () => {
    const err = await thrown(legacyFlatHttpError(404, 'gone'));
    expect(err).toBeInstanceOf(NotFoundError);
    expect(err.message).toBe('gone');
  });
});
