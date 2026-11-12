//! The facts the backend hands LLVM beside the code: what a value's range is (`!range`), what
//! memory a load or store can touch (`!tbaa`), what can't change (`!invariant.load`), that an
//! address stays inside its block (`inbounds`), that a sum doesn't wrap (`nsw`, `nuw`), that a
//! parameter's address is its alone (`noalias`) and kept nowhere (`captures(none)`), and what's
//! assumed (`llvm.assume`). LLVM trusts each, so a wrong one miscompiles silently.
//!
//! `FENNEC_STRIP_FACTS=1` builds without any of them, which runs the same, only slower: a program
//! printing otherwise stripped has been handed a wrong fact. That's the one way to catch a wrong
//! TBAA tag, which nothing can check where it runs. A build checking proofs checks the facts it
//! can instead of handing them over (see `Emit::proof_held_here`).
//!
//! Alignment stays: a load says how its address is aligned, and none says more than the type's
//! own. So does `nounwind`, as nothing Fennec calls unwinds.

/// Module text as a build with `shared`'s settings hands it to LLVM: stripped of facts, or not.
pub(crate) fn as_handed(text: String, shared: &crate::Shared) -> String {
    match shared.facts {
        crate::Facts::Given => text,
        crate::Facts::Stripped => stripped(&text),
    }
}

/// The words that state a fact about the instruction or parameter they're on.
const WORDS: [&str; 7] = ["inbounds", "nsw", "nuw", "exact", "noalias", "nonnull", "captures(none)"];

/// The metadata that states a fact about the instruction it's on.
const ATTACHED: [&str; 4] = ["!tbaa", "!range", "!invariant.load", "!nonnull"];

/// Module text with every fact taken out, and nothing else changed: text in quotes, as in a
/// string's bytes or a symbol's name, is left alone.
pub(crate) fn stripped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("call void @llvm.assume(") {
            continue;
        }
        // Outside quotes are the even pieces: LLVM writes a quote in a string as `\22`.
        for (at, piece) in line.split('"').enumerate() {
            if at > 0 {
                out.push('"');
            }
            if at % 2 == 1 {
                out.push_str(piece);
            } else {
                out.push_str(&without_facts(piece));
            }
        }
    }
    out
}

/// A piece of a line outside quotes, without its facts.
fn without_facts(piece: &str) -> String {
    let mut kept = piece.to_owned();
    for name in ATTACHED {
        // `, !tbaa !4`: the name, then the node it names.
        while let Some(start) = find_word(&kept, &format!(", {name} !")) {
            let node = start + name.len() + 4;
            let end = kept
                .get(node..)
                .and_then(|rest| rest.find(|c: char| !c.is_ascii_digit()))
                .map_or(kept.len(), |len| node + len);
            kept.replace_range(start..end, "");
        }
    }
    for word in WORDS {
        while let Some(start) = find_word(&kept, &format!("{word} ")) {
            let end = start + word.len() + " ".len();
            kept.replace_range(start..end, "");
        }
    }
    kept
}

/// Where `word` starts in `text` as a word of its own: at the start, or after a space or a `(`.
fn find_word(text: &str, word: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text.get(from..).and_then(|rest| rest.find(word)) {
        let at = from + found;
        let before = text.get(..at).and_then(|head| head.chars().next_back());
        if word.starts_with(',') || before.is_none_or(|c| c == ' ' || c == '(') {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fact comes out, and the code stays as it was: the hints (`!prof`, `!llvm.loop`),
    /// alignment, and words in a string or a symbol's name.
    #[test]
    fn stripping_takes_out_every_fact_and_nothing_else() {
        let text = "\
define swiftcc i64 @\"f_go nsw inbounds\"(ptr noalias captures(none) %p0, ptr swifterror %err) #0 {
entry:
  %a = getelementptr inbounds [16 x i8], ptr %p0, i64 2
  %b = load i64, ptr %a, align 8, !tbaa !4, !range !7
  %c = add nuw nsw i64 %b, 1
  %d = load i64, ptr @fennec_hash_seed, !invariant.load !1
  %e = icmp sge i64 %c, 0
  call void @llvm.assume(i1 %e)
  br i1 %e, label %x, label %y, !prof !8
x:
  store i64 %c, ptr %a, !tbaa !12
  br label %x, !llvm.loop !13
y:
  ret i64 %d
}
@s = private constant [12 x i8] c\"nsw inbounds\"
";
        let got = stripped(text);
        let want = "\
define swiftcc i64 @\"f_go nsw inbounds\"(ptr %p0, ptr swifterror %err) #0 {
entry:
  %a = getelementptr [16 x i8], ptr %p0, i64 2
  %b = load i64, ptr %a, align 8
  %c = add i64 %b, 1
  %d = load i64, ptr @fennec_hash_seed
  %e = icmp sge i64 %c, 0
  br i1 %e, label %x, label %y, !prof !8
x:
  store i64 %c, ptr %a
  br label %x, !llvm.loop !13
y:
  ret i64 %d
}
@s = private constant [12 x i8] c\"nsw inbounds\"
";
        assert_eq!(got, want);
    }

    /// A name that only ends like a fact's word, or has one inside it, stays whole.
    #[test]
    fn a_name_like_a_fact_stays() {
        assert_eq!(stripped("  %unsw = add i64 %xnsw, 1\n"), "  %unsw = add i64 %xnsw, 1\n");
        assert_eq!(
            stripped("  %r = call i64 @inbounds_of(i64 1)\n"),
            "  %r = call i64 @inbounds_of(i64 1)\n"
        );
    }
}
