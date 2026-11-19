//! The `time` target: `Time.parse`, built natively, on any text, held to the RFC 3339 it says it
//! reads. A time a stranger sends, in a header or a JSON field, comes in through it.
//!
//! What it should give is worked out here from the same rules: the date, a `T`, `t` or space,
//! the time of day, a fraction of a second whose digits past nanoseconds are left off, then `Z`,
//! `z` or an offset, with the year from 1678 to 2261 and the date and time of day real ones, and
//! the moment, once its offset is taken off, still inside those years in UTC. What it gives
//! also prints, with `iso`, as text it reads back as the same moment.

/// The program that parses the text, printing `Err`, or `Ok`, the moment's nanoseconds and its
/// `iso` text, then what parsing that text again gives.
pub(crate) const READER: &str = "\
from std.time import Time

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    found: Time? = Time.parse(text) except ParseError(): None
    if found is None:
        stdout.print(\"Err\")
        return
    written = found.iso()
    stdout.print(\"Ok\", found.nanos, written)
    again: Time? = Time.parse(written) except ParseError(): None
    if again is None:
        stdout.print(\"again Err\")
    else:
        stdout.print(\"again\", again.nanos)
";

/// The target: `Time.parse` gives the moment the text says or fails, and what it gives reads
/// back from its own `iso` text.
pub(crate) fn time(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("time", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let mut lines = printed.lines();
    let should = moment(&text);
    match (lines.next(), should) {
        (Some("Err"), None) => Ok(()),
        (Some(line), Some(should)) if line.starts_with("Ok ") => {
            let mut words = line.split(' ').skip(1);
            let nanos = words.next().and_then(|nanos| nanos.parse::<i64>().ok());
            let written = words.next().unwrap_or_default();
            if nanos != Some(should) {
                return Err(format!("{text:?}: `Time.parse` gave {line:?}, where the moment is {should}"));
            }
            match lines.next() {
                Some(again) if again == format!("again {should}") => Ok(()),
                Some(again) => {
                    Err(format!("{text:?}: its `iso` text {written:?} reads back as {again:?}, not {should}"))
                }
                None => Err(format!("{text:?}: reading back {written:?} stopped the program")),
            }
        }
        (Some(line), should) => {
            let should = should.map_or_else(|| "Err".to_owned(), |nanos| format!("Ok({nanos})"));
            Err(format!("{text:?}: `Time.parse` gave {line:?}, where it should give {should}"))
        }
        (None, _) => Err(format!("{text:?}: `Time.parse` stopped the program")),
    }
}

/// The nanoseconds since 1970 that RFC 3339 `text` says, as `Time.parse` reads it, or `None`.
fn moment(text: &str) -> Option<i64> {
    let c: Vec<char> = text.chars().collect();
    let char_at = |at: usize| c.get(at).copied();
    let digits = |at: usize, count: usize| -> Option<i64> {
        let part = c.get(at..at.checked_add(count)?)?;
        part.iter().try_fold(0_i64, |n, digit| {
            let value = digit.to_digit(10).filter(|_| digit.is_ascii_digit())?;
            Some(n * 10 + i64::from(value))
        })
    };
    let laid_out = c.len() >= 20
        && char_at(4) == Some('-')
        && char_at(7) == Some('-')
        && char_at(13) == Some(':')
        && char_at(16) == Some(':')
        && matches!(char_at(10), Some('T' | 't' | ' '));
    if !laid_out {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let mut at = 19;
    let mut fraction = 0;
    if char_at(at) == Some('.') {
        at += 1;
        let mut places = 0;
        while let Some(digit) = char_at(at).filter(char::is_ascii_digit) {
            if places < 9 {
                fraction = fraction * 10 + i64::from(digit.to_digit(10)?);
                places += 1;
            }
            at += 1;
        }
        if places == 0 {
            return None;
        }
        fraction *= 10_i64.pow(9 - places);
    }
    let offset = match char_at(at)? {
        'Z' | 'z' => {
            at += 1;
            0
        }
        sign @ ('+' | '-') => {
            if at + 6 > c.len() || char_at(at + 3) != Some(':') {
                return None;
            }
            let (hours, minutes) = (digits(at + 1, 2)?, digits(at + 4, 2)?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            at += 6;
            let offset = (hours * 60 + minutes) * 60_000_000_000;
            if sign == '-' { -offset } else { offset }
        }
        _ => return None,
    };
    if at != c.len() || !(1678..=2261).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    let moment = seconds * 1_000_000_000 + fraction - offset;
    let (first, past) = (days_from_civil(1678, 1, 1), days_from_civil(2262, 1, 1));
    (first * 86_400_000_000_000..past * 86_400_000_000_000).contains(&moment).then_some(moment)
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar, Howard Hinnant's way.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = of_era * 365 + of_era / 4 - of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::moment;

    #[test]
    fn the_moment_is_worked_out_from_the_text() {
        assert_eq!(moment("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(moment("1970-01-01t00:00:01.5z"), Some(1_500_000_000));
        assert_eq!(moment("1970-01-01 01:00:00+01:00"), Some(0));
        assert_eq!(moment("2026-11-06T21:55:56.1234567891Z"), Some(1_794_002_156_123_456_789));
        assert_eq!(moment("2025-02-29T00:00:00Z"), None);
        assert_eq!(moment("2024-02-29T23:59:60Z"), None);
        assert_eq!(moment("1677-12-31T23:59:59Z"), None);
        assert_eq!(moment("1678-01-01T00:00:00+00:01"), None);
        assert_eq!(moment("2261-12-31T23:59:59-00:01"), None);
        assert_eq!(moment("1678-01-01T00:00:00-00:01"), Some(-9_214_559_940_000_000_000));
        assert_eq!(moment("2026-11-06T21:55:56"), None);
    }
}
