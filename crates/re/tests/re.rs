//! The engine against what Python's `re` gives for the same patterns.

use fennec_re::{Anchor, Regex};

/// The text between two byte offsets; a marker if they're not in it, failing the test.
fn slice(text: &str, (from, to): (usize, usize)) -> String {
    text.get(from..to).unwrap_or("<not in the text>").to_owned()
}

/// Every match's text, as Python's `findall` gives it for a pattern without groups.
fn find_all(pattern: &str, text: &str) -> Vec<String> {
    let Ok(regex) = Regex::new(pattern) else { return vec!["<invalid pattern>".to_owned()] };
    regex.find_all(text).iter().map(|captures| slice(text, captures.whole())).collect()
}

fn search(pattern: &str, text: &str) -> Option<String> {
    let captures = Regex::new(pattern).ok()?.find_at(text, 0, Anchor::Search)?;
    Some(slice(text, captures.whole()))
}

fn groups(pattern: &str, text: &str) -> Vec<Option<String>> {
    let found = Regex::new(pattern).ok().and_then(|regex| regex.find_at(text, 0, Anchor::Search));
    let Some(captures) = found else { return Vec::new() };
    captures.groups.iter().map(|group| group.map(|bounds| slice(text, bounds))).collect()
}

fn error(pattern: &str) -> String {
    Regex::new(pattern).err().map_or_else(|| "<valid>".to_owned(), |error| error.message)
}

#[test]
fn literals_dots_and_classes() {
    assert_eq!(search("cat", "concatenate"), Some("cat".into()));
    assert_eq!(search("c.t", "a cut"), Some("cut".into()));
    assert_eq!(search("a.b", "a\nb"), None);
    assert_eq!(find_all("[aeiou]", "fennec"), ["e", "e"]);
    assert_eq!(find_all("[^a-z ]+", "abc DEF ghi 42"), ["DEF", "42"]);
    assert_eq!(find_all(r"\d+", "hp 30, mp 7"), ["30", "7"]);
    assert_eq!(find_all(r"[\d.]+", "v1.25 and 3"), ["1.25", "3"]);
    assert_eq!(find_all(r"\w+", "naïve café_2!"), ["naïve", "café_2"]);
    assert_eq!(find_all(r"\S+", " a  bc "), ["a", "bc"]);
    assert_eq!(find_all("[]a]", "a]b"), ["a", "]"]);
    assert_eq!(find_all("[a-]", "-ab"), ["-", "a"]);
    assert_eq!(search(r"\.", "a.b"), Some(".".into()));
}

#[test]
fn anchors_and_word_boundaries() {
    assert_eq!(search("^ab", "cab"), None);
    assert_eq!(search("ab$", "abc"), None);
    assert_eq!(search("^abc$", "abc"), Some("abc".into()));
    assert_eq!(find_all(r"\bcat\b", "cat concat cat."), ["cat", "cat"]);
    assert_eq!(find_all(r"\Bcat", "cat concat"), ["cat"]);
    assert_eq!(search(r"\bcat", " cat"), Some("cat".into()));
    assert_eq!(search(r"cat\b", "cat."), Some("cat".into()));
}

#[test]
fn repeats_are_greedy_unless_lazy() {
    assert_eq!(search("a*", "aaab"), Some("aaa".into()));
    assert_eq!(search("a+?", "aaab"), Some("a".into()));
    assert_eq!(search("<.*>", "<a><b>"), Some("<a><b>".into()));
    assert_eq!(search("<.*?>", "<a><b>"), Some("<a>".into()));
    assert_eq!(search("colou?r", "color"), Some("color".into()));
    assert_eq!(search("a{2,3}", "aaaa"), Some("aaa".into()));
    assert_eq!(search("a{2,3}?", "aaaa"), Some("aa".into()));
    assert_eq!(search("a{2}", "a aa"), Some("aa".into()));
    assert_eq!(search("a{2,}", "aaaaa"), Some("aaaaa".into()));
    assert_eq!(search("a{,2}", "aaa"), Some("aa".into()));
    assert_eq!(search("x{a}", "x{a}"), Some("x{a}".into()));
}

#[test]
fn groups_and_alternation() {
    assert_eq!(
        groups(r"(\w+)@(\w+)\.com", "mail ash@den.com"),
        [Some("ash@den.com".into()), Some("ash".into()), Some("den".into())]
    );
    assert_eq!(groups("(a)|(b)", "b"), [Some("b".into()), None, Some("b".into())]);
    assert_eq!(groups("(a|ab)(c|bcd)", "abcd"), [Some("abcd".into()), Some("a".into()), Some("bcd".into())]);
    assert_eq!(groups("(?:ab)+(c)", "ababc"), [Some("ababc".into()), Some("c".into())]);
    assert_eq!(groups("(a*)+", "b"), [Some(String::new()), Some(String::new())]);
    assert_eq!(find_all("cat|dog", "dog cat bird"), ["dog", "cat"]);
    assert_eq!(search("|a", "a"), Some(String::new()));
}

#[test]
fn empty_matches_move_on_one_character() {
    assert_eq!(find_all("a*", "baaa"), ["", "aaa", ""]);
    assert_eq!(find_all("", "ab"), ["", "", ""]);
    assert_eq!(find_all("x*", "é"), ["", ""]);
}

#[test]
fn anchored_matches() {
    let regex = Regex::new(r"\d+").unwrap();
    assert!(regex.find_at("a1", 0, Anchor::Start).is_none());
    assert_eq!(regex.find_at("12a", 0, Anchor::Start).unwrap().whole(), (0, 2));
    assert!(regex.find_at("12a", 0, Anchor::Both).is_none());
    assert_eq!(regex.find_at("123", 0, Anchor::Both).unwrap().whole(), (0, 3));
    assert_eq!(Regex::new("a|ab").unwrap().find_at("ab", 0, Anchor::Both).unwrap().whole(), (0, 2));
}

#[test]
fn bad_patterns_say_what_is_wrong() {
    assert_eq!(error("(ab"), "this `(` is never closed");
    assert_eq!(error("ab)"), "this `)` has no `(` before it");
    assert_eq!(error("[ab"), "this `[` is never closed");
    assert_eq!(error("*a"), "there's nothing here to repeat");
    assert_eq!(error("a**"), "this repeats a repeat; put the inner one in a group");
    assert_eq!(error("[z-a]"), "the range `z-a` is backwards");
    assert_eq!(error(r"a\"), "a `\\` at the end of a pattern escapes nothing");
    assert_eq!(error(r"\q"), "`\\q` isn't an escape patterns know");
    assert_eq!(error("a{3,2}"), "in `{m,n}`, m can't be bigger than n");
    assert_eq!(error("a{1001}"), "repeats are limited to 1000");
    assert_eq!(error("(?=a)"), "the only group extension so far is `(?:...)`");
    assert_eq!(error("(a{1000}){1000}"), "this pattern is too big");
    assert_eq!(Regex::new("ab(c").unwrap_err().at, 2);
}

#[test]
fn patterns_that_make_backtrackers_blow_up_take_linear_time() {
    // `(a*)*b` and `(a|a)*b` take exponential time in a backtracking engine.
    let text = "a".repeat(20_000);
    let started = std::time::Instant::now();
    assert_eq!(search("(a*)*b", &text), None);
    assert_eq!(search("(a|a)*b", &text), None);
    assert_eq!(search("(x+x+)+y", &"x".repeat(5_000)), None);
    assert!(started.elapsed().as_secs() < 20, "took {:?}", started.elapsed());
}
