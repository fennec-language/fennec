//! Byte ranges into a source file.

/// A range of bytes in a source file, used to point diagnostics at code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// Where the standard library's spans start, well past any program's own source.
    pub const PRELUDE: u32 = 1 << 31;

    /// Where the spans of what the compiler writes in start, below the standard library's. A
    /// program's own text ends before it, or what it wrote would read as the compiler's.
    pub const WRITTEN: u32 = Span::PRELUDE - (1 << 28);

    /// Whether this points into the standard library rather than the program.
    pub fn in_prelude(self) -> bool {
        self.start >= Span::PRELUDE
    }

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

/// Spans a copy of some text was moved to, so each copy written in is apart from every other in
/// what's found by span, and where the text it copies starts. A span in it points, for whatever
/// says where something is, at the text it's a copy of (`Span::copied_from`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Copied {
    /// The first span of the copy.
    pub at: u32,
    /// How many spans the copy takes.
    pub len: u32,
    /// The span of the text the copy's first span copies.
    pub from: u32,
}

impl Span {
    /// Where this span's text is: in the text a copy `copied` lists was made from, when it's in
    /// one, and otherwise this span. `copied` is in order of `at`.
    #[must_use]
    pub fn copied_from(self, copied: &[Copied]) -> Span {
        let after = copied.partition_point(|copy| copy.at <= self.start);
        let Some(copy) = after.checked_sub(1).and_then(|at| copied.get(at)) else { return self };
        if self.start - copy.at >= copy.len {
            return self;
        }
        let moved = |at: u32| at - copy.at + copy.from;
        Span { start: moved(self.start), end: moved(self.end.max(self.start)) }
    }
}

fn to_u32(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}
