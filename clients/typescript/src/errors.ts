/**
 * Typed error hierarchy mapping AletheiaDB's structured error contract.
 *
 * Two envelope shapes reach this SDK, and both are normalized here:
 *
 * 1. **MCP-tool / structured errors (Issue #3234)** — returned in-band with an
 *    HTTP 200 as `{ error: { code, message, retriable, details? } }`. The node/
 *    edge/traverse routes forward these verbatim.
 * 2. **HTTP envelope errors (Issue #3629)** — real non-2xx responses now share
 *    the *exact same* nested shape, `{ error: { code, message, retriable,
 *    details? } }`, with `trace_id` (when present) a **top-level** sibling of
 *    `error`. The legacy flat `{ success: false, error, code?, … }` body has
 *    been removed server-side; the flat branch below is retained only for
 *    backward compatibility with older servers.
 *
 * Every error becomes an {@link AletheiaError} subclass keyed off `code`. The
 * `retriable` flag is preserved exactly and drives the opt-in retry policy;
 * an unknown code degrades to the base {@link AletheiaError} with
 * `retriable = false`.
 *
 * @packageDocumentation
 */

/**
 * The stable structured error-code vocabulary. Union of the MCP `code_enum`
 * and the HTTP `code_vocabulary`. Unknown codes are represented as the literal
 * string they arrived as; treat any code you don't recognize as non-retriable.
 */
export type AletheiaErrorCode =
  | 'NOT_FOUND'
  | 'INVALID_ARGUMENT'
  | 'CONSTRAINT_VIOLATION'
  | 'FAILED_PRECONDITION'
  | 'CONFLICT'
  | 'UNAVAILABLE'
  | 'INTERNAL'
  | 'UNAUTHENTICATED'
  | 'PERMISSION_DENIED'
  | 'RESOURCE_EXHAUSTED';

/** Structured metadata carried under `error.details`. Shape varies per code. */
export type ErrorDetails = Record<string, unknown>;

/** The normalized fields every AletheiaDB error exposes. */
export interface AletheiaErrorInit {
  code: string;
  message: string;
  retriable: boolean;
  details?: ErrorDetails | undefined;
  /** HTTP status, when the error originated from a non-2xx response. */
  httpStatus?: number | undefined;
  /** Trace id from the HTTP envelope, when observability is enabled server-side. */
  traceId?: string | undefined;
}

/**
 * Base class for every error surfaced by the SDK. Subclasses correspond 1:1 to
 * a known {@link AletheiaErrorCode}; an unrecognized code lands here directly
 * with `retriable = false`.
 */
export class AletheiaError extends Error {
  /** The structured error code (may be an unknown string for forward-compat). */
  readonly code: string;
  /** Whether the operation is safe to retry. Only ever `true` for transient classes. */
  readonly retriable: boolean;
  /** Per-code structured metadata, when present. */
  readonly details: ErrorDetails | undefined;
  /** Originating HTTP status, when applicable. */
  readonly httpStatus: number | undefined;
  /** Server trace id, when present. */
  readonly traceId: string | undefined;

  constructor(init: AletheiaErrorInit) {
    super(init.message);
    this.name = new.target.name;
    this.code = init.code;
    this.retriable = init.retriable;
    this.details = init.details;
    this.httpStatus = init.httpStatus;
    this.traceId = init.traceId;
    // Restore the prototype chain for extends-Error under downleveled targets.
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

/** `NOT_FOUND` — the entity/version does not exist. Non-retriable. */
export class NotFoundError extends AletheiaError {}
/** `INVALID_ARGUMENT` — caller-fault input (bad id, unparseable time, …). Non-retriable. */
export class InvalidArgumentError extends AletheiaError {}
/** `CONSTRAINT_VIOLATION` — a unique/constraint rule was violated. Non-retriable. */
export class ConstraintViolationError extends AletheiaError {}
/** `FAILED_PRECONDITION` — well-formed but the system isn't in the required state. Non-retriable. */
export class FailedPreconditionError extends AletheiaError {}
/** `CONFLICT` — a write/serialization conflict. Usually retriable. */
export class ConflictError extends AletheiaError {}
/** `UNAVAILABLE` — a transient outage / capacity guard. Retriable. */
export class UnavailableError extends AletheiaError {}
/** `INTERNAL` — an unexpected server-side failure. Non-retriable. */
export class InternalError extends AletheiaError {}
/** `UNAUTHENTICATED` — missing/invalid/revoked credential (HTTP 401). Non-retriable. */
export class UnauthenticatedError extends AletheiaError {}
/** `PERMISSION_DENIED` — authenticated but the role lacks the class (HTTP 403). Non-retriable. */
export class PermissionDeniedError extends AletheiaError {}
/** `RESOURCE_EXHAUSTED` — a per-query resource limit (rows/bytes/timeout). Sometimes retriable. */
export class ResourceExhaustedError extends AletheiaError {}

/** Thrown by SDK methods that wrap endpoints not yet merged server-side. */
export class NotImplementedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'NotImplementedError';
    Object.setPrototypeOf(this, NotImplementedError.prototype);
  }
}

const CODE_TO_CLASS: Record<AletheiaErrorCode, new (init: AletheiaErrorInit) => AletheiaError> = {
  NOT_FOUND: NotFoundError,
  INVALID_ARGUMENT: InvalidArgumentError,
  CONSTRAINT_VIOLATION: ConstraintViolationError,
  FAILED_PRECONDITION: FailedPreconditionError,
  CONFLICT: ConflictError,
  UNAVAILABLE: UnavailableError,
  INTERNAL: InternalError,
  UNAUTHENTICATED: UnauthenticatedError,
  PERMISSION_DENIED: PermissionDeniedError,
  RESOURCE_EXHAUSTED: ResourceExhaustedError,
};

/**
 * Default retriability, used **only** when the envelope omits `retriable`.
 *
 * A stated flag **always** wins, and every current AletheiaDB server states one
 * (`AletheiaHttpError::into_response_with_trace` writes `retriable`
 * unconditionally). This default therefore fires against a bare status, a
 * proxy's error page, or an older / non-conforming server — never against a
 * conforming response.
 *
 * Mirrors the #3234 contract: only `CONFLICT` and `UNAVAILABLE` are transient
 * by code alone. `RESOURCE_EXHAUSTED` is split by status, since the code cannot
 * distinguish its flavors: an HTTP 429 (the transient-overload status: a
 * read-class wall-clock timeout, or a rate limiter in front of the server) gets
 * the benefit of the doubt, while 413/422 — the byte/row caps and the invalid
 * limit override — stay non-retriable.
 *
 * Deliberately **not** a blanket "429 is retriable": the status is checked only
 * alongside `RESOURCE_EXHAUSTED`, so a 429 carrying a caller-fault code
 * (`NOT_FOUND`, `INVALID_ARGUMENT`, …) stays non-retriable as the #3234 table
 * requires. Note the server also emits 429 with an explicit `retriable: false`
 * for a **write-class** wall-clock timeout (the write may already have
 * committed — a retry could duplicate it) and for a tenant-quota breach; both
 * state the flag, so both are honored verbatim and never reach this fallback.
 *
 * The default is shared by **both** envelope shapes — the nested #3234/#3629
 * body and the legacy flat one — so an identical error normalizes identically
 * whichever shape it arrived in.
 *
 * @param code - the structured error code.
 * @param httpStatus - the originating HTTP status. In-band MCP errors ride an
 *   HTTP 200 and pass `200` here, which matches no status rule — they fall
 *   through to the code-only decision, as intended.
 */
function defaultRetriable(code: string, httpStatus?: number): boolean {
  if (code === 'RESOURCE_EXHAUSTED') return httpStatus === 429;
  return code === 'CONFLICT' || code === 'UNAVAILABLE';
}

/**
 * Construct the correct {@link AletheiaError} subclass from normalized fields.
 * An unknown `code` yields the base {@link AletheiaError} with `retriable` as
 * supplied (defaulting to `false`).
 */
export function makeError(init: AletheiaErrorInit): AletheiaError {
  const Ctor = (CODE_TO_CLASS as Record<string, new (init: AletheiaErrorInit) => AletheiaError>)[init.code];
  if (Ctor) {
    return new Ctor(init);
  }
  // Unknown code -> base class, never retriable (forward-compat rule).
  return new AletheiaError({ ...init, retriable: false });
}

/**
 * The **legacy flat** HTTP envelope, retained only to keep a client pinned
 * against a pre-#3629 server working: `{ success: false, error, code?,
 * retriable?, details?, trace_id? }`. No current server emits this shape.
 *
 * The current nested shape — `{ error: { code, message, retriable, details? },
 * trace_id? }`, shared byte-for-byte by the MCP in-band surface (#3234) and the
 * HTTP error surface (#3629) — is read field-by-field with runtime guards in
 * {@link parseMcpError} rather than through a declared interface, since a
 * malformed body must not be trusted to match its type.
 */
interface HttpErrorEnvelope {
  success: false;
  error: string;
  code?: string;
  retriable?: boolean;
  details?: ErrorDetails;
  trace_id?: string;
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null;
}

/**
 * If `body` is a nested structured error (`{ error: { code, ... } }`), return
 * its normalized form; else `null`. Handles both the MCP in-band 200 shape and
 * the unified HTTP error envelope (#3629), capturing a top-level `trace_id`
 * sibling when present.
 */
export function parseMcpError(body: unknown, httpStatus?: number): AletheiaError | null {
  if (!isObject(body)) return null;
  const err = body['error'];
  if (!isObject(err) || typeof err['code'] !== 'string') return null;
  const code = err['code'];
  return makeError({
    code,
    message: typeof err['message'] === 'string' ? err['message'] : code,
    retriable: asBoolean(err['retriable']) ?? defaultRetriable(code, httpStatus),
    details: asDetails(err['details']),
    httpStatus,
    // The server places `trace_id` top-level, a sibling of `error`. Fall back to
    // an inner one only for a non-conforming emitter — recovering the id is
    // strictly better than dropping it, and a conforming body never gets here.
    traceId: asString(body['trace_id']) ?? asString(err['trace_id']),
  });
}

/** Narrow to a real `boolean`, or `undefined` — a `"true"` string is not a flag. */
function asBoolean(v: unknown): boolean | undefined {
  return typeof v === 'boolean' ? v : undefined;
}

/** Narrow to a non-empty `string`, or `undefined`. */
function asString(v: unknown): string | undefined {
  return typeof v === 'string' && v.length > 0 ? v : undefined;
}

/**
 * Narrow to a real object for {@link ErrorDetails}, or `undefined`. A malformed
 * scalar/array `details` must not land in a field declared
 * `Record<string, unknown>`.
 */
function asDetails(v: unknown): ErrorDetails | undefined {
  return isObject(v) && !Array.isArray(v) ? v : undefined;
}

/**
 * Normalize an HTTP-envelope error (`{ success: false, ... }`), a plain-text
 * error body, or a bare non-2xx response into an {@link AletheiaError}.
 * `statusFallbackCode` maps the HTTP status to a code when the body carries
 * none (401/403 in particular).
 */
export function parseHttpError(
  body: unknown,
  httpStatus: number,
  statusFallbackCode: string,
): AletheiaError {
  if (isObject(body)) {
    // Prefer an in-band MCP structured error if present.
    const mcp = parseMcpError(body, httpStatus);
    if (mcp) return mcp;

    if (body['success'] === false || typeof body['error'] === 'string' || typeof body['code'] === 'string') {
      const env = body as unknown as HttpErrorEnvelope;
      const code = asString(env.code) ?? statusFallbackCode;
      return makeError({
        code,
        // Falls back to the code (not `HTTP <status>`) exactly as the nested
        // branch does, so a message-less body normalizes identically in both
        // shapes.
        message: asString(env.error) ?? code,
        retriable: asBoolean(env.retriable) ?? defaultRetriable(code, httpStatus),
        details: asDetails(env.details),
        httpStatus,
        // Guarded exactly like `parseMcpError`: a non-string `trace_id` from a
        // malformed body must not land in a field typed `string | undefined`.
        traceId: asString(env.trace_id),
      });
    }
  }
  // A non-empty plain-text body (e.g. a proxy's "502 Bad Gateway" page) becomes
  // the error message so the caller sees the server's actual payload.
  const message =
    typeof body === 'string' && body.length > 0
      ? body
      : // A *nested-shaped* body that `parseMcpError` rejected — `error` is an
        // object but its `code` is missing or not a string — still carries the
        // server's explanation. Salvage it rather than discarding it for a bare
        // `HTTP <status>`: with the nested envelope now the only shape a real
        // server emits, a malformed nested body is the likeliest failure mode.
        (nestedMessage(body) ?? `HTTP ${httpStatus}`);
  // No usable envelope: synthesize from the status.
  return makeError({
    code: statusFallbackCode,
    message,
    retriable: defaultRetriable(statusFallbackCode, httpStatus),
    httpStatus,
  });
}

/**
 * Extract `error.message` from a nested-shaped body whose `code` was missing or
 * non-string, or `null` when there is no usable string there.
 */
function nestedMessage(body: unknown): string | null {
  if (!isObject(body)) return null;
  const err = body['error'];
  if (!isObject(err)) return null;
  const message = err['message'];
  return typeof message === 'string' && message.length > 0 ? message : null;
}

/**
 * Map an HTTP status to the fallback structured code used when the body omits
 * one. Mirrors `AletheiaHttpError::status()` / `code_str()` (`src/http/error.rs`):
 *
 * - 412 is the server's `FAILED_PRECONDITION` status (read-only replica, a
 *   non-conforming schema-constraint enable, transaction validation, …).
 * - 413 is the row/byte cap → `RESOURCE_EXHAUSTED`; 429 is the wall-clock
 *   timeout / principal quota → also `RESOURCE_EXHAUSTED`.
 * - 422 is emitted by exactly one variant, `InvalidLimitOverride`, whose
 *   `code_str()` is `INVALID_ARGUMENT` — **not** a resource cap.
 *
 * 409 is genuinely ambiguous (the server returns it for both the retriable
 * `CONFLICT` and the non-retriable `CONSTRAINT_VIOLATION`); a code-less 409
 * carries no signal to tell them apart, so it keeps the historical `CONFLICT`
 * mapping. Every conforming server states the code, so this is a last resort.
 */
export function statusToCode(status: number): string {
  switch (status) {
    case 400:
      return 'INVALID_ARGUMENT';
    case 401:
      return 'UNAUTHENTICATED';
    case 403:
      return 'PERMISSION_DENIED';
    case 404:
      return 'NOT_FOUND';
    case 409:
      return 'CONFLICT';
    case 412:
      return 'FAILED_PRECONDITION';
    case 413:
    case 429:
      return 'RESOURCE_EXHAUSTED';
    case 422:
      return 'INVALID_ARGUMENT';
    case 503:
      return 'UNAVAILABLE';
    default:
      return status >= 500 ? 'INTERNAL' : 'INVALID_ARGUMENT';
  }
}
