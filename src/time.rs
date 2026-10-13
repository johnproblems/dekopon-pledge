//! Broker-clock time: RFC 3339 parsing and formatting plus relative durations.
//!
//! Pledge vendors a small time parser instead of depending on chrono so the
//! component stays dependency-lean. All instants are Unix milliseconds; all
//! rendered timestamps are UTC with millisecond precision.

use std::fmt;

/// `9999-12-31T23:59:59.999Z`, the largest representable instant.
pub const MAX_MILLIS: u64 = 253_402_300_799_999;

/// Why a time specification was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeError {
    /// Not an RFC 3339 timestamp or relative duration.
    Invalid,
    /// Well-formed but outside the representable range.
    OutOfRange,
}

impl fmt::Display for TimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "not an RFC 3339 timestamp or relative duration",
            Self::OutOfRange => "outside the representable time range",
        })
    }
}

impl std::error::Error for TimeError {}

/// Parses `YYYY-MM-DDTHH:MM:SS[.frac](Z|±HH:MM)` to Unix milliseconds.
///
/// Fractional seconds truncate to millisecond precision. Years before 1970
/// and instants past [`MAX_MILLIS`] are [`TimeError::OutOfRange`]; anything
/// else malformed is [`TimeError::Invalid`].
pub fn parse_rfc3339(text: &str) -> Result<u64, TimeError> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return Err(TimeError::Invalid);
    }
    let year = digits(&bytes[0..4])?;
    let month = digits(&bytes[5..7])?;
    let day = digits(&bytes[8..10])?;
    let hour = digits(&bytes[11..13])?;
    let minute = digits(&bytes[14..16])?;
    let second = digits(&bytes[17..19])?;
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || (bytes[10] != b'T' && bytes[10] != b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(TimeError::Invalid);
    }
    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(TimeError::Invalid);
    }
    let mut rest = &bytes[19..];
    let mut millis = 0_u64;
    if rest.first() == Some(&b'.') {
        rest = &rest[1..];
        let count = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
        if count == 0 {
            return Err(TimeError::Invalid);
        }
        for (index, byte) in rest[..count].iter().enumerate().take(3) {
            millis += u64::from(byte - b'0') * [100, 10, 1][index];
        }
        rest = &rest[count..];
    }
    let offset_seconds: i64 = match rest {
        [b'Z'] | [b'z'] => 0,
        [sign, h0, h1, b':', m0, m1]
            if (*sign == b'+' || *sign == b'-')
                && h0.is_ascii_digit()
                && h1.is_ascii_digit()
                && m0.is_ascii_digit()
                && m1.is_ascii_digit() =>
        {
            let hours = i64::from(h0 - b'0') * 10 + i64::from(h1 - b'0');
            let minutes = i64::from(m0 - b'0') * 10 + i64::from(m1 - b'0');
            if hours > 23 || minutes > 59 {
                return Err(TimeError::Invalid);
            }
            let offset = hours * 3_600 + minutes * 60;
            if *sign == b'-' { -offset } else { offset }
        }
        _ => return Err(TimeError::Invalid),
    };
    let days = days_from_civil(year, month, day);
    let local = days
        .checked_mul(86_400)
        .and_then(|date| date.checked_add(hour * 3_600 + minute * 60 + second))
        .and_then(|stamp| stamp.checked_sub(offset_seconds))
        .ok_or(TimeError::OutOfRange)?;
    let whole = u64::try_from(local).map_err(|_| TimeError::OutOfRange)?;
    let stamp = whole
        .checked_mul(1_000)
        .and_then(|base| base.checked_add(millis))
        .ok_or(TimeError::OutOfRange)?;
    if stamp > MAX_MILLIS {
        return Err(TimeError::OutOfRange);
    }
    Ok(stamp)
}

/// Formats Unix milliseconds as `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub fn format_rfc3339(millis: u64) -> Result<String, TimeError> {
    if millis > MAX_MILLIS {
        return Err(TimeError::OutOfRange);
    }
    let seconds = millis / 1_000;
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let second_of_day = seconds % 86_400;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60,
        millis % 1_000,
    ))
}

/// Parses a relative duration (`30s`, `30m`, `2h`, `3d`, `2w`, either case)
/// to milliseconds.
pub fn parse_duration(text: &str) -> Result<u64, TimeError> {
    let text = text.trim();
    let (digits, suffix) = text.split_at(text.len().saturating_sub(1));
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TimeError::Invalid);
    }
    let unit = match suffix {
        "s" | "S" => 1_000,
        "m" | "M" => 60_000,
        "h" | "H" => 3_600_000,
        "d" | "D" => 86_400_000,
        "w" | "W" => 604_800_000,
        _ => return Err(TimeError::Invalid),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(unit))
        .ok_or(TimeError::OutOfRange)
}

/// Resolves a `--due` value against the broker clock: an absolute RFC 3339
/// timestamp passes through, a relative duration is added to `now_ms`.
pub fn resolve_due(spec: &str, now_ms: u64) -> Result<u64, TimeError> {
    if let Ok(stamp) = parse_rfc3339(spec) {
        return Ok(stamp);
    }
    let delta = parse_duration(spec)?;
    let stamp = now_ms.checked_add(delta).ok_or(TimeError::OutOfRange)?;
    if stamp > MAX_MILLIS {
        return Err(TimeError::OutOfRange);
    }
    Ok(stamp)
}

fn digits(bytes: &[u8]) -> Result<i64, TimeError> {
    if bytes.is_empty() || !bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return Err(TimeError::Invalid);
    }
    let mut value = 0_i64;
    for byte in bytes {
        value = value * 10 + i64::from(byte - b'0');
    }
    Ok(value)
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
    }
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
const fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = if adjusted >= 0 {
        adjusted
    } else {
        adjusted - 399
    } / 400;
    let year_of_era = adjusted - era * 400;
    let march_month = (month + 9) % 12;
    let day_of_year = (153 * march_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
const fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = year_of_era + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_across_leap_days_and_offsets() {
        for (text, expected) in [
            ("1970-01-01T00:00:00Z", 0),
            ("2000-02-29T00:00:00Z", 951_782_400_000),
            ("2026-10-14T20:00:00Z", 1_792_008_000_000),
            ("2026-10-14T16:00:00-04:00", 1_792_008_000_000),
            ("2026-10-14T20:00:00.250Z", 1_792_008_000_250),
            ("2026-10-14T20:00:00.250987654Z", 1_792_008_000_250),
            ("9999-12-31T23:59:59.999Z", MAX_MILLIS),
        ] {
            let stamp = parse_rfc3339(text).unwrap();
            assert_eq!(stamp, expected, "{text}");
            assert_eq!(
                parse_rfc3339(&format_rfc3339(stamp).unwrap()).unwrap(),
                stamp
            );
        }
        assert_eq!(
            format_rfc3339(1_792_008_000_000).unwrap(),
            "2026-10-14T20:00:00.000Z"
        );
        assert_eq!(format_rfc3339(0).unwrap(), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn rfc3339_refuses_malformed_or_out_of_range_input() {
        for text in [
            "",
            "tomorrow",
            "2h",
            "2026-10-14",
            "2026-10-14T20:00:00",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-10-32T00:00:00Z",
            "2026-10-14T24:00:00Z",
            "2026-10-14T20:00:60Z",
            "2026-10-14 20:00:00Z",
            "2026-10-14T20:00:00.",
            "2026-10-14T20:00:00+24:00",
            "2026-10-14T20:00:00+00:60",
            "2026-10-14T20:00:00+0000",
            "26-10-14T20:00:00Z",
            "10000-01-01T00:00:00Z",
        ] {
            assert_eq!(parse_rfc3339(text), Err(TimeError::Invalid), "{text}");
        }
        for text in ["1969-12-31T23:59:59.999Z", "0001-01-01T00:00:00Z"] {
            assert_eq!(parse_rfc3339(text), Err(TimeError::OutOfRange), "{text}");
        }
        assert_eq!(format_rfc3339(MAX_MILLIS + 1), Err(TimeError::OutOfRange));
    }

    #[test]
    fn durations_parse_and_reject_everything_else() {
        for (text, expected) in [
            ("0m", 0),
            ("45s", 45_000),
            ("30m", 1_800_000),
            ("2h", 7_200_000),
            ("3d", 259_200_000),
            ("2w", 1_209_600_000),
            ("1D", 86_400_000),
            ("  2H  ", 7_200_000),
        ] {
            assert_eq!(parse_duration(text).unwrap(), expected, "{text}");
        }
        for text in [
            "",
            "m",
            "-2h",
            "+2h",
            "1h30m",
            "1.5h",
            "2y",
            "2",
            "2026-10-14T20:00:00Z",
        ] {
            assert_eq!(parse_duration(text), Err(TimeError::Invalid), "{text}");
        }
        assert_eq!(
            parse_duration("99999999999999999999d"),
            Err(TimeError::OutOfRange)
        );
    }

    /// Deterministic xorshift64 for parser fuzzing without new dependencies.
    fn next_random(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn ten_thousand_generated_cases_round_trip_or_refuse_cleanly() {
        const TEMPLATES: [&str; 5] = [
            "2026-10-14T20:00:00Z",
            "9999-12-31T23:59:59.999+14:00",
            "2h",
            "30m",
            "0s",
        ];
        let mut state = 0x243F_6A88_85A3_08D3;
        let mut rfc_accepts = 0_u32;
        let mut duration_accepts = 0_u32;
        for index in 0..10_000 {
            // Every hundredth case feeds a raw template, so the accept paths
            // are always exercised no matter what the soup yields.
            if index % 100 == 0 {
                let template = TEMPLATES[(index / 100) % TEMPLATES.len()];
                assert!(parse_rfc3339(template).is_ok() || parse_duration(template).is_ok());
            }
            let pick = next_random(&mut state);
            match pick % 3 {
                // In-range millis round-trip exactly.
                0 => {
                    let millis = next_random(&mut state) % (MAX_MILLIS + 1);
                    let text = format_rfc3339(millis).unwrap();
                    assert_eq!(parse_rfc3339(&text).unwrap(), millis);
                }
                // Out-of-range millis are refused, never wrapped.
                1 => {
                    let millis = u64::MAX - (next_random(&mut state) % 1_000);
                    assert_eq!(format_rfc3339(millis), Err(TimeError::OutOfRange));
                }
                // Byte soup and template mutations never panic; anything
                // accepted re-parses to itself.
                _ => {
                    let bytes = if pick.is_multiple_of(2) {
                        let length = (next_random(&mut state) % 32) as usize;
                        (0..length)
                            .map(|_| b' ' + (next_random(&mut state) % 95) as u8)
                            .collect::<Vec<_>>()
                    } else {
                        let template =
                            TEMPLATES[next_random(&mut state) as usize % TEMPLATES.len()];
                        let mut bytes = template.as_bytes().to_vec();
                        for _ in 0..=next_random(&mut state) % 3 {
                            let at = next_random(&mut state) as usize % bytes.len();
                            bytes[at] = b' ' + (next_random(&mut state) % 95) as u8;
                        }
                        bytes
                    };
                    let Ok(text) = String::from_utf8(bytes) else {
                        continue;
                    };
                    if let Ok(stamp) = parse_rfc3339(&text) {
                        rfc_accepts += 1;
                        assert_eq!(
                            parse_rfc3339(&format_rfc3339(stamp).unwrap()).unwrap(),
                            stamp
                        );
                    }
                    if parse_duration(&text).is_ok() {
                        duration_accepts += 1;
                        assert_ne!(resolve_due(&text, 0), Err(TimeError::Invalid));
                    }
                }
            }
        }
        assert!(rfc_accepts > 0, "fuzz never accepted RFC 3339");
        assert!(duration_accepts > 0, "fuzz never accepted a duration");
    }

    #[test]
    fn due_resolution_prefers_absolute_and_saturates_nothing() {
        let now = 1_792_008_000_000;
        assert_eq!(
            resolve_due("2026-10-14T20:00:00Z", now).unwrap(),
            1_792_008_000_000
        );
        assert_eq!(resolve_due("1h", now).unwrap(), now + 3_600_000);
        assert_eq!(resolve_due("tomorrow", now), Err(TimeError::Invalid));
        assert_eq!(resolve_due("1h", u64::MAX), Err(TimeError::OutOfRange));
    }
}
