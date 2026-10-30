//! Building a long string piece by piece, then scanning it: the twin of `strings.fen`.

fn main() {
    let mut text = String::new();
    for i in 0..std::hint::black_box(2_000_000) {
        text += if i % 3 == 0 { "fennec " } else { "fox " };
    }
    let spaces = text.chars().filter(|&c| c == ' ').count();
    println!("{} {}", text.len(), spaces);
}
