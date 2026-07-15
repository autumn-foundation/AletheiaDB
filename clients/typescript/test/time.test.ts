import { describe, expect, it } from 'vitest';
import { toEpochMicros, toWireTime } from '../src/index.js';
import { AletheiaClient } from '../src/index.js';
import { mockFetch, nodeFixture } from './fixtures.js';

describe('time coercion', () => {
  const instantIso = '2024-01-15T10:00:00.000Z';
  const instantMs = Date.parse(instantIso);
  const instantMicros = instantMs * 1000;

  it('coerces Date, ISO string, and epoch-micros to the SAME wire value', () => {
    const fromDate = toWireTime(new Date(instantIso));
    const fromString = toWireTime(instantIso);
    const fromNumber = toWireTime(instantMicros);
    expect(fromDate).toBe(instantMicros);
    expect(fromString).toBe(instantMicros);
    expect(fromNumber).toBe(instantMicros);
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
      seen.push((calls[0]!.body as { valid_time: number }).valid_time);
    }
    expect(new Set(seen).size).toBe(1);
    expect(seen[0]).toBe(instantMicros);
  });
});
