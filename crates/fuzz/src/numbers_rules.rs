//! The `numbers` target: `int.parse`, `float.parse`, the sized ints' `parse` and `Uuid.parse`,
//! built natively, on any text, each held to what it says it reads. Text a stranger sends a
//! server comes in through these, so each must give the number it says or fail, and never stop
//! the program, whatever the text.
//!
//! What each should give is worked out here: `int` reads what Rust's `i64` does from the text
//! with the space around it left off, `float` what Rust's `f64` does, a sized int an `int` that
//! fits it, a `u64` an `int` that fits it or what Rust's `u64` does past the largest `int`, and a
//! UUID 36 characters with hyphens where they go and hex digits between.

/// The program that parses the text each way, printing a line for each: its name, then
/// `Ok(value)` or `Err`. A line missing means a parse stopped the program.
pub(crate) const READER: &str = "\
from std.uuid import Uuid

def say(stdout: Stdout, name: str, value: str?):
    if value is None:
        stdout.print(name, \"Err\")
    else:
        stdout.print(name, f\"Ok({value})\")

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    text = fs.read_text(args[0])?
    whole: int? = int.parse(text) except InvalidValue(): None
    say(stdout, name=\"int\", value=None if whole is None else str(whole))
    real: float? = float.parse(text) except InvalidValue(): None
    say(stdout, name=\"float\", value=None if real is None else str(real))
    tiny: i8? = i8.parse(text) except InvalidValue(): None
    say(stdout, name=\"i8\", value=None if tiny is None else str(tiny))
    short: i16? = i16.parse(text) except InvalidValue(): None
    say(stdout, name=\"i16\", value=None if short is None else str(short))
    middle: i32? = i32.parse(text) except InvalidValue(): None
    say(stdout, name=\"i32\", value=None if middle is None else str(middle))
    byte: u8? = u8.parse(text) except InvalidValue(): None
    say(stdout, name=\"u8\", value=None if byte is None else str(byte))
    pair: u16? = u16.parse(text) except InvalidValue(): None
    say(stdout, name=\"u16\", value=None if pair is None else str(pair))
    quad: u32? = u32.parse(text) except InvalidValue(): None
    say(stdout, name=\"u32\", value=None if quad is None else str(quad))
    wide: u64? = u64.parse(text) except InvalidValue(): None
    say(stdout, name=\"u64\", value=None if wide is None else str(wide))
    found: Uuid? = Uuid.parse(text) except InvalidValue(): None
    say(stdout, name=\"uuid\", value=None if found is None else str(found))
";

/// The target: each parse gives what it should, and none stops the program.
pub(crate) fn numbers(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("numbers", text.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let wanted = wanted(&text);
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != wanted.len() {
        return Err(format!("{text:?}: a parse stopped the program, after printing:\n{printed}"));
    }
    for ((name, should), line) in wanted.iter().zip(&lines) {
        let Some(gave) = line.strip_prefix(name).and_then(|rest| rest.strip_prefix(' ')) else {
            return Err(format!("{text:?}: the reader printed {line:?} for `{name}`"));
        };
        let right = match (should, gave.strip_prefix("Ok(").and_then(|value| value.strip_suffix(')'))) {
            (None, None) => gave == "Err",
            (Some(should), Some(value)) if *name == "float" => same_float(should, value),
            (Some(should), Some(value)) => should == value,
            _ => false,
        };
        if !right {
            let should = should.as_ref().map_or_else(|| "Err".to_owned(), |value| format!("Ok({value})"));
            return Err(format!("{text:?}: `{name}.parse` gave {gave}, where it should give {should}"));
        }
    }
    Ok(())
}

/// What each parse should give, in the order the reader prints them.
fn wanted(text: &str) -> Vec<(&'static str, Option<String>)> {
    let trimmed = text.trim();
    let whole = trimmed.parse::<i64>().ok();
    let fitting = |low: i64, high: i64| whole.filter(|n| (low..=high).contains(n)).map(|n| n.to_string());
    vec![
        ("int", whole.map(|n| n.to_string())),
        ("float", trimmed.parse::<f64>().ok().map(|n| format!("{n:?}"))),
        ("i8", fitting(i64::from(i8::MIN), i64::from(i8::MAX))),
        ("i16", fitting(i64::from(i16::MIN), i64::from(i16::MAX))),
        ("i32", fitting(i64::from(i32::MIN), i64::from(i32::MAX))),
        ("u8", fitting(0, i64::from(u8::MAX))),
        ("u16", fitting(0, i64::from(u16::MAX))),
        ("u32", fitting(0, i64::from(u32::MAX))),
        // What `int` reads when it isn't below 0, `-0` too, and past the largest `int`, digits
        // with an optional `+`, as Rust's `u64` reads them.
        (
            "u64",
            whole
                .map_or_else(|| trimmed.parse::<u64>().ok(), |n| u64::try_from(n).ok())
                .map(|n| n.to_string()),
        ),
        ("uuid", uuid(text)),
    ]
}

/// Whether the float printed reads back as the one wanted: the same bits, or both NaN.
fn same_float(wanted: &str, printed: &str) -> bool {
    match (wanted.parse::<f64>(), printed.parse::<f64>()) {
        (Ok(wanted), Ok(printed)) => {
            wanted.to_bits() == printed.to_bits() || (wanted.is_nan() && printed.is_nan())
        }
        _ => false,
    }
}

/// The UUID `text` is, in lower case, if it's 36 characters with hyphens at the 9th, 14th, 19th
/// and 24th and hex digits everywhere else.
fn uuid(text: &str) -> Option<String> {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() != 36 {
        return None;
    }
    let laid_out = characters
        .iter()
        .enumerate()
        .all(|(at, c)| if [8, 13, 18, 23].contains(&at) { *c == '-' } else { c.is_ascii_hexdigit() });
    laid_out.then(|| text.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{uuid, wanted};

    #[test]
    fn what_each_parse_should_give_is_worked_out_from_the_text() {
        let given = |text: &str, name: &str| {
            wanted(text).into_iter().find(|(own, _)| *own == name).and_then(|(_, should)| should)
        };
        assert_eq!(given(" -42\n", "int").as_deref(), Some("-42"));
        assert_eq!(given("1_000", "int"), None);
        assert_eq!(given("300", "u8"), None);
        assert_eq!(given("-1", "u64"), None);
        assert_eq!(given("-0", "u64").as_deref(), Some("0"));
        assert_eq!(given(" 18446744073709551615 ", "u64").as_deref(), Some("18446744073709551615"));
        assert_eq!(given("1e400", "float").as_deref(), Some("inf"));
        assert_eq!(
            uuid("0F8FAD5B-D9CB-469F-A165-70867728950E").as_deref(),
            Some("0f8fad5b-d9cb-469f-a165-70867728950e")
        );
        assert_eq!(uuid("0f8fad5b-d9cb-469f-a165-70867728950"), None);
    }
}
