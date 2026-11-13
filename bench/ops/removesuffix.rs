//! The twin of `removesuffix.fen`, done the idiomatic way: `text.removesuffix("ms")` of a time like `111ms`.

use std::hint::black_box;

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let texts: Vec<String> = (shift..shift + 16).map(|i| format!("{}ms", i * 3)).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let text = texts[((at + shift) % 16) as usize].as_str();
        let trimmed = text.strip_suffix("ms").unwrap_or(text);
        total += trimmed.len() as i64;
    }
    println!("{total}");
}
