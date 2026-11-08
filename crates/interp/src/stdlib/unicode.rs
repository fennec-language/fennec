//! `std.unicode`: a character's general category, the normalization forms, and case folding,
//! which the interpreter does in Rust, from `crate::unicode`'s tables.

use fennec_syntax::{Diagnostic, Span};

use crate::value::Value;


/// One of these functions, which need nothing of the interpreter.
pub(crate) fn run(name: &str, values: &[Value], span: Span) -> Result<Value, Diagnostic> {
    use crate::unicode::{casefolded, category, normalized};
    if name == "unicode.version" {
        return Ok(Value::str(crate::unicode::VERSION));
    }
    let Some(Value::Str(text)) = values.first() else {
        return Err(Diagnostic::new(format!("`{name}` takes a str"), span));
    };
    Ok(Value::str(&match name {
        "unicode.category" => {
            let mut chars = text.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return Err(Diagnostic::new(
                    format!("`category` takes one character, but this is {} of them", text.chars().count()),
                    span,
                )
                .with_help("give it one, like `unicode.category(text[0])`"));
            };
            category(c).to_owned()
        }
        "unicode.nfc" => normalized(text, false, true),
        "unicode.nfd" => normalized(text, false, false),
        "unicode.nfkc" => normalized(text, true, true),
        "unicode.nfkd" => normalized(text, true, false),
        "unicode.casefold" => casefolded(text),
        _ => return Err(Diagnostic::new(format!("`{name}` isn't in std.unicode"), span)),
    }))
}
