//! K-nucleotide, the twin of `k_nucleotide.fen`.

use std::collections::HashMap;

fn sequence(length: usize) -> String {
    let mut seed: i64 = 42;
    let mut letters = String::with_capacity(length);
    for _ in 0..length {
        seed = (seed * 3877 + 29573) % 139968;
        letters.push(['a', 'c', 'g', 't'][(seed * 4 / 139968) as usize]);
    }
    letters
}

fn counted(dna: &str, size: usize) -> HashMap<&str, i64> {
    let mut counts = HashMap::new();
    for at in 0..=dna.len() - size {
        *counts.entry(&dna[at..at + size]).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let dna = sequence(std::hint::black_box(100_000));
    for size in [1, 2] {
        let mut ordered: Vec<(&str, i64)> = counted(&dna, size).into_iter().collect();
        ordered.sort_by_key(|pair| (-pair.1, pair.0));
        for (piece, count) in ordered {
            println!("{piece} {count}");
        }
    }
    for piece in ["ggt", "ggta", "ggtatt", "ggtattttaatt", "ggtattttaatttatagt"] {
        println!("{} {piece}", counted(&dna, piece.len()).get(piece).unwrap_or(&0));
    }
}
