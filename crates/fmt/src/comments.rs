//! Comments, attached to the tokens around them. A comment after code on the same line
//! trails the token before it; a comment on its own line leads the next token.

use fennec_syntax::{SourceToken, Span, TokenKind};

/// The comments of a file, attached to token indexes, plus where each source line starts.
pub(crate) struct Comments<'a> {
    /// Own-line comments before each token, by token index.
    pub(crate) leading: Vec<Vec<&'a str>>,
    /// The comment after each token on its line, by token index.
    pub(crate) trailing: Vec<Option<&'a str>>,
    /// Where each own-line comment starts, to place it by its column and blank lines.
    pub(crate) starts: Vec<Vec<u32>>,
    line_starts: Vec<u32>,
}

/// Whether a token is a layout token rather than code.
pub(crate) fn is_layout(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent)
}

impl<'a> Comments<'a> {
    pub(crate) fn attach(source: &'a str, tokens: &[SourceToken], spans: &[Span]) -> Self {
        let line_starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(at, _)| u32::try_from(at + 1).unwrap_or(u32::MAX)))
            .collect();
        let mut comments = Comments {
            leading: vec![Vec::new(); tokens.len()],
            trailing: vec![None; tokens.len()],
            starts: vec![Vec::new(); tokens.len()],
            line_starts,
        };
        let mut next = 0;
        let mut prev_code: Option<usize> = None;
        for span in spans {
            while tokens.get(next).is_some_and(|token| token.span.start < span.start) {
                if tokens.get(next).is_some_and(|token| !is_layout(token.kind)) {
                    prev_code = Some(next);
                }
                next += 1;
            }
            let text = text(source, *span).trim_end();
            let same_line = prev_code
                .and_then(|at| tokens.get(at))
                .is_some_and(|token| comments.line(token.span.end) == comments.line(span.start));
            if same_line && let Some(slot) = prev_code.and_then(|at| comments.trailing.get_mut(at)) {
                *slot = Some(text);
                continue;
            }
            let owner = (next..tokens.len())
                .find(|&at| tokens.get(at).is_some_and(|token| !is_layout(token.kind)))
                .unwrap_or(tokens.len().saturating_sub(1));
            if let Some(list) = comments.leading.get_mut(owner) {
                list.push(text);
            }
            if let Some(list) = comments.starts.get_mut(owner) {
                list.push(span.start);
            }
        }
        comments
    }

    /// The zero-based line an offset is on.
    pub(crate) fn line(&self, offset: u32) -> usize {
        self.line_starts.partition_point(|&start| start <= offset).saturating_sub(1)
    }

    /// The column an offset is at, in bytes.
    pub(crate) fn column(&self, offset: u32) -> u32 {
        let start = self.line_starts.get(self.line(offset)).copied().unwrap_or(0);
        offset.saturating_sub(start)
    }

    /// How many blank lines lie between two offsets.
    pub(crate) fn blank_lines(&self, from: u32, to: u32) -> usize {
        self.line(to).saturating_sub(self.line(from)).saturating_sub(1)
    }

    pub(crate) fn leading(&self, at: usize) -> &[&'a str] {
        self.leading.get(at).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn trailing(&self, at: usize) -> Option<&'a str> {
        self.trailing.get(at).copied().flatten()
    }
}

/// The source text in a span.
pub(crate) fn text(source: &str, span: Span) -> &str {
    let (start, end) = (usize::try_from(span.start).unwrap_or(0), usize::try_from(span.end).unwrap_or(0));
    source.get(start..end).unwrap_or("")
}
