//! A `str` value: its text, and how many characters it holds and where they start, worked out the
//! first time either is asked for. So `text[at]` and `len(text)` on a long string take the same
//! time as on a short one, where counting through the characters each time made a program that
//! indexes a long string over and over take time growing with its length times the indexes.

use std::sync::{Arc, OnceLock};

/// How many characters apart the starts kept are: one in every 64, so what's kept is small beside
/// the text, and finding a character from the start before it reads at most 63 more.
const STRIDE: usize = 64;

/// A `str` value. It can't be changed in place, only made anew, so what it's worked out about its
/// characters is never out of date, and every copy of it shares one: copying one copies a pointer,
/// and a `Value` stays as small as a `String` kept it.
#[derive(Clone, Default)]
pub(crate) struct Text(Arc<Held>);

/// What a `Text` holds.
#[derive(Default)]
struct Held {
    text: String,
    chars: OnceLock<Chars>,
}

/// What a `Text` has worked out about its characters.
#[derive(Clone)]
struct Chars {
    count: usize,
    /// The byte each `STRIDE`th character starts at: the first's, the 65th's, and so on. Empty when
    /// every character is one byte, so the character at `at` starts at byte `at`.
    starts: Vec<usize>,
}

impl Text {
    pub(crate) fn as_str(&self) -> &str {
        &self.0.text
    }

    /// Its text, taken out when nothing else shares it.
    pub(crate) fn into_string(self) -> String {
        Arc::try_unwrap(self.0).map_or_else(|shared| shared.text.clone(), |held| held.text)
    }

    fn chars_known(&self) -> &Chars {
        let text = self.as_str();
        self.0.chars.get_or_init(|| {
            if text.is_ascii() {
                return Chars { count: text.len(), starts: Vec::new() };
            }
            let mut count = 0;
            let mut starts = Vec::new();
            for (byte, _) in text.char_indices() {
                if count % STRIDE == 0 {
                    starts.push(byte);
                }
                count += 1;
            }
            Chars { count, starts }
        })
    }

    /// How many characters it holds.
    pub(crate) fn char_count(&self) -> usize {
        self.chars_known().count
    }

    /// The byte the character at `at` starts at, counting from 0: the text's length for `at` just
    /// past the last, and `None` further on.
    pub(crate) fn byte_at(&self, at: usize) -> Option<usize> {
        let chars = self.chars_known();
        if at >= chars.count {
            return (at == chars.count).then_some(self.len());
        }
        if chars.starts.is_empty() {
            return Some(at);
        }
        let from = *chars.starts.get(at / STRIDE)?;
        let (byte, _) = self.as_str().get(from..)?.char_indices().nth(at % STRIDE)?;
        Some(from + byte)
    }

    /// The character at `at`, counting from 0.
    pub(crate) fn char_at(&self, at: usize) -> Option<char> {
        self.as_str().get(self.byte_at(at)?..)?.chars().next()
    }

    /// The characters from `from` up to, not including, `to`, counting from 0: as many of them as
    /// it holds.
    pub(crate) fn chars_between(&self, from: usize, to: usize) -> &str {
        let count = self.char_count();
        let start = self.byte_at(from.min(count)).unwrap_or(self.len());
        let end = self.byte_at(to.clamp(from.min(count), count)).unwrap_or(self.len());
        self.as_str().get(start..end).unwrap_or_default()
    }
}

impl std::ops::Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl From<String> for Text {
    fn from(text: String) -> Text {
        Text(Arc::new(Held { text, chars: OnceLock::new() }))
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Text {
        Text::from(text.to_owned())
    }
}

impl From<char> for Text {
    fn from(c: char) -> Text {
        Text::from(String::from(c))
    }
}

impl From<Text> for String {
    fn from(text: Text) -> String {
        text.into_string()
    }
}

impl FromIterator<char> for Text {
    fn from_iter<I: IntoIterator<Item = char>>(chars: I) -> Text {
        Text::from(chars.into_iter().collect::<String>())
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Debug for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self.as_str(), f)
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Text) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Text {}

impl PartialEq<str> for Text {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Text {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialOrd for Text {
    fn partial_cmp(&self, other: &Text) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Text {
    fn cmp(&self, other: &Text) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl std::hash::Hash for Text {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl std::borrow::Borrow<str> for Text {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    /// Each character found by where it is, in text of one-byte characters and of longer ones, on
    /// both sides of where a kept start is, and slices across them.
    #[test]
    fn a_character_is_found_by_where_it_is() {
        let mixed: String = (0..300).map(|at| ['a', 'é', '日', '🦊'][at % 4]).collect();
        for text in [mixed, "plain ascii text".repeat(20), String::new()] {
            let held = Text::from(text.clone());
            let chars: Vec<char> = text.chars().collect();
            assert_eq!(held.char_count(), chars.len());
            for (at, c) in chars.iter().enumerate() {
                assert_eq!(held.char_at(at), Some(*c), "at {at}");
            }
            assert_eq!(held.char_at(chars.len()), None);
            assert_eq!(held.byte_at(chars.len()), Some(text.len()));
            assert_eq!(held.byte_at(chars.len() + 1), None);
            for (from, to) in [(0_usize, 0_usize), (0, 5), (60, 70), (63, 129), (5, 2), (10, 10_000)] {
                let wanted: String = chars.iter().skip(from).take(to.saturating_sub(from)).collect();
                assert_eq!(held.chars_between(from, to), wanted, "{from}..{to}");
            }
        }
    }
}
