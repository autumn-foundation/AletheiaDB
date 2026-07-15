//! Pure interval math for temporal aggregation windows (Issue #3363).
//!
//! This module holds the *storage-free* logic behind AQL temporal aggregation
//! windows: parsing a window boundary timestamp, formatting a boundary back to
//! RFC 3339, and generating the sequence of tumbling `[start, end)` windows for
//! a given granularity. Keeping it separate from the executor iterator lets the
//! subtle interval-edge and calendar arithmetic be unit-tested without a
//! database.
//!
//! # Window semantics (falsifiable contract)
//!
//! Windows are half-open `[b_k, b_{k+1})` with `b_0 = start` and
//! `b_{k+1} = step(b_k)`, generated until `b_k >= end`. The final window's end
//! is **clamped to `end`**, so the union of all windows equals exactly
//! `[start, end)` with no overlap and no gap. A version whose `valid_from`
//! equals a boundary belongs to the window it *starts*
//! (`b_k <= valid_from < b_{k+1}`).
//!
//! Fixed-duration units (minute/hour/day/week) advance by a constant
//! microsecond delta. Calendar units (month/quarter/year) advance by
//! chrono UTC calendar arithmetic, so month lengths (28/29/30/31) and leap
//! years are honored exactly.

use chrono::{DateTime, Months, TimeZone, Utc};

use crate::core::temporal::Timestamp;

/// Microseconds in one minute.
const MICROS_PER_MINUTE: i64 = 60 * 1_000_000;
/// Microseconds in one hour.
const MICROS_PER_HOUR: i64 = 60 * MICROS_PER_MINUTE;
/// Microseconds in one day.
const MICROS_PER_DAY: i64 = 24 * MICROS_PER_HOUR;
/// Microseconds in one week.
const MICROS_PER_WEEK: i64 = 7 * MICROS_PER_DAY;

/// Hard cap on the number of windows a single query may generate, guarding
/// against a caller pairing a huge range with a tiny granularity (which would
/// otherwise allocate unbounded rows and reconstruct history per window).
pub const MAX_WINDOWS: usize = 100_000;

/// A window granularity: a positive multiplier and a time unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGranularity {
    /// The positive multiplier (e.g. `3` in `3 months`).
    pub count: u32,
    /// The time unit.
    pub unit: WindowUnit,
}

/// A temporal window unit. Fixed-duration units advance by a constant
/// microsecond delta; calendar units advance via chrono month arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowUnit {
    /// A minute (fixed 60s).
    Minute,
    /// An hour (fixed 3600s).
    Hour,
    /// A day (fixed 86400s).
    Day,
    /// A week (fixed 7 days).
    Week,
    /// A calendar month.
    Month,
    /// A calendar quarter (3 calendar months).
    Quarter,
    /// A calendar year (12 calendar months).
    Year,
}

impl WindowUnit {
    /// Parse a unit word (case-insensitive, singular or plural). Returns `None`
    /// for an unknown unit so the caller can raise a structured error.
    pub fn parse(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "minute" | "minutes" => Some(WindowUnit::Minute),
            "hour" | "hours" => Some(WindowUnit::Hour),
            "day" | "days" => Some(WindowUnit::Day),
            "week" | "weeks" => Some(WindowUnit::Week),
            "month" | "months" => Some(WindowUnit::Month),
            "quarter" | "quarters" => Some(WindowUnit::Quarter),
            "year" | "years" => Some(WindowUnit::Year),
            _ => None,
        }
    }

    /// The fixed microsecond delta for one unit, or `None` for calendar units.
    fn fixed_micros(self) -> Option<i64> {
        match self {
            WindowUnit::Minute => Some(MICROS_PER_MINUTE),
            WindowUnit::Hour => Some(MICROS_PER_HOUR),
            WindowUnit::Day => Some(MICROS_PER_DAY),
            WindowUnit::Week => Some(MICROS_PER_WEEK),
            _ => None,
        }
    }

    /// The number of calendar months per unit, or `None` for fixed units.
    fn calendar_months(self) -> Option<u32> {
        match self {
            WindowUnit::Month => Some(1),
            WindowUnit::Quarter => Some(3),
            WindowUnit::Year => Some(12),
            _ => None,
        }
    }
}

/// A single tumbling window: half-open `[start, end)` in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Inclusive start (microseconds since epoch).
    pub start_micros: i64,
    /// Exclusive end (microseconds since epoch), clamped to the range end.
    pub end_micros: i64,
}

/// Errors from window construction. Kept as a small enum so the converter can
/// map each to the right structured [`crate::core::error::QueryError`] variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowError {
    /// The multiplier was zero.
    ZeroCount,
    /// `end <= start`.
    EmptyRange,
    /// The granularity/range pair would generate more than [`MAX_WINDOWS`].
    TooManyWindows(usize),
    /// A boundary timestamp could not be parsed.
    BadTimestamp(String),
    /// Calendar arithmetic overflowed the representable timestamp range.
    CalendarOverflow,
}

/// Parse a window boundary timestamp: RFC 3339 / ISO-8601 string, or an integer
/// (or digit string) of microseconds since the Unix epoch. This is a superset
/// of the existing AQL timestamp handling (which accepted only microseconds),
/// so previously valid queries are unaffected.
pub fn parse_boundary_micros(raw: &str) -> Result<i64, WindowError> {
    let trimmed = raw.trim();
    // Integer microseconds since epoch (matches existing AQL convention).
    if let Ok(micros) = trimmed.parse::<i64>() {
        return Ok(micros);
    }
    // RFC 3339 / ISO-8601 with an explicit offset (e.g. `...Z`, `...+00:00`).
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc).timestamp_micros());
    }
    // Bare date `YYYY-MM-DD` interpreted as UTC midnight.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
        && let Some(dt) = date.and_hms_opt(0, 0, 0)
    {
        return Ok(Utc.from_utc_datetime(&dt).timestamp_micros());
    }
    // Naive date-time `YYYY-MM-DDTHH:MM:SS` interpreted as UTC.
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Ok(Utc.from_utc_datetime(&dt).timestamp_micros());
        }
    }
    Err(WindowError::BadTimestamp(raw.to_string()))
}

/// Format a microsecond timestamp as an RFC 3339 UTC string (`Z` suffix,
/// microsecond precision), mirroring the MCP surface's convention. Falls back to
/// a `<micros>us` token if the value is outside chrono's representable range.
pub fn format_rfc3339(micros: i64) -> String {
    DateTime::<Utc>::from_timestamp_micros(micros)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
        .unwrap_or_else(|| format!("{micros}us"))
}

/// Advance a boundary by one granularity step. Fixed units add a constant
/// microsecond delta; calendar units add whole months via chrono UTC
/// arithmetic (clamping to the last valid day of the target month, chrono's
/// documented behavior).
fn step(boundary_micros: i64, gran: WindowGranularity) -> Result<i64, WindowError> {
    if let Some(delta) = gran.unit.fixed_micros() {
        return boundary_micros
            .checked_add(
                delta
                    .checked_mul(i64::from(gran.count))
                    .ok_or(WindowError::CalendarOverflow)?,
            )
            .ok_or(WindowError::CalendarOverflow);
    }
    let months_per = gran
        .unit
        .calendar_months()
        .expect("non-fixed unit has months");
    let total_months = months_per
        .checked_mul(gran.count)
        .ok_or(WindowError::CalendarOverflow)?;
    let dt = DateTime::<Utc>::from_timestamp_micros(boundary_micros)
        .ok_or(WindowError::CalendarOverflow)?;
    // Preserve the sub-second/day-of-month position; chrono clamps day-of-month
    // to the target month's length (e.g. Jan 31 + 1 month -> Feb 28/29).
    let advanced = dt
        .checked_add_months(Months::new(total_months))
        .ok_or(WindowError::CalendarOverflow)?;
    Ok(advanced.timestamp_micros())
}

/// Generate the tumbling windows tiling `[start_micros, end_micros)` at the
/// given granularity. See the module docs for the half-open boundary contract.
pub fn generate_windows(
    start_micros: i64,
    end_micros: i64,
    gran: WindowGranularity,
) -> Result<Vec<Window>, WindowError> {
    if gran.count == 0 {
        return Err(WindowError::ZeroCount);
    }
    if end_micros <= start_micros {
        return Err(WindowError::EmptyRange);
    }
    // Sanity pre-check for fixed units: cheap upper-bound on window count so a
    // pathological range/granularity fails fast with a structured error rather
    // than looping to allocate millions of rows.
    if let Some(delta) = gran.unit.fixed_micros() {
        let span = (end_micros - start_micros) as i128;
        let step_span = delta as i128 * i128::from(gran.count);
        let approx = (span / step_span) + 1;
        if approx > MAX_WINDOWS as i128 {
            return Err(WindowError::TooManyWindows(approx as usize));
        }
    }

    let mut windows = Vec::new();
    let mut b = start_micros;
    while b < end_micros {
        let next = step(b, gran)?;
        // Guard against a non-advancing step (should be impossible given
        // count>0, but defend against any degenerate calendar edge).
        if next <= b {
            return Err(WindowError::CalendarOverflow);
        }
        let clamped_end = next.min(end_micros);
        windows.push(Window {
            start_micros: b,
            end_micros: clamped_end,
        });
        if windows.len() > MAX_WINDOWS {
            return Err(WindowError::TooManyWindows(windows.len()));
        }
        b = next;
    }
    Ok(windows)
}

/// Convenience: window start as a [`Timestamp`] for point-in-time reads.
pub fn window_start_timestamp(w: &Window) -> Timestamp {
    Timestamp::from(w.start_micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn micros_of(rfc: &str) -> i64 {
        parse_boundary_micros(rfc).unwrap()
    }

    #[test]
    fn parse_rfc3339_and_micros_agree() {
        // 2024-01-01T00:00:00Z == 1704067200 seconds == 1704067200_000_000 us.
        let a = parse_boundary_micros("2024-01-01T00:00:00Z").unwrap();
        let b = parse_boundary_micros("1704067200000000").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, 1_704_067_200_000_000);
    }

    #[test]
    fn parse_bare_date_is_utc_midnight() {
        let a = parse_boundary_micros("2024-01-01").unwrap();
        assert_eq!(a, 1_704_067_200_000_000);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(matches!(
            parse_boundary_micros("not-a-time"),
            Err(WindowError::BadTimestamp(_))
        ));
    }

    #[test]
    fn format_round_trips_rfc3339() {
        let m = 1_704_067_200_000_000;
        assert_eq!(format_rfc3339(m), "2024-01-01T00:00:00.000000Z");
    }

    #[test]
    fn fixed_day_windows_tile_exactly() {
        let start = micros_of("2024-01-01T00:00:00Z");
        let end = micros_of("2024-01-04T00:00:00Z");
        let gran = WindowGranularity {
            count: 1,
            unit: WindowUnit::Day,
        };
        let ws = generate_windows(start, end, gran).unwrap();
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].start_micros, start);
        assert_eq!(ws[0].end_micros, micros_of("2024-01-02T00:00:00Z"));
        assert_eq!(ws[2].end_micros, end);
        // Union == [start, end): contiguous, no gap/overlap.
        for pair in ws.windows(2) {
            assert_eq!(pair[0].end_micros, pair[1].start_micros);
        }
    }

    #[test]
    fn last_window_clamped_to_end() {
        // 2.5 days at 1-day granularity -> last window is a half day, clamped.
        let start = micros_of("2024-01-01T00:00:00Z");
        let end = micros_of("2024-01-03T12:00:00Z");
        let gran = WindowGranularity {
            count: 1,
            unit: WindowUnit::Day,
        };
        let ws = generate_windows(start, end, gran).unwrap();
        assert_eq!(ws.len(), 3);
        let last = ws.last().unwrap();
        assert_eq!(last.start_micros, micros_of("2024-01-03T00:00:00Z"));
        assert_eq!(last.end_micros, end);
        assert!(last.end_micros - last.start_micros < MICROS_PER_DAY);
    }

    #[test]
    fn calendar_months_have_variable_length() {
        // Jan, Feb (leap 2024), Mar starts.
        let start = micros_of("2024-01-01T00:00:00Z");
        let end = micros_of("2024-04-01T00:00:00Z");
        let gran = WindowGranularity {
            count: 1,
            unit: WindowUnit::Month,
        };
        let ws = generate_windows(start, end, gran).unwrap();
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].start_micros, micros_of("2024-01-01T00:00:00Z"));
        assert_eq!(ws[0].end_micros, micros_of("2024-02-01T00:00:00Z"));
        assert_eq!(ws[1].end_micros, micros_of("2024-03-01T00:00:00Z"));
        assert_eq!(ws[2].end_micros, end);
        // January has 31 days, February 2024 has 29 (leap).
        assert_eq!(ws[0].end_micros - ws[0].start_micros, 31 * MICROS_PER_DAY);
        assert_eq!(ws[1].end_micros - ws[1].start_micros, 29 * MICROS_PER_DAY);
    }

    #[test]
    fn quarter_is_three_months() {
        let start = micros_of("2024-01-01T00:00:00Z");
        let end = micros_of("2024-07-01T00:00:00Z");
        let gran = WindowGranularity {
            count: 1,
            unit: WindowUnit::Quarter,
        };
        let ws = generate_windows(start, end, gran).unwrap();
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].end_micros, micros_of("2024-04-01T00:00:00Z"));
    }

    #[test]
    fn year_end_of_month_clamps() {
        // Jan 31 + 1 month clamps to Feb 29 (2024 leap year), chrono behavior.
        let start = micros_of("2024-01-31T00:00:00Z");
        let next = step(
            start,
            WindowGranularity {
                count: 1,
                unit: WindowUnit::Month,
            },
        )
        .unwrap();
        assert_eq!(next, micros_of("2024-02-29T00:00:00Z"));
    }

    #[test]
    fn zero_count_and_empty_range_rejected() {
        let s = micros_of("2024-01-01T00:00:00Z");
        let e = micros_of("2024-02-01T00:00:00Z");
        assert_eq!(
            generate_windows(
                s,
                e,
                WindowGranularity {
                    count: 0,
                    unit: WindowUnit::Day
                }
            ),
            Err(WindowError::ZeroCount)
        );
        assert_eq!(
            generate_windows(
                e,
                s,
                WindowGranularity {
                    count: 1,
                    unit: WindowUnit::Day
                }
            ),
            Err(WindowError::EmptyRange)
        );
        assert_eq!(
            generate_windows(
                s,
                s,
                WindowGranularity {
                    count: 1,
                    unit: WindowUnit::Day
                }
            ),
            Err(WindowError::EmptyRange)
        );
    }

    #[test]
    fn too_many_windows_rejected() {
        let s = 0i64;
        let e = MICROS_PER_DAY * (MAX_WINDOWS as i64 + 10);
        let err = generate_windows(
            s,
            e,
            WindowGranularity {
                count: 1,
                unit: WindowUnit::Day,
            },
        )
        .unwrap_err();
        assert!(matches!(err, WindowError::TooManyWindows(_)));
    }

    #[test]
    fn multi_count_units() {
        let s = micros_of("2024-01-01T00:00:00Z");
        let e = micros_of("2024-01-01T06:00:00Z");
        let ws = generate_windows(
            s,
            e,
            WindowGranularity {
                count: 2,
                unit: WindowUnit::Hour,
            },
        )
        .unwrap();
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].end_micros, micros_of("2024-01-01T02:00:00Z"));
    }

    #[test]
    fn unit_parse_singular_plural_case_insensitive() {
        assert_eq!(WindowUnit::parse("Month"), Some(WindowUnit::Month));
        assert_eq!(WindowUnit::parse("months"), Some(WindowUnit::Month));
        assert_eq!(WindowUnit::parse("HOURS"), Some(WindowUnit::Hour));
        assert_eq!(WindowUnit::parse("fortnight"), None);
    }

    #[test]
    fn parse_naive_datetime_forms_are_utc() {
        // Naive date-times (no explicit offset) are interpreted as UTC, in both
        // the `T`-separated and space-separated forms, and must agree with the
        // equivalent RFC 3339 `Z` instant.
        let expected = micros_of("2024-03-04T05:06:07Z");
        assert_eq!(
            parse_boundary_micros("2024-03-04T05:06:07").unwrap(),
            expected
        );
        assert_eq!(
            parse_boundary_micros("2024-03-04 05:06:07").unwrap(),
            expected
        );
        // Whitespace around the token is tolerated.
        assert_eq!(
            parse_boundary_micros("  2024-03-04T05:06:07  ").unwrap(),
            expected
        );
    }

    #[test]
    fn format_rfc3339_falls_back_for_out_of_range_micros() {
        // `i64::MAX` microseconds is far outside chrono's representable range,
        // so formatting falls back to the `<micros>us` token rather than
        // panicking.
        assert_eq!(format_rfc3339(i64::MAX), format!("{}us", i64::MAX));
    }

    #[test]
    fn window_start_timestamp_matches_start_micros() {
        let w = Window {
            start_micros: 1_704_067_200_000_000,
            end_micros: 1_704_070_800_000_000,
        };
        assert_eq!(
            window_start_timestamp(&w),
            Timestamp::from(1_704_067_200_000_000)
        );
    }
}
