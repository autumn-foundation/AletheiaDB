/**
 * Opt-in retry-with-backoff policy.
 *
 * Off by default. When enabled it retries **only** errors whose `retriable`
 * flag is `true` (never a non-retriable code, regardless of class), with a
 * bounded number of attempts and jittered exponential backoff.
 *
 * @packageDocumentation
 */

import { AletheiaError } from './errors.js';

/** Configuration for the built-in retry policy. */
export interface RetryOptions {
  /**
   * Enable retries. Default `false` — with retries off, a call is attempted
   * exactly once and any error propagates immediately.
   */
  enabled?: boolean;
  /** Total attempts including the first (so `3` means up to 2 retries). Default `3`. */
  maxAttempts?: number;
  /** Base backoff in milliseconds for the first retry. Default `100`. */
  baseDelayMs?: number;
  /** Maximum backoff in milliseconds for any single wait. Default `2000`. */
  maxDelayMs?: number;
  /**
   * Jitter fraction in `[0, 1]` applied to each delay. Default `0.5` (each
   * wait is uniformly sampled between 50% and 100% of the computed backoff).
   */
  jitter?: number;
  /** Injectable sleep, primarily for deterministic tests. Defaults to `setTimeout`. */
  sleep?: (ms: number) => Promise<void>;
  /** Injectable RNG in `[0, 1)`, primarily for deterministic tests. Defaults to `Math.random`. */
  random?: () => number;
}

/** Fully-resolved retry configuration. */
export interface ResolvedRetryOptions {
  enabled: boolean;
  maxAttempts: number;
  baseDelayMs: number;
  maxDelayMs: number;
  jitter: number;
  sleep: (ms: number) => Promise<void>;
  random: () => number;
}

const defaultSleep = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

/** Merge user-supplied {@link RetryOptions} over the defaults. */
export function resolveRetryOptions(opts: RetryOptions | undefined): ResolvedRetryOptions {
  return {
    enabled: opts?.enabled ?? false,
    maxAttempts: opts?.maxAttempts ?? 3,
    baseDelayMs: opts?.baseDelayMs ?? 100,
    maxDelayMs: opts?.maxDelayMs ?? 2000,
    jitter: opts?.jitter ?? 0.5,
    sleep: opts?.sleep ?? defaultSleep,
    random: opts?.random ?? Math.random,
  };
}

/** Whether `err` is a retriable AletheiaDB error. Anything else is never retried. */
function isRetriable(err: unknown): boolean {
  return err instanceof AletheiaError && err.retriable === true;
}

/**
 * Run `op` under the resolved retry policy. Retries strictly on retriable
 * {@link AletheiaError}s while attempts remain; a non-retriable error (or a
 * non-AletheiaDB throwable) propagates on the first occurrence.
 *
 * @typeParam T - the operation's resolved value.
 */
export async function withRetry<T>(
  op: () => Promise<T>,
  cfg: ResolvedRetryOptions,
): Promise<T> {
  const attempts = cfg.enabled ? Math.max(1, cfg.maxAttempts) : 1;
  let lastError: unknown;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    try {
      return await op();
    } catch (err) {
      lastError = err;
      const canRetry = cfg.enabled && attempt < attempts && isRetriable(err);
      if (!canRetry) {
        throw err;
      }
      await cfg.sleep(computeDelay(attempt, cfg));
    }
  }
  // Unreachable in practice (the loop either returns or throws), but keeps the
  // type checker satisfied and preserves the last error if it ever is reached.
  throw lastError;
}

/** Exponential backoff with jitter for the given 1-based attempt number. */
function computeDelay(attempt: number, cfg: ResolvedRetryOptions): number {
  const exp = cfg.baseDelayMs * 2 ** (attempt - 1);
  const capped = Math.min(exp, cfg.maxDelayMs);
  const floor = capped * (1 - cfg.jitter);
  const span = capped - floor;
  return Math.round(floor + cfg.random() * span);
}
