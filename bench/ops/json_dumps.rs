//! The twin of `json_dumps.fen`, done the idiomatic way: `json.dumps(order)` of a struct with two ints and a str.

use std::fmt::Write;
use std::hint::black_box;

struct Order {
    user: i64,
    item: String,
    quantity: i64,
}

/// Writes `text` as a JSON string.
fn quoted(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Order {
    /// What `json.dumps(order)` writes.
    fn dump(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{{\"user\": {}, \"item\": ", self.user);
        quoted(&mut out, &self.item);
        let _ = write!(out, ", \"quantity\": {}}}", self.quantity);
        out
    }
}

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let orders: Vec<Order> = (shift..shift + 16).map(|i| Order { user: i, item: format!("arrow-{i}"), quantity: i + 1 }).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let text = orders[((at + shift) % 16) as usize].dump();
        total += text.len() as i64;
    }
    println!("{total}");
}
