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
 * Every test here drives a **real client call** through `parseHttpError` /
 * `parseMcpError` and asserts the **absolute** normalized result — never merely
 * that two code paths agree with each other. (An equality-only assertion over a
 * shared implementation is satisfied by a uniformly broken one: gutting
 * `parseHttpError` to return a constant keeps every such comparison green.)
 * The flat-vs-nested equivalence below is therefore asserted as
 * "both equal this **stated** value", with the cross-shape equality as a
 * secondary check.
 */

import { describe, expect, it } from 'vitest';
import type { AletheiaError } from '../src/index.js';
import {
  AletheiaClient,
  ConflictError,
  ConstraintViolationError,
  FailedPreconditionError,
  InternalError,
  InvalidArgumentError,
  NotFoundError,
  PermissionDeniedError,
  ResourceExhaustedError,
  UnauthenticatedError,
  UnavailableError,
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

// ─────────────────────────────────────────────────────────────────────────────
// Fixture-helper guard. NOTE: these two assert on the *test helper's* output and
// deliberately exercise no SDK code — they exist so the canonical fixture can
// never drift back toward the removed flat shape, which is what would silently
// rot every other test in the repo. They are not a guard on the product.
// ─────────────────────────────────────────────────────────────────────────────
describe('the canonical error fixture models the nested #3629 wire shape (helper guard)', () => {
  it('emits { error: { … } } with no legacy flat keys', () => {
    const body = httpError(404, 'no such node', 'NOT_FOUND', false).body as Record<
      string,
      unknown
    >;
    expect(body).not.toHaveProperty('success');
    expect(body).not.toHaveProperty('code');
    expect(body).not.toHaveProperty('retriable');
    expect(body).not.toHaveProperty('details');
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
  // `AletheiaHttpError::status()` / `code_str()` / `retriable()` in
  // src/http/error.rs. 412/409 both arrive as `Structured` variants, which is
  // why each status can carry more than one code.
  const cases: Array<[number, string, boolean, new (...a: never[]) => AletheiaError]> = [
    [400, 'INVALID_ARGUMENT', false, InvalidArgumentError],
    [401, 'UNAUTHENTICATED', false, UnauthenticatedError],
    [403, 'PERMISSION_DENIED', false, PermissionDeniedError],
    [404, 'NOT_FOUND', false, NotFoundError],
    [409, 'CONFLICT', true, ConflictError],
    [409, 'CONSTRAINT_VIOLATION', false, ConstraintViolationError],
    [412, 'FAILED_PRECONDITION', false, FailedPreconditionError],
    [413, 'RESOURCE_EXHAUSTED', false, ResourceExhaustedError],
    [422, 'INVALID_ARGUMENT', false, InvalidArgumentError],
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
});

// ─────────────────────────────────────────────────────────────────────────────
// The status -> code fallback, exercised with bodies that carry NO code. Without
// these, `statusToCode` is almost entirely unpinned: rewriting it to return
// 'INTERNAL' for every status breaks only the bare-401 auth test.
// ─────────────────────────────────────────────────────────────────────────────
describe('statusToCode fallback — a body carrying no code at all', () => {
  const cases: Array<[number, string, boolean, new (...a: never[]) => AletheiaError]> = [
    [400, 'INVALID_ARGUMENT', false, InvalidArgumentError],
    [401, 'UNAUTHENTICATED', false, UnauthenticatedError],
    [403, 'PERMISSION_DENIED', false, PermissionDeniedError],
    [404, 'NOT_FOUND', false, NotFoundError],
    [409, 'CONFLICT', true, ConflictError],
    // 412 is the server's FAILED_PRECONDITION status (read-only replica,
    // non-conforming constraint enable, transaction validation, …).
    [412, 'FAILED_PRECONDITION', false, FailedPreconditionError],
    [413, 'RESOURCE_EXHAUSTED', false, ResourceExhaustedError],
    // 422 is emitted by exactly one server variant, `InvalidLimitOverride`,
    // whose code_str() is INVALID_ARGUMENT — NOT a resource cap.
    [422, 'INVALID_ARGUMENT', false, InvalidArgumentError],
    [429, 'RESOURCE_EXHAUSTED', true, ResourceExhaustedError],
    [500, 'INTERNAL', false, InternalError],
    [502, 'INTERNAL', false, InternalError],
    [503, 'UNAVAILABLE', true, UnavailableError],
  ];

  for (const [status, code, retriable, Klass] of cases) {
    it(`bare ${status} -> ${code} (${Klass.name}), retriable=${retriable}`, async () => {
      const err = await thrown({ status, raw: '' });
      expect(err).toBeInstanceOf(Klass);
      expect(err.code).toBe(code);
      expect(err.retriable).toBe(retriable);
      expect(err.httpStatus).toBe(status);
    });
  }
});

describe('retriable defaults when the envelope omits the flag', () => {
  it('a server-stated retriable=false on a 429 WINS over the status-aware default', async () => {
    // This is the server's write-class wall-clock timeout and its tenant-quota
    // breach: both are 429 with an explicit retriable:false, because the write
    // may already have committed. The stated flag must be honored verbatim.
    const err = await thrown(httpError(429, 'write timed out', 'RESOURCE_EXHAUSTED', false));
    expect(err).toBeInstanceOf(ResourceExhaustedError);
    expect(err.code).toBe('RESOURCE_EXHAUSTED');
    expect(err.retriable).toBe(false);
  });

  it('a 429 RESOURCE_EXHAUSTED omitting retriable falls back to retriable', async () => {
    const err = await thrown(httpError(429, 'timed out', 'RESOURCE_EXHAUSTED'));
    expect(err).toBeInstanceOf(ResourceExhaustedError);
    expect(err.retriable).toBe(true);
  });

  it('a 413 RESOURCE_EXHAUSTED omitting retriable stays non-retriable (a cap, not a timeout)', async () => {
    const err = await thrown(httpError(413, 'byte cap', 'RESOURCE_EXHAUSTED'));
    expect(err).toBeInstanceOf(ResourceExhaustedError);
    expect(err.code).toBe('RESOURCE_EXHAUSTED');
    expect(err.retriable).toBe(false);
  });

  // The status must never override a caller-fault code. #3234: retriable is
  // "always false for caller-fault classes".
  const callerFault = [
    ['NOT_FOUND', NotFoundError],
    ['INVALID_ARGUMENT', InvalidArgumentError],
    ['CONSTRAINT_VIOLATION', ConstraintViolationError],
    ['FAILED_PRECONDITION', FailedPreconditionError],
    ['PERMISSION_DENIED', PermissionDeniedError],
  ] as const;

  for (const [code, Klass] of callerFault) {
    it(`a 429 carrying ${code} stays NON-retriable`, async () => {
      const err = await thrown(httpError(429, 'weird', code));
      expect(err).toBeInstanceOf(Klass);
      expect(err.code).toBe(code);
      expect(err.retriable).toBe(false);
    });
  }

  it('an in-band RESOURCE_EXHAUSTED (HTTP 200) has no status signal -> non-retriable', async () => {
    const err = await thrown({ status: 200, body: { error: { code: 'RESOURCE_EXHAUSTED', message: 'cap' } } });
    expect(err.retriable).toBe(false);
    // In-band errors ride a 200 and the SDK records that status verbatim.
    expect(err.httpStatus).toBe(200);
  });
});

describe('legacy flat envelope normalizes to the SAME stated result (back-compat, pre-#3629)', () => {
  // Each row states the ABSOLUTE expected normalization. Both shapes are checked
  // against it independently, so a uniformly-broken parser fails both — then the
  // cross-shape equality is asserted as a secondary check.
  const cases: Array<{
    status: number;
    message: string;
    code: string;
    retriable: boolean | undefined;
    expected: { klass: new (...a: never[]) => AletheiaError; retriable: boolean };
  }> = [
    { status: 404, message: 'no such node', code: 'NOT_FOUND', retriable: false, expected: { klass: NotFoundError, retriable: false } },
    { status: 409, message: 'write conflict', code: 'CONFLICT', retriable: true, expected: { klass: ConflictError, retriable: true } },
    { status: 403, message: 'forbidden', code: 'PERMISSION_DENIED', retriable: false, expected: { klass: PermissionDeniedError, retriable: false } },
    { status: 503, message: 'busy', code: 'UNAVAILABLE', retriable: true, expected: { klass: UnavailableError, retriable: true } },
    // Flag omitted on both sides: both must reach the same DEFAULT.
    { status: 429, message: 'slow down', code: 'RESOURCE_EXHAUSTED', retriable: undefined, expected: { klass: ResourceExhaustedError, retriable: true } },
    { status: 413, message: 'too big', code: 'RESOURCE_EXHAUSTED', retriable: undefined, expected: { klass: ResourceExhaustedError, retriable: false } },
    { status: 500, message: 'boom', code: 'INTERNAL', retriable: undefined, expected: { klass: InternalError, retriable: false } },
  ];

  for (const { status, message, code, retriable, expected } of cases) {
    it(`${status} ${code}: both shapes -> ${expected.klass.name}(retriable=${expected.retriable})`, async () => {
      const details = { k: 'v' };
      const traceId = `trace-${status}`;
      const nested = await thrown(httpError(status, message, code, retriable, details, traceId));
      const flat = await thrown(
        legacyFlatHttpError(status, message, code, retriable, details, traceId),
      );

      // Absolute assertions — these fail on a uniformly-broken parser.
      for (const err of [nested, flat]) {
        expect(err).toBeInstanceOf(expected.klass);
        expect(err.code).toBe(code);
        expect(err.message).toBe(message);
        expect(err.retriable).toBe(expected.retriable);
        expect(err.details).toEqual(details);
        expect(err.httpStatus).toBe(status);
        expect(err.traceId).toBe(traceId);
      }
      // Secondary: nothing observable differs between the shapes.
      expect(normalized(flat)).toEqual(normalized(nested));
    });
  }

  it('a message-less body falls back to the code in BOTH shapes', async () => {
    const nested = await thrown({ status: 404, body: { error: { code: 'NOT_FOUND' } } });
    const flat = await thrown({ status: 404, body: { success: false, code: 'NOT_FOUND' } });
    expect(nested.message).toBe('NOT_FOUND');
    expect(flat.message).toBe('NOT_FOUND');
    expect(normalized(flat)).toEqual(normalized(nested));
  });

  it('a flat body with no code falls back to the status-derived code', async () => {
    const err = await thrown(legacyFlatHttpError(404, 'gone'));
    expect(err).toBeInstanceOf(NotFoundError);
    expect(err.message).toBe('gone');
  });

  it('a bare { error: "<string>" } body (no success key) is still typed by status', async () => {
    const err = await thrown({ status: 409, body: { error: 'plain string reason' } });
    expect(err).toBeInstanceOf(ConflictError);
    expect(err.message).toBe('plain string reason');
  });
});

describe('malformed bodies never lose the server’s explanation or corrupt typed fields', () => {
  it('a nested body whose code is missing still surfaces error.message', async () => {
    const err = await thrown({ status: 404, body: { error: { message: 'Node 42 not found' } } });
    expect(err).toBeInstanceOf(NotFoundError);
    expect(err.message).toBe('Node 42 not found');
  });

  it('a nested body whose code is not a string still surfaces error.message', async () => {
    const err = await thrown({ status: 500, body: { error: { code: 500, message: 'disk on fire' } } });
    expect(err).toBeInstanceOf(InternalError);
    expect(err.message).toBe('disk on fire');
  });

  it('a non-boolean retriable is ignored, not forwarded as a string', async () => {
    const err = await thrown({
      status: 404,
      body: { error: { code: 'NOT_FOUND', message: 'x', retriable: 'true' } },
    });
    expect(err.retriable).toBe(false);
    expect(typeof err.retriable).toBe('boolean');
  });

  it('a non-object details is ignored, not forwarded as a scalar', async () => {
    const err = await thrown({
      status: 404,
      body: { error: { code: 'NOT_FOUND', message: 'x', details: 'oops' } },
    });
    expect(err.details).toBeUndefined();
  });

  it('a non-string trace_id is ignored in BOTH shapes', async () => {
    const nested = await thrown({
      status: 500,
      body: { error: { code: 'INTERNAL', message: 'x' }, trace_id: 123 },
    });
    const flat = await thrown({
      status: 500,
      body: { success: false, error: 'x', code: 'INTERNAL', trace_id: 123 },
    });
    expect(nested.traceId).toBeUndefined();
    expect(flat.traceId).toBeUndefined();
  });

  it('an empty body on a 500 still types by status', async () => {
    const err = await thrown({ status: 500, raw: '' });
    expect(err).toBeInstanceOf(InternalError);
    expect(err.message).toBe('HTTP 500');
  });
});

describe('the retry policy actually re-issues on an HTTP-status error (not just in-band)', () => {
  it('retries a 429 RESOURCE_EXHAUSTED end-to-end, then succeeds', async () => {
    const { fetch, calls } = mockFetch((_req, i) =>
      i < 2
        ? httpError(429, 'slow down', 'RESOURCE_EXHAUSTED', true)
        : { body: { nodes: [], count: 0 } },
    );
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      retry: {
        enabled: true,
        maxAttempts: 3,
        sleep: () => Promise.resolve(),
        random: () => 0.5,
      },
    });
    await db.listNodes();
    expect(calls.length).toBe(3);
  });

  it('NEVER retries a 429 the server marked retriable:false (a write may have committed)', async () => {
    const { fetch, calls } = mockFetch(() =>
      httpError(429, 'write timed out', 'RESOURCE_EXHAUSTED', false),
    );
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      retry: {
        enabled: true,
        maxAttempts: 5,
        sleep: () => Promise.resolve(),
        random: () => 0.5,
      },
    });
    await expect(db.createNode({ label: 'Person' })).rejects.toBeInstanceOf(ResourceExhaustedError);
    expect(calls.length).toBe(1);
  });
});
