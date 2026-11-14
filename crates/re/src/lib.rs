//! Regular expressions for Fennec's `std.re`, hand-written with no dependencies.
//!
//! A pattern is parsed, compiled to a small program, and run by a Pike VM: every way the
//! pattern could match is followed at once, a step per character, so matching takes time
//! in proportion to the text's length times the pattern's size, whatever the pattern.
//! There's no backtracking, so no pattern can make matching blow up.
//!
//! Matches follow Python's rules: the leftmost match wins, and among matches starting
//! there, the one the pattern prefers (greedy repeats take as much as they can).

mod compile;
mod parse;
mod vm;

use compile::Program;

/// Why a pattern isn't valid, and where in it the problem is, counting characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub message: String,
    pub at: usize,
}

/// Where a match starts and ends in the text, as byte offsets, and where each group did.
/// Group 0 is the whole match; a group that took no part in the match is `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captures {
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Captures {
    /// The whole match.
    pub fn whole(&self) -> (usize, usize) {
        self.groups.first().copied().flatten().unwrap_or((0, 0))
    }
}

/// How much of the text a match must cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The match can start anywhere.
    Search,
    /// The match must start where the search does.
    Start,
    /// The match must start where the search does and run to the end of the text.
    Both,
}

/// A compiled pattern.
#[derive(Clone, Debug)]
pub struct Regex {
    program: Program,
}

/// The most instructions a pattern may compile to. Matching follows each instruction a
/// thread can be at, for each character, so a pattern's size is what each character of the text
/// costs, and this is what one from outside can make it cost: `(?:(?:a?){1000}){4}b`, 22
/// characters, is about 8,000. It also keeps `x{1000}{1000}` from filling memory.
pub const MAX_SIZE: usize = 10_000;

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, Error> {
        let (node, groups) = parse::parse(pattern)?;
        // Counted first, so a pattern too big is never built, and says how big it is.
        let size = compile::size(&node).saturating_add(3);
        if size > u64::try_from(MAX_SIZE).unwrap_or(u64::MAX) {
            let message = format!(
                "it compiles to {} steps, and the most a pattern may is {}, since each character of a text costs that many",
                commas(size),
                commas(u64::try_from(MAX_SIZE).unwrap_or(u64::MAX)),
            );
            return Err(Error { message, at: 0 });
        }
        let program = compile::compile(&node, groups, MAX_SIZE)
            .ok_or_else(|| Error { message: "this pattern is too big".to_owned(), at: 0 })?;
        Ok(Regex { program })
    }

    /// How many instructions the pattern compiled to: what each character of a text costs.
    pub fn size(&self) -> usize {
        self.program.insts.len()
    }

    /// How many groups the pattern has, not counting the whole match.
    pub fn groups(&self) -> usize {
        self.program.groups
    }

    /// The first match in `text` at or after byte offset `start`.
    pub fn find_at(&self, text: &str, start: usize, anchor: Anchor) -> Option<Captures> {
        vm::run(&self.program, text, start, anchor)
    }

    /// Every match in `text`, left to right, without overlaps. After an empty match the
    /// next search starts a character later, as Python's `findall` does.
    pub fn find_all(&self, text: &str) -> Vec<Captures> {
        let mut found = Vec::new();
        let mut start = 0;
        while start <= text.len() {
            let Some(captures) = self.find_at(text, start, Anchor::Search) else { break };
            let (from, to) = captures.whole();
            start = if to == from { next_boundary(text, to) } else { to };
            found.push(captures);
        }
        found
    }
}

/// `n` with a comma between each group of three digits.
fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The byte offset of the character after the one at `at`, or past the end.
fn next_boundary(text: &str, at: usize) -> usize {
    text.get(at..).and_then(|rest| rest.chars().next()).map_or(at + 1, |c| at + c.len_utf8())
}
