//! Regex-redux, the twin of `regex.fen`, on the engine behind Fennec's `std.re`.

use fennec_re as re;

fn sequence(length: usize) -> String {
    let mut seed: i64 = 42;
    let mut letters = String::with_capacity(length);
    for _ in 0..length {
        seed = (seed * 3877 + 29573) % 139968;
        letters.push(['a', 'c', 'g', 't'][(seed * 4 / 139968) as usize]);
    }
    letters
}

/// `text` with each match replaced.
fn replaced(pattern: &str, text: &str, with: &str) -> String {
    let regex = re::Regex::new(pattern).unwrap();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for found in regex.find_all(text) {
        let (start, end) = found.whole();
        out.push_str(&text[last..start]);
        out.push_str(with);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

fn main() {
    let dna = sequence(std::hint::black_box(50_000));
    let patterns = [
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
    ];
    for pattern in patterns {
        println!("{pattern} {}", re::Regex::new(pattern).unwrap().find_all(&dna).len());
    }
    let mut text = dna.clone();
    for (pattern, with) in [("tHa[Nt]", "<4>"), ("aND|caN|Ha[DS]|WaS", "<3>"), ("a[NSt]|BY", "<2>"), ("<[^>]*>", "|")] {
        text = replaced(pattern, &text, with);
    }
    println!("{}", text.len());
    println!("{}", replaced("ta+c", &dna, "x").len());
}
