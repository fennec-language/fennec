//! The twin of `json_loads.fen`, done the idiomatic way: `json.loads[Order](text)` of a 50-byte object with two ints and a str.

use std::hint::black_box;

/// Reads JSON text a value at a time, as a deserializer does.
struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn space(&mut self) {
        while self.at < self.text.len() && matches!(self.text[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }

    fn take(&mut self, byte: u8) -> Option<()> {
        self.space();
        if self.text.get(self.at) == Some(&byte) {
            self.at += 1;
            Some(())
        } else {
            None
        }
    }

    fn int(&mut self) -> Option<i64> {
        self.space();
        let negative = self.text.get(self.at) == Some(&b'-');
        if negative {
            self.at += 1;
        }
        let start = self.at;
        let mut value: i64 = 0;
        while let Some(&digit) = self.text.get(self.at).filter(|digit| digit.is_ascii_digit()) {
            value = value.checked_mul(10)?.checked_add(i64::from(digit - b'0'))?;
            self.at += 1;
        }
        if self.at == start {
            return None;
        }
        Some(if negative { -value } else { value })
    }

    fn text(&mut self) -> Option<String> {
        self.take(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(*self.text.get(self.at)?, b'"' | b'\\') {
                self.at += 1;
            }
            out.push_str(std::str::from_utf8(&self.text[start..self.at]).ok()?);
            if self.text[self.at] == b'"' {
                self.at += 1;
                return Some(out);
            }
            self.at += 1;
            let escaped = *self.text.get(self.at)?;
            self.at += 1;
            out.push(match escaped {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                _ => return None,
            });
        }
    }

    /// The next field's name in an object, or `None` at its end. Starts the object when
    /// `first`.
    fn field(&mut self, first: bool) -> Option<Option<String>> {
        if first {
            self.take(b'{')?;
            if self.take(b'}').is_some() {
                return Some(None);
            }
        } else if self.take(b'}').is_some() {
            return Some(None);
        } else {
            self.take(b',')?;
        }
        let name = self.text()?;
        self.take(b':')?;
        Some(Some(name))
    }
}

/// Sets `slot` from the reader, refusing a field given twice.
fn once<T>(slot: &mut Option<T>, value: Option<T>) -> Option<()> {
    if slot.is_some() {
        return None;
    }
    *slot = Some(value?);
    Some(())
}

struct Order {
    user: i64,
    item: String,
    quantity: i64,
}

impl Order {
    /// What `json.loads[Order]` does: each field by name, unknown and repeated ones refused.
    fn load(text: &str) -> Option<Self> {
        let mut reader = Reader { text: text.as_bytes(), at: 0 };
        let (mut user, mut item, mut quantity) = (None, None, None);
        let mut first = true;
        while let Some(field) = reader.field(first)? {
            first = false;
            match field.as_str() {
                "user" => once(&mut user, reader.int())?,
                "item" => once(&mut item, reader.text())?,
                "quantity" => once(&mut quantity, reader.int())?,
                _ => return None,
            }
        }
        reader.space();
        (reader.at == reader.text.len()).then_some(())?;
        Some(Order { user: user?, item: item?, quantity: quantity? })
    }
}

fn main() {
    let ops = black_box(20_000_i64);
    let shift = black_box(0_i64);
    let texts: Vec<String> =
        (shift..shift + 16).map(|i| format!("{{\"user\": {i}, \"item\": \"arrow-{i}\", \"quantity\": {}}}", i + 1)).collect();
    let mut total: i64 = 0;
    for at in 0..ops {
        let order = Order::load(&texts[((at + shift) % 16) as usize]).expect("each text is an order");
        total += order.user + order.quantity + order.item.len() as i64;
    }
    println!("{total}");
}
