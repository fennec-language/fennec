//! Writing `ref` for `mut` where the language has moved to it: on a loop, a parameter, an argument
//! and a `with` name. `mut` stays on a local. Both words are three letters long, so everything
//! else in the file, and every span, stays where it was.

use crate::diagnostic::Diagnostic;
use crate::doc_examples::example_code;
use crate::span::Span;
use crate::trivia::{Lexed, SourceToken, TokenKind, lex_source};

/// A file with `ref` written in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renamed {
    pub text: String,
    /// Comments and f-strings that still say `mut`, which a person has to read: a comment may be
    /// about either word, and the code in an f-string isn't tokens yet.
    pub left: Vec<Span>,
}

/// `source` with `ref` written in, as far as it can be read as tokens. Code in a doc comment's
/// examples is changed too, as it's compiled as the file is.
pub fn mut_to_ref(source: &str) -> Result<Renamed, Diagnostic> {
    let lexed = lex_source(source)?;
    let mut places = ref_places(source, &lexed);
    let examples = example_code(source);
    for code in &examples {
        // An example that can't be read is reported when the file is compiled.
        if let Ok(lexed) = lex_source(code) {
            places.extend(ref_places(code, &lexed));
        }
    }
    let mut text = source.to_owned();
    for place in places {
        text.replace_range(place.start as usize..place.end as usize, "ref");
    }
    let in_an_example = |span: Span| {
        examples.iter().any(|code| {
            code.get(span.start as usize..span.end as usize).is_some_and(|text| !text.trim().is_empty())
        })
    };
    let comments = lexed.comments.iter().copied().filter(|comment| !in_an_example(*comment));
    let fstrings = lexed.tokens.iter().filter(|token| token.kind == TokenKind::FStr).map(|token| token.span);
    let mut left: Vec<Span> = comments.chain(fstrings).filter(|span| says_mut(source, *span)).collect();
    left.sort_by_key(|span| span.start);
    Ok(Renamed { text, left })
}

/// The `mut`s in `lexed` that aren't declaring a local. A local's is the first thing in its
/// statement; the others follow `for`, `as`, an opening `(` or a comma.
fn ref_places(source: &str, lexed: &Lexed) -> Vec<Span> {
    let text = |token: &SourceToken| source.get(token.span.start as usize..token.span.end as usize);
    let mut places = Vec::new();
    let mut before: Option<&SourceToken> = None;
    for token in &lexed.tokens {
        if token.kind == TokenKind::Keyword
            && text(token) == Some("mut")
            && before.and_then(text).is_some_and(|word| matches!(word, "for" | "as" | "(" | ","))
        {
            places.push(token.span);
        }
        before = Some(token);
    }
    places
}

/// Whether the source in `span` has the word `mut` in it.
fn says_mut(source: &str, span: Span) -> bool {
    source
        .get(span.start as usize..span.end as usize)
        .is_some_and(|text| text.split(|c: char| !c.is_alphanumeric() && c != '_').any(|word| word == "mut"))
}
