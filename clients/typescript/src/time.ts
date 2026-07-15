/**
 * Time coercion for AletheiaDB's bi-temporal API.
 *
 * AletheiaDB tracks **two** independent time dimensions on every fact:
 *
 * - **valid time** — when the fact was true *in the real world*. You control
 *   this: back-date an import, or record that something became true in the
 *   future. Write requests accept an optional `validTime` for exactly this.
 * - **transaction time** — when the fact was *recorded in the database*. This
 *   is always system-assigned and can never be set by a caller; it only
 *   appears on the read side (`as_of_transaction_time`, temporal bounds).
 *
 * Every time-typed parameter in this SDK accepts a {@link TimeInput}: a
 * `Date`, an ISO 8601 / RFC 3339 `string`, or a `number` interpreted as
 * **microseconds since the Unix epoch**. All three are coerced to a single
 * canonical wire value — epoch microseconds — so the same instant expressed
 * three different ways serializes identically.
 *
 * @packageDocumentation
 */

/**
 * A point in time accepted by any temporal parameter of the SDK.
 *
 * - `Date` — coerced via {@link Date.getTime} (millisecond precision).
 * - `string` — parsed as ISO 8601 / RFC 3339 (millisecond precision; any
 *   sub-millisecond fraction in the string is truncated by the JS parser).
 * - `number` — taken **as-is** to be microseconds since the Unix epoch. This
 *   is the only form that can carry microsecond precision.
 *
 * @remarks
 * Precision: `Date` and `string` inputs resolve to millisecond granularity
 * (the coerced microsecond value is always a multiple of 1000). Supply a
 * `number` of epoch-microseconds when you need finer resolution.
 */
export type TimeInput = Date | string | number;

/** Microseconds per millisecond. */
const MICROS_PER_MILLI = 1000;

/**
 * Coerce a {@link TimeInput} to **epoch microseconds** — the canonical wire
 * value this SDK sends for every temporal parameter.
 *
 * @param value - a `Date`, ISO 8601 string, or epoch-microseconds number.
 * @returns integer microseconds since the Unix epoch.
 * @throws {RangeError} if a `Date` is invalid, a string cannot be parsed as a
 *   timestamp, or a number is not finite.
 *
 * @example
 * ```ts
 * const a = toEpochMicros(new Date('2024-01-15T10:00:00Z'));
 * const b = toEpochMicros('2024-01-15T10:00:00Z');
 * const c = toEpochMicros(1705312800000000);
 * // a === b === c
 * ```
 */
export function toEpochMicros(value: TimeInput): number {
  if (value instanceof Date) {
    const ms = value.getTime();
    if (Number.isNaN(ms)) {
      throw new RangeError('toEpochMicros: received an invalid Date');
    }
    return ms * MICROS_PER_MILLI;
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) {
      throw new RangeError('toEpochMicros: numeric time must be a finite number of epoch microseconds');
    }
    // A bare number is already epoch microseconds.
    return Math.trunc(value);
  }
  // string: parse as ISO 8601 / RFC 3339.
  const ms = Date.parse(value);
  if (Number.isNaN(ms)) {
    throw new RangeError(`toEpochMicros: could not parse timestamp string '${value}'`);
  }
  return ms * MICROS_PER_MILLI;
}

/**
 * Coerce a {@link TimeInput} to the value placed on the wire. The AletheiaDB
 * HTTP surface accepts either an RFC 3339 string or integer epoch microseconds
 * for every temporal field; this SDK always sends epoch microseconds so that
 * a `Date`, its ISO string, and its epoch-microsecond number are byte-for-byte
 * identical requests.
 *
 * @param value - the temporal input to serialize.
 * @returns integer epoch microseconds, ready to embed in a query string or body.
 */
export function toWireTime(value: TimeInput): number {
  return toEpochMicros(value);
}
