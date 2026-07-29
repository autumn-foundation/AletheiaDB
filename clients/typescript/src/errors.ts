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
 * Default retriability, used **only** when the envelope omits `retriable` (a
 * bare 4xx/5xx, a proxy's error page, or an older / non-conforming server — the
 * current server always states the flag, and a stated flag always wins).
 *
 * Mirrors the #3234 contract: only `CONFLICT`, `UNAVAILABLE`, and the timeout
 * flavor of `RESOURCE_EXHAUSTED` may be transient. Since the code alone cannot
 * distinguish the timeout flavor of `RESOURCE_EXHAUSTED` (retriable) from the
 * byte/row-cap flavor (not), `httpStatus` disambiguates when it is known: HTTP
 * 429 is the server's wall-clock-timeout / rate-limit status (it even sets
 * `Retry-After`), whereas 413/422 are the non-retriable caps.
 *
 * This default is shared by **both** envelope shapes — the nested #3234/#3629
 * body and the legacy flat one — so an identical error normalizes identically
 * whichever shape it arrived in.
 *
 * @param code - the structured error code.
 * @param httpStatus - the originating HTTP status, when known. Omitted for
 *   in-band MCP errors, which ride an HTTP 200 and carry no status signal.
 */
function defaultRetriable(code: string, httpStatus?: number): boolean {
  if (httpStatus === 429) return true;
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
 * The nested structured error object shared by the MCP in-band surface (#3234)
 * and — since Issue #3629 — the HTTP error surface: `{ error: { code, message,
 * retriable, details? } }`. On the HTTP surface a `trace_id` may sit as a
 * top-level sibling of `error` (never inside it).
 */
interface McpErrorEnvelope {
  error: {
    code: string;
    message?: string;
    retriable?: boolean;
    details?: ErrorDetails;
  };
  /** Present only on the HTTP surface (#3629); a top-level sibling of `error`. */
  trace_id?: string;
}

/** The HTTP envelope: `{ success: false, error, code?, retriable?, details?, trace_id? }`. */
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
  const envelope = body as unknown as McpErrorEnvelope;
  const code = envelope.error.code;
  const traceId = typeof envelope.trace_id === 'string' ? envelope.trace_id : undefined;
  return makeError({
    code,
    message: envelope.error.message ?? code,
    retriable: envelope.error.retriable ?? defaultRetriable(code, httpStatus),
    details: envelope.error.details,
    httpStatus,
    traceId,
  });
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
      const code = env.code ?? statusFallbackCode;
      const message = typeof env.error === 'string' ? env.error : `HTTP ${httpStatus}`;
      return makeError({
        code,
        message,
        retriable: env.retriable ?? defaultRetriable(code, httpStatus),
        details: env.details,
        httpStatus,
        traceId: env.trace_id,
      });
    }
  }
  // A non-empty plain-text body (e.g. a proxy's "502 Bad Gateway" page) becomes
  // the error message so the caller sees the server's actual payload.
  const message =
    typeof body === 'string' && body.length > 0 ? body : `HTTP ${httpStatus}`;
  // No usable envelope: synthesize from the status.
  return makeError({
    code: statusFallbackCode,
    message,
    retriable: defaultRetriable(statusFallbackCode, httpStatus),
    httpStatus,
  });
}

/** Map an HTTP status to the fallback structured code used when the body omits one. */
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
    case 413:
    case 422:
    case 429:
      return 'RESOURCE_EXHAUSTED';
    case 503:
      return 'UNAVAILABLE';
    default:
      return status >= 500 ? 'INTERNAL' : 'INVALID_ARGUMENT';
  }
}
