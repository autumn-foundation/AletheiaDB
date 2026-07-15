import { describe, expect, it } from 'vitest';
import { toEpochMicros, toWireTime } from '../src/index.js';
import { AletheiaClient } from '../src/index.js';
import { mockFetch, nodeFixture } from './fixtures.js';

describe('time coercion', () => {
  const instantIso = '2024-01-15T10:00:00.000Z';
  const instantMs = Date.parse(instantIso);
  const instantMicros = instantMs * 1000;

  it('coerces Date, ISO string, and epoch-micros to the SAME wire value', () => {
    // The wire value is a DECIMAL-MICROSECONDS STRING: the server deserializes
    // POST-body temporal fields as Rust `String`, so a JSON number would 4xx.
    const expected = String(instantMicros);
    const fromDate = toWireTime(new Date(instantIso));
    const fromString = toWireTime(instantIso);
    const fromNumber = toWireTime(instantMicros);
    expect(typeof fromDate).toBe('string');
    expect(fromDate).toBe(expected);
    expect(fromString).toBe(expected);
    expect(fromNumber).toBe(expected);
    expect(fromDate).toBe(fromString);
    expect(fromString).toBe(fromNumber);
  });

  it('documents millisecond precision for Date/string (multiple of 1000 micros)', () => {
    expect(toEpochMicros(new Date(instantIso)) % 1000).toBe(0);
    expect(toEpochMicros(instantIso) % 1000).toBe(0);
    // A numeric input can carry finer resolution.
    expect(toEpochMicros(instantMicros + 7)).toBe(instantMicros + 7);
  });

  it('rejects invalid inputs', () => {
    expect(() => toEpochMicros(new Date('nope'))).toThrow(RangeError);
    expect(() => toEpochMicros('not-a-timestamp')).toThrow(RangeError);
    expect(() => toEpochMicros(Number.POSITIVE_INFINITY)).toThrow(RangeError);
  });

  it('all three forms produce the same serialized request (create_node valid_time)', async () => {
    const seen: unknown[] = [];
    const capture = () => {
      const { fetch, calls } = mockFetch(() => ({ body: nodeFixture(1, 'Person', {}) }));
      const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });
      return { db, calls, seen };
    };
    for (const t of [new Date(instantIso), instantIso, instantMicros]) {
      const { db, calls } = capture();
      await db.createNode({ label: 'Person', properties: {}, validTime: t });
      seen.push((calls[0]!.body as { valid_time: string }).valid_time);
    }
    expect(new Set(seen).size).toBe(1);
    // Serialized as a decimal-microseconds STRING, not a JSON number.
    expect(typeof seen[0]).toBe('string');
    expect(seen[0]).toBe(String(instantMicros));
  });

  it('serializes POST-body timestamp fields as JSON strings (regression guard #blocker)', async () => {
    // The server deserializes valid_time/transaction_time as Rust `String`;
    // a JSON number would fail deserialization with a 4xx. The record-replay
    // layer parses JSON before we inspect it, so assert on `typeof` here to
    // pin the wire type and prevent a silent regression to numbers.
    const { fetch, calls } = mockFetch(() => ({ body: nodeFixture(1, 'Person', {}) }));
    const db = new AletheiaClient({ baseUrl: 'http://x', apiKey: 'k', fetch });

    await db.createNode({ label: 'Person', properties: {}, validTime: instantMicros });
    const createBody = calls[0]!.body as { valid_time: unknown };
    expect(typeof createBody.valid_time).toBe('string');

    await db.getNodeAtTime({ nodeId: 1, validTime: instantIso, transactionTime: instantIso });
    const atTimeBody = calls[1]!.body as { valid_time: unknown; transaction_time: unknown };
    expect(typeof atTimeBody.valid_time).toBe('string');
    expect(typeof atTimeBody.transaction_time).toBe('string');
  });
});
