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
    assert_eq!(
        error("(a{1000}){1000}"),
        "it compiles to 1,002,003 steps, and the most a pattern may is 10,000, since each character of a text costs that many"
    );
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

/// Groups nest at most 250 deep. Deeper, a pattern fails as too deep rather than recursing until
/// the stack runs out, which ended the process with a signal: a server taking a pattern from a
/// request could be stopped by one.
#[test]
fn a_pattern_nested_ten_thousand_deep_fails_as_too_deep() {
    let deep = |open: &str, depth: usize| format!("{}a{}", open.repeat(depth), ")".repeat(depth));
    assert_eq!(error(&deep("(", 10_000)), "groups nest more than 250 deep");
    assert_eq!(error(&deep("(?:", 10_000)), "groups nest more than 250 deep");
    // Never closed, it's still too deep before it's anything else.
    assert_eq!(error(&"(".repeat(100_000)), "groups nest more than 250 deep");
    assert_eq!(error(&deep("(", 251)), "groups nest more than 250 deep");
    assert_eq!(error(&deep("(", 250)), "<valid>");
    assert_eq!(search(&deep("(", 250), "xay"), Some("a".to_owned()));
}

/// What a pattern counts as its size is what it compiles to, so the limit refuses exactly the
/// patterns it should, and says how big each is.
#[test]
fn a_patterns_size_is_counted_before_it_compiles() {
    let commas = |n: usize| {
        let digits = n.to_string();
        let mut out = String::new();
        for (at, digit) in digits.chars().enumerate() {
            if at > 0 && (digits.len() - at).is_multiple_of(3) {
                out.push(',');
            }
            out.push(digit);
        }
        out
    };
    for pattern in
        ["a", "abc", "a|b|c", "(a)", "(?:a)", "a*", "a+?", "a{3}", "a{2,5}", "(a|bc){0,4}d", "\\bx\\b", ""]
    {
        let size = Regex::new(pattern).unwrap().size();
        // Twenty of it, or a thousand, and the three instructions every program has.
        for times in [20, 1000] {
            let wanted = (size - 3) * times + 3;
            match Regex::new(&format!("(?:{pattern}){{{times}}}")) {
                Ok(regex) => assert_eq!(regex.size(), wanted, "{pattern} {times} times"),
                Err(error) => {
                    assert!(wanted > fennec_re::MAX_SIZE, "{pattern} {times} times: {}", error.message);
                    assert!(
                        error.message.starts_with(&format!("it compiles to {} steps", commas(wanted))),
                        "{}",
                        error.message
                    );
                }
            }
        }
    }
}

/// The most a pattern may compile to is pinned, so a later change can't quietly let a pattern
/// from outside cost more per character: just under it builds, and just over it says both sizes.
#[test]
fn the_most_a_pattern_may_cost_is_pinned() {
    assert_eq!(fennec_re::MAX_SIZE, 10_000);
    // `a?` is 2 steps: 4,998 of them and the 3 every program has make 9,999.
    let under = Regex::new("(?:a?){999}(?:a?){999}(?:a?){999}(?:a?){999}(?:a?){999}(?:a?){3}").unwrap();
    assert_eq!(under.size(), 9_999);
    let over = Regex::new("(?:a?){999}(?:a?){999}(?:a?){999}(?:a?){999}(?:a?){999}(?:a?){4}b").unwrap_err();
    assert_eq!(
        over.message,
        "it compiles to 10,002 steps, and the most a pattern may is 10,000, since each character of a text costs that many"
    );
    // Too big to build at all is counted without building it.
    let huge = Regex::new("(?:(?:(?:a?){1000}){1000}){1000}").unwrap_err();
    assert!(huge.message.starts_with("it compiles to 2,000,000,003 steps"), "{}", huge.message);
}

/// The costliest pattern the limit lets through, with a live thread at nearly every one of its
/// steps, runs through text in time that grows with the text, and finishes: 10 KB here, in a
/// debug build; the docs give release times for a megabyte.
#[test]
fn the_costliest_pattern_allowed_runs_through_text() {
    let worst = Regex::new("(?:(?:a?){1000}){4}b").unwrap();
    assert!(worst.size() > 8_000 && worst.size() <= fennec_re::MAX_SIZE, "{}", worst.size());
    let text = "a".repeat(10_000);
    assert_eq!(worst.find_at(&text, 0, Anchor::Search), None);
    assert!(worst.find_at(&format!("{text}b"), 0, Anchor::Search).is_some());
}
