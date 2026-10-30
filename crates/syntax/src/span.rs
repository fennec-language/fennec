//! Byte ranges into a source file.

/// A range of bytes in a source file, used to point diagnostics at code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start: to_u32(start), end: to_u32(end) }
    }

    /// The smallest span covering both `self` and `other`.
    #[must_use]
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    /// This span moved `by` bytes later, for code lexed out of an f-string.
    #[must_use]
    pub fn shifted(self, by: u32) -> Span {
        Span { start: self.start.saturating_add(by), end: self.end.saturating_add(by) }
    }
}

fn to_u32(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}
