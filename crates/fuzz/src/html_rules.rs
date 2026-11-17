//! The `html` target: `std.html.render`, built natively, given any text as the value in each of
//! `PLACES`. A template's text is written in the source, so what a stranger reaches is the value:
//! between tags and in a quoted attribute it has to come out escaped, in a link it has to be one a
//! browser would follow only to `http`, `https`, `mailto` or somewhere relative, and where
//! escaping isn't enough it has to be refused. A value rendered once and put in another page goes
//! in as it is.

use std::fmt::Write as _;

/// Where a value goes: the template, with `{v}` for it, and what `render` has to do with it.
const PLACES: [(&str, Rule); 13] = [
    ("<p>{v}</p>", Rule::Escaped),
    ("<p title=\\\"{v}\\\">x</p>", Rule::Escaped),
    ("<p title='{v}'>x</p>", Rule::Escaped),
    ("<a href=\\\"{v}\\\">x</a>", Rule::Link),
    ("<img src='{v}'>", Rule::Link),
    ("<a href=\\\"/notes?q={v}\\\">x</a>", Rule::Escaped),
    ("<a href=\\\"https://den.example/{v}\\\">x</a>", Rule::Escaped),
    ("<script>{v}</script>", Rule::Refused),
    ("<style>{v}</style>", Rule::Refused),
    ("<p title={v}>x</p>", Rule::Refused),
    ("<p onclick=\\\"{v}\\\">x</p>", Rule::Refused),
    ("<!-- {v} -->", Rule::Refused),
    ("<meta http-equiv=\\\"refresh\\\" content=\\\"0; url={v}\\\">", Rule::Anything),
];

/// What `render` has to do with a value in one of `PLACES`.
#[derive(Clone, Copy)]
enum Rule {
    /// Make the page, with the value escaped where `{v}` is.
    Escaped,
    /// The same, or refuse it, but never make a link a browser would follow anywhere but
    /// `http`, `https`, `mailto` or somewhere relative.
    Link,
    /// Refuse it, whatever it is.
    Refused,
    /// Anything but crash or get stuck.
    Anything,
}

/// The program that renders its file's text in each of `PLACES`, and once inside a piece rendered
/// first, a line each: `ok` and the page in hex, or `err` and why in hex.
pub(crate) fn reader() -> String {
    let mut source = "\
from std import html
from std.html import Html, HtmlError

def shown(run: () -> Html fails HtmlError) -> str:
    page = run() except why:
        return \"err \" + why.message().encode().hex()
    return \"ok \" + page.text().encode().hex()

def nested(v: str) -> Html fails HtmlError:
    inner = html.render(t\"<b>{v}</b>\")?
    return html.render(t\"<div>{inner}</div>\")?

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    v = fs.read_text(args[0])?
"
    .to_owned();
    for (template, _) in PLACES {
        let _ = writeln!(source, "    stdout.print(shown(() => html.render(t\"{template}\")?))");
    }
    source.push_str("    stdout.print(shown(() => nested(v)?))\n");
    source
}

/// Text with `& < > " '` escaped, as `html.escape` writes it.
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Whether a browser would follow `link` only to `http`, `https`, `mailto` or somewhere relative.
/// It reads a URL as the URL standard does: spaces and control characters at either end are
/// passed over, and tabs and line breaks anywhere; then a letter, followed by letters, digits,
/// `+`, `-` and `.` up to a `:`, is its scheme, and anything else has none, so it's relative.
fn followed_safely(link: &str) -> bool {
    let trimmed = link.trim_matches(|c: char| c <= ' ');
    let cleaned: String = trimmed.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let Some((scheme, _)) = cleaned.split_once(':') else { return true };
    let mut chars = scheme.chars();
    let is_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    !is_scheme || ["http", "https", "mailto"].contains(&scheme.to_ascii_lowercase().as_str())
}

/// Text from the hex the reader printed, or `None` if it isn't hex of UTF-8.
fn from_hex(text: &str) -> Option<String> {
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|at| text.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()))
        .collect();
    String::from_utf8(bytes?).ok()
}

/// The target: each place does with the input what its rule says.
pub(crate) fn html(input: &[u8]) -> Result<(), String> {
    let value = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("html", value.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != PLACES.len() + 1 {
        return Err(format!("the reader printed something else:\n{printed}"));
    }
    let wanted = |template: &str| template.replace("\\\"", "\"").replace("{v}", &escaped(&value));
    let nested = format!("<div><b>{}</b></div>", escaped(&value));
    let places = PLACES.iter().copied().chain([("{nested}", Rule::Escaped)]);
    for ((template, rule), line) in places.zip(&lines) {
        let (made, text) = match line.split_once(' ') {
            Some(("ok", hex)) => (true, from_hex(hex)),
            Some(("err", hex)) => (false, from_hex(hex)),
            _ => return Err(format!("the reader printed something else: {line}")),
        };
        let Some(text) = text else { return Err(format!("the reader printed something else: {line}")) };
        let page = if template == "{nested}" { nested.clone() } else { wanted(template) };
        let wrong = match rule {
            Rule::Escaped => (!made || text != page).then(|| format!("should make {page:?}")),
            Rule::Link if made && !followed_safely(&value) => Some(
                "a browser would follow that link somewhere not http, https, mailto or relative".to_owned(),
            ),
            Rule::Link => (made && text != page).then(|| format!("should make {page:?}")),
            Rule::Refused => made.then(|| "should be refused".to_owned()),
            Rule::Anything => None,
        };
        if let Some(wrong) = wrong {
            let said = if made { "made" } else { "refused" };
            return Err(format!("{template} with {value:?}: {said} {text:?}, but {wrong}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::followed_safely;

    /// Links are read as a browser reads them.
    #[test]
    fn links_are_read_as_a_browser_reads_them() {
        for safe in ["/next", "https://a", "MAILTO:x", "java script:x", "&#106;avascript:x", "?a:b", "a/b:c"]
        {
            assert!(followed_safely(safe), "{safe}");
        }
        for dangerous in ["javascript:x", " \u{1}JavaScript:x", "java\tscript:x", "data:text/html,x", "a+b:c"]
        {
            assert!(!followed_safely(dangerous), "{dangerous}");
        }
    }
}
