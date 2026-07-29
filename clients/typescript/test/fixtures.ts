/**
 * Record-replay fetch fixtures.
 *
 * The AletheiaDB server binary is too heavy to build reliably in the SDK CI
 * sandbox, so these tests inject a mock `fetch` that returns canned responses
 * whose shapes match `tests/parity/inventory.json` and the autumn-server
 * `*_tools.rs` handlers. This is the record-replay approach the issue permits
 * for the unit layer; integration against a live binary is wired separately.
 */

import type { FetchLike, FetchResponseLike } from '../src/index.js';

/** A request captured by the mock, with its URL, method, headers, and parsed body. */
export interface RecordedRequest {
  url: string;
  path: string;
  query: URLSearchParams;
  method: string;
  headers: Record<string, string>;
  body: unknown;
}

/** A canned response the handler returns for a request. */
export interface CannedResponse {
  status?: number;
  /** JSON body; serialized to the response text. */
  body: unknown;
  /** Override the raw text (takes precedence over `body`). */
  raw?: string;
}

/** Build a {@link FetchResponseLike} from a status + text. */
function makeResponse(status: number, text: string): FetchResponseLike {
  return {
    status,
    ok: status >= 200 && status < 300,
    text: () => Promise.resolve(text),
  };
}

/** A handler maps a recorded request to a canned response. */
export type MockHandler = (req: RecordedRequest, callIndex: number) => CannedResponse;

/** The mock fetch plus the list of requests it has seen (in order). */
export interface MockFetch {
  fetch: FetchLike;
  calls: RecordedRequest[];
}

/**
 * Build a mock `fetch` from a handler. Every call is recorded (with its parsed
 * URL, query, headers, and JSON body) into `calls`, and the handler's canned
 * response is replayed.
 */
export function mockFetch(handler: MockHandler): MockFetch {
  const calls: RecordedRequest[] = [];
  const fetch: FetchLike = (input, init) => {
    const url = new URL(input);
    const parsedBody =
      init?.body !== undefined ? (JSON.parse(init.body) as unknown) : undefined;
    const recorded: RecordedRequest = {
      url: input,
      path: url.pathname,
      query: url.searchParams,
      method: init?.method ?? 'GET',
      headers: init?.headers ?? {},
      body: parsedBody,
    };
    const callIndex = calls.length;
    calls.push(recorded);
    const canned = handler(recorded, callIndex);
    const status = canned.status ?? 200;
    const text = canned.raw ?? JSON.stringify(canned.body);
    return Promise.resolve(makeResponse(status, text));
  };
  return { fetch, calls };
}

/** A canned node entity with an open, current temporal block (matches #3232). */
export function nodeFixture(
  id: number,
  label: string,
  properties: Record<string, unknown>,
): Record<string, unknown> {
  return {
    id,
    label,
    properties,
    temporal: {
      valid_from: '2024-01-01T00:00:00Z',
      valid_to: null,
      transaction_from: '2024-01-01T00:00:00Z',
      transaction_to: null,
      is_current: true,
    },
  };
}

/** A canned edge entity with an open, current temporal block. */
export function edgeFixture(
  id: number,
  label: string,
  sourceId: number,
  targetId: number,
  properties: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    id,
    label,
    source_id: sourceId,
    target_id: targetId,
    properties,
    temporal: {
      valid_from: '2024-01-01T00:00:00Z',
      valid_to: null,
      transaction_from: '2024-01-01T00:00:00Z',
      transaction_to: null,
      is_current: true,
    },
  };
}

/** The MCP-style in-band structured error envelope (#3234), returned with HTTP 200. */
export function mcpError(
  code: string,
  message: string,
  retriable: boolean,
  details?: Record<string, unknown>,
): CannedResponse {
  return {
    status: 200,
    body: { error: { code, message, retriable, ...(details ? { details } : {}) } },
  };
}

/**
 * The **nested** HTTP error envelope (Issue #3629): `{ error: { code, message,
 * retriable, details? }, trace_id? }`, byte-shape-identical to the MCP surface
 * (#3234), with `trace_id` a top-level sibling of `error`. The legacy flat
 * `{ success: false, error, code, ... }` shape has been removed server-side.
 */
export function httpError(
  status: number,
  message: string,
  code: string,
  retriable?: boolean,
  details?: Record<string, unknown>,
  traceId?: string,
): CannedResponse {
  const err: Record<string, unknown> = { code, message };
  if (retriable !== undefined) err['retriable'] = retriable;
  if (details !== undefined) err['details'] = details;
  const body: Record<string, unknown> = { error: err };
  if (traceId !== undefined) body['trace_id'] = traceId;
  return { status, body };
}

/**
 * The **legacy flat** HTTP error envelope: `{ success: false, error: "<msg>",
 * code?, retriable?, details?, trace_id? }`.
 *
 * @deprecated No current AletheiaDB server emits this shape — Issue #3629
 * replaced it with the nested {@link httpError} envelope, byte-shape-identical
 * to the MCP surface (#3234). This helper exists **only** to exercise the
 * SDK's retained backward-compatibility branch in `parseHttpError`, so a caller
 * pinned against a pre-#3629 server still gets a typed error rather than a bare
 * `HTTP <status>`. Use {@link httpError} for every fixture that models the
 * current server; a test using this helper is asserting back-compat, nothing
 * else.
 */
export function legacyFlatHttpError(
  status: number,
  message: string,
  code?: string,
  retriable?: boolean,
  details?: Record<string, unknown>,
  traceId?: string,
): CannedResponse {
  const body: Record<string, unknown> = { success: false, error: message };
  if (code !== undefined) body['code'] = code;
  if (retriable !== undefined) body['retriable'] = retriable;
  if (details !== undefined) body['details'] = details;
  if (traceId !== undefined) body['trace_id'] = traceId;
  return { status, body };
}
