import { describe, expect, it } from 'vitest';
import {
  AletheiaClient,
  AletheiaError,
  UnavailableError,
  type FetchLike,
  type FetchResponseLike,
} from '../src/index.js';
import { mockFetch, nodeFixture } from './fixtures.js';

const noSleep = (): Promise<void> => Promise.resolve();
const fixedRandom = (): number => 0.5;

/** Build a canned {@link FetchResponseLike} for a status + JSON body. */
function jsonResponse(status: number, body: unknown): FetchResponseLike {
  const text = JSON.stringify(body);
  return { status, ok: status >= 200 && status < 300, text: () => Promise.resolve(text) };
}

/** A native `AbortError`, matching what `fetch` throws on an aborted signal. */
function abortError(): Error {
  const e = new Error('The operation was aborted');
  e.name = 'AbortError';
  return e;
}

describe('network-error retriability (#3369 review: HIGH)', () => {
  it('retries a rejected fetch (network error) when retry is enabled, then succeeds', async () => {
    let n = 0;
    const fetch: FetchLike = () => {
      n += 1;
      if (n <= 2) return Promise.reject(new Error('ECONNREFUSED'));
      return Promise.resolve(jsonResponse(200, nodeFixture(1, 'Person', { name: 'Alice' })));
    };
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      retry: { enabled: true, maxAttempts: 3, sleep: noSleep, random: fixedRandom },
    });
    const node = await db.getNode(1);
    expect(node.label).toBe('Person');
    expect(n).toBe(3);
  });

  it('throws UNAVAILABLE (retriable) on the first rejected fetch when retry is OFF', async () => {
    let n = 0;
    const fetch: FetchLike = () => {
      n += 1;
      return Promise.reject(new Error('getaddrinfo ENOTFOUND'));
    };
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const err = await db.getNode(1).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(UnavailableError);
    expect((err as AletheiaError).code).toBe('UNAVAILABLE');
    expect((err as AletheiaError).retriable).toBe(true);
    expect((err as AletheiaError).message).toContain('Network error');
    expect(n).toBe(1);
  });
});

describe('plain-text error body + 429 retriability (#3369 review: MEDIUM)', () => {
  it('surfaces a 502 plain-text body as the error message', async () => {
    const { fetch } = mockFetch(() => ({ status: 502, body: undefined, raw: 'Bad Gateway: upstream timeout' }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const err = await db.getNode(1).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(AletheiaError);
    expect((err as AletheiaError).message).toBe('Bad Gateway: upstream timeout');
    expect((err as AletheiaError).httpStatus).toBe(502);
  });

  it('defaults a 429 to retriable (envelope without an explicit flag)', async () => {
    const { fetch } = mockFetch(() => ({ status: 429, body: { success: false, error: 'slow down' } }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const err = await db.listNodes().catch((e: unknown) => e);
    expect((err as AletheiaError).httpStatus).toBe(429);
    expect((err as AletheiaError).retriable).toBe(true);
  });

  it('defaults a bare 429 (no envelope) to retriable', async () => {
    const { fetch } = mockFetch(() => ({ status: 429, body: undefined, raw: 'Too Many Requests' }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
    const err = await db.getNode(1).catch((e: unknown) => e);
    expect((err as AletheiaError).message).toBe('Too Many Requests');
    expect((err as AletheiaError).retriable).toBe(true);
  });
});

describe('abort / timeout support (#3369 review: MAJOR)', () => {
  it('rejects an already-aborted signal before any fetch is issued', async () => {
    let fetchCalls = 0;
    const fetch: FetchLike = () => {
      fetchCalls += 1;
      return Promise.resolve(jsonResponse(200, nodeFixture(1, 'Person', {})));
    };
    const controller = new AbortController();
    controller.abort();
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      signal: controller.signal,
    });
    await expect(db.getNode(1)).rejects.toBeTruthy();
    expect(fetchCalls).toBe(0);
  });

  it('stops retries when the signal aborts during backoff', async () => {
    let fetchCalls = 0;
    // Every attempt returns a retriable UNAVAILABLE network error.
    const fetch: FetchLike = () => {
      fetchCalls += 1;
      return Promise.reject(new Error('ECONNRESET'));
    };
    const controller = new AbortController();
    let sleepCalls = 0;
    // The first backoff wait aborts the request and never resolves; the
    // abortable sleep must reject on the abort instead of waiting it out.
    const sleep = (): Promise<void> => {
      sleepCalls += 1;
      controller.abort();
      return new Promise<void>(() => {
        /* never resolves */
      });
    };
    const db = new AletheiaClient({
      baseUrl: 'http://x',
      apiKey: 'k',
      fetch,
      signal: controller.signal,
      retry: { enabled: true, maxAttempts: 5, sleep, random: fixedRandom },
    });
    await expect(db.getNode(1)).rejects.toBeTruthy();
    expect(fetchCalls).toBe(1);
    expect(sleepCalls).toBe(1);
  });

  it('times out a hanging request via timeoutMs', async () => {
    let aborted = false;
    // A fetch that only settles when its signal aborts (as real fetch does).
    const fetch: FetchLike = (_input, init) =>
      new Promise<FetchResponseLike>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => {
          aborted = true;
          reject(abortError());
        });
      });
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch, timeoutMs: 10 });
    await expect(db.getNode(1)).rejects.toBeTruthy();
    expect(aborted).toBe(true);
  });
});
