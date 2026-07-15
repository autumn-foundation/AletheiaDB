/**
 * The fetch-based transport: request building, auth, envelope decoding, and
 * error normalization. Uses the global `fetch` by default; a custom `fetch`
 * may be injected (older Node, tests, edge runtimes with a polyfill).
 *
 * @packageDocumentation
 */

import {
  AletheiaError,
  parseHttpError,
  parseMcpError,
  statusToCode,
} from './errors.js';
import type { RetryOptions, ResolvedRetryOptions } from './retry.js';
import { resolveRetryOptions, withRetry } from './retry.js';

/** The minimal `fetch` signature this SDK depends on. */
export type FetchLike = (
  input: string,
  init?: {
    method?: string;
    headers?: Record<string, string>;
    body?: string;
    signal?: AbortSignal;
  },
) => Promise<FetchResponseLike>;

/** The minimal `Response` surface this SDK reads. */
export interface FetchResponseLike {
  status: number;
  ok: boolean;
  text(): Promise<string>;
}

/** How the API key is presented to the server. */
export type AuthScheme = 'bearer' | 'x-api-key';

/** Options for constructing an {@link AletheiaClient}. */
export interface ClientOptions {
  /** Base URL of the AletheiaDB HTTP server, e.g. `http://localhost:8080`. */
  baseUrl: string;
  /** API key. Omit only against a server running in anonymous mode. */
  apiKey?: string;
  /**
   * How to send the key. `'bearer'` (default) sends
   * `Authorization: Bearer <key>`; `'x-api-key'` sends the `x-api-key` header.
   */
  authScheme?: AuthScheme;
  /** Inject a custom `fetch`. Defaults to the global `fetch`. */
  fetch?: FetchLike;
  /** Extra headers merged into every request. */
  headers?: Record<string, string>;
  /** Built-in retry policy. Off by default; honors `retriable` only. */
  retry?: RetryOptions;
}

/** An internal request descriptor. */
export interface RequestSpec {
  method: 'GET' | 'POST';
  /** Path beginning with `/`, e.g. `/nodes/1`. */
  path: string;
  /** Query parameters; `undefined` values are dropped. */
  query?: Record<string, string | number | boolean | string[] | undefined>;
  /** JSON body for POST requests. */
  body?: unknown;
}

/**
 * The HTTP transport. Owns the base URL, auth, fetch, and retry policy, and
 * exposes a single {@link Transport.request} that returns decoded JSON or
 * throws a typed {@link AletheiaError}.
 */
export class Transport {
  private readonly baseUrl: string;
  private readonly apiKey: string | undefined;
  private readonly authScheme: AuthScheme;
  private readonly fetchImpl: FetchLike;
  private readonly baseHeaders: Record<string, string>;
  private readonly retryCfg: ResolvedRetryOptions;

  constructor(options: ClientOptions) {
    if (!options.baseUrl) {
      throw new Error('AletheiaClient: `baseUrl` is required');
    }
    this.baseUrl = options.baseUrl.replace(/\/+$/, '');
    this.apiKey = options.apiKey;
    this.authScheme = options.authScheme ?? 'bearer';
    const injected = options.fetch;
    const globalFetch = (globalThis as { fetch?: FetchLike }).fetch;
    const chosen = injected ?? globalFetch;
    if (!chosen) {
      throw new Error(
        'AletheiaClient: no global `fetch` available; pass `fetch` in ClientOptions (Node <18 or non-fetch runtime)',
      );
    }
    this.fetchImpl = chosen;
    this.baseHeaders = { ...(options.headers ?? {}) };
    this.retryCfg = resolveRetryOptions(options.retry);
  }

  /** The resolved retry configuration (exposed for a per-call override). */
  retryConfig(): ResolvedRetryOptions {
    return this.retryCfg;
  }

  /** Build the auth headers for a request. */
  private authHeaders(): Record<string, string> {
    if (!this.apiKey) return {};
    if (this.authScheme === 'x-api-key') {
      return { 'x-api-key': this.apiKey };
    }
    return { Authorization: `Bearer ${this.apiKey}` };
  }

  /** Build a full URL with an encoded query string. */
  private url(spec: RequestSpec): string {
    const params: string[] = [];
    if (spec.query) {
      for (const [key, value] of Object.entries(spec.query)) {
        if (value === undefined) continue;
        if (Array.isArray(value)) {
          for (const item of value) {
            params.push(`${encodeURIComponent(key)}=${encodeURIComponent(item)}`);
          }
        } else {
          params.push(`${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
        }
      }
    }
    const qs = params.length > 0 ? `?${params.join('&')}` : '';
    return `${this.baseUrl}${spec.path}${qs}`;
  }

  /**
   * Perform a single request (no retry). Decodes the response into JSON and
   * throws a typed {@link AletheiaError} for any error envelope or non-2xx
   * status.
   *
   * @typeParam T - the expected decoded success shape.
   */
  async requestOnce<T>(spec: RequestSpec): Promise<T> {
    const headers: Record<string, string> = {
      accept: 'application/json',
      ...this.baseHeaders,
      ...this.authHeaders(),
    };
    const init: {
      method?: string;
      headers?: Record<string, string>;
      body?: string;
    } = { method: spec.method, headers };
    if (spec.body !== undefined) {
      headers['content-type'] = 'application/json';
      init.body = JSON.stringify(spec.body);
    }

    const resp = await this.fetchImpl(this.url(spec), init);
    const raw = await resp.text();
    const parsed = raw.length > 0 ? safeJsonParse(raw) : undefined;

    if (!resp.ok) {
      throw parseHttpError(parsed, resp.status, statusToCode(resp.status));
    }

    // 2xx: node/edge/traverse routes forward MCP in-band errors with a 200.
    const inBand = parseMcpError(parsed, resp.status);
    if (inBand) {
      throw inBand;
    }
    // HTTP admin/status routes wrap success as { success: true, data }.
    if (isObject(parsed) && parsed['success'] === false) {
      throw parseHttpError(parsed, resp.status, statusToCode(resp.status));
    }
    return parsed as T;
  }

  /**
   * Perform a request under the retry policy. With retries disabled (default)
   * this is a single attempt.
   *
   * @typeParam T - the expected decoded success shape.
   */
  async request<T>(spec: RequestSpec): Promise<T> {
    return withRetry(() => this.requestOnce<T>(spec), this.retryCfg);
  }
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null;
}

function safeJsonParse(raw: string): unknown {
  try {
    return JSON.parse(raw) as unknown;
  } catch {
    // Non-JSON body (should not happen against the AletheiaDB surface). Surface
    // it as a plain string so callers still see the payload.
    return raw;
  }
}

/**
 * Unwrap the HTTP `{ success: true, data }` envelope used by the admin/status
 * routes. Tool routes return bare entity JSON and bypass this.
 */
export function unwrapData<T>(body: unknown): T {
  if (isObject(body) && 'data' in body) {
    return body['data'] as T;
  }
  return body as T;
}

export { AletheiaError };
