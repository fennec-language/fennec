//! `fennec fmt`: Fennec's one style, with no options.
//!
//! The formatter works on tokens, so it can only change the space between them: it never
//! adds, drops or reorders code, apart from a trailing comma on a list it breaks across
//! lines. Comments are kept word for word. After formatting, it parses the result and checks
//! the syntax tree and the comments are the same as before, and refuses to change the file
//! if they aren't.

mod blocks;
mod comments;
mod layout;
mod output;
mod spacing;
mod tree;

use fennec_syntax::Diagnostic;

/// The widest a line gets before a bracketed list in it breaks across lines.
pub const WIDTH: usize = 100;

/// Spaces per level of indentation.
pub const INDENT: usize = 4;

/// Why a file couldn't be formatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file doesn't parse, so there's nothing safe to format.
    Syntax(Diagnostic),
    /// Formatting would have changed the file's meaning or comments. This is a bug in the
    /// formatter; the file is left alone.
    WouldChange(&'static str),
}

/// Formats a whole `.fen` file.
pub fn format(source: &str) -> Result<String, Error> {
    let before = fennec_syntax::parse(source).map_err(Error::Syntax)?;
    let lexed = fennec_syntax::lex_source(source).map_err(Error::Syntax)?;
    let comments = comments::Comments::attach(source, &lexed.tokens, &lexed.comments);
    let mut driver = blocks::Driver::new(source, &lexed.tokens, comments);
    driver.run();
    let formatted = output::render(&driver.writer.lines);
    check_same(source, &before, &formatted)?;
    Ok(formatted)
}

/// Checks formatting kept the syntax tree and every comment. A failure here is a bug in the
/// formatter, so the file is left alone.
fn check_same(source: &str, before: &fennec_syntax::Module, formatted: &str) -> Result<(), Error> {
    let after = fennec_syntax::parse(formatted).map_err(|_| Error::WouldChange("meaning"))?;
    if without_spans(&imports_sorted(before)) != without_spans(&imports_sorted(&after)) {
        return Err(Error::WouldChange("meaning"));
    }
    if comment_texts(source) != comment_texts(formatted) {
        return Err(Error::WouldChange("comments"));
    }
    Ok(())
}

/// A module with its imports first and sorted, names included, since their order means
/// nothing.
fn imports_sorted(module: &fennec_syntax::Module) -> fennec_syntax::Module {
    let (mut imports, rest): (Vec<_>, Vec<_>) =
        module.items.iter().cloned().partition(|item| matches!(item, fennec_syntax::Item::Import(_)));
    for item in &mut imports {
        if let fennec_syntax::Item::Import(import) = item {
            import.names.sort();
            import.span = fennec_syntax::Span::default();
        }
    }
    imports.sort_by_key(|item| format!("{item:?}"));
    imports.extend(rest);
    fennec_syntax::Module { items: imports }
}

/// A syntax tree written out with every position left out, so trees from differently laid
/// out sources compare equal.
pub fn without_spans(module: &fennec_syntax::Module) -> String {
    const MARK: &str = "Span { start: ";
    let debug = format!("{module:?}");
    let mut out = String::with_capacity(debug.len());
    let mut rest = debug.as_str();
    while let Some(at) = rest.find(MARK) {
        out.push_str(rest.get(..at).unwrap_or(""));
        out.push_str("Span");
        let after = rest.get(at..).unwrap_or("");
        rest = after.find('}').and_then(|end| after.get(end + 1..)).unwrap_or("");
    }
    out.push_str(rest);
    out
}

/// Every comment, as the formatter writes it, sorted: sorting imports moves their comments.
fn comment_texts(source: &str) -> Vec<String> {
    let mut texts: Vec<String> = fennec_syntax::lex_source(source)
        .map(|lexed| {
            lexed
                .comments
                .iter()
                .map(|span| layout::spaced(comments::text(source, *span).trim_end()))
                .collect()
        })
        .unwrap_or_default();
    texts.sort();
    texts
}

#[cfg(test)]
mod tests {
    use super::{Error, check_same, comment_texts, without_spans};

    fn parse(source: &str) -> fennec_syntax::Module {
        fennec_syntax::parse(source).unwrap()
    }

    #[test]
    fn trees_compare_without_their_positions() {
        let tree = without_spans(&parse("def f():\n    pass\n"));
        assert_eq!(
            tree,
            "Module { items: [Function(FnDef { name: \"f\", generics: [], params: [], ret: None, \
             body: [Stmt { kind: Pass, span: Span }], span: Span })] }"
        );
        assert_eq!(tree, without_spans(&parse("def  f( ):\n\n        pass\n")));
        assert_ne!(tree, without_spans(&parse("def g():\n    pass\n")));
    }

    #[test]
    fn comments_are_listed_sorted_with_their_space_after_the_hash() {
        assert_eq!(comment_texts("x = 1  #two\n# one  \n"), ["# one", "# two"]);
        assert_eq!(comment_texts("x = 1\n"), Vec::<String>::new());
    }

    #[test]
    fn a_result_that_changes_the_file_is_refused() {
        let source = "def f():\n    pass  # why\n";
        let before = parse(source);
        assert_eq!(check_same(source, &before, "def f():\n    pass  # why\n"), Ok(()));
        assert_eq!(
            check_same(source, &before, "def g():\n    pass  # why\n"),
            Err(Error::WouldChange("meaning"))
        );
        assert_eq!(check_same(source, &before, "def f(:\n"), Err(Error::WouldChange("meaning")));
        assert_eq!(check_same(source, &before, "def f():\n    pass\n"), Err(Error::WouldChange("comments")));
    }
}
