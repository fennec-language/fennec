//! A data pipeline, the twin of `data_pipeline.fen`: the CSV read by a small RFC 4180 reader into
//! rows of `String`s, parsed into `Order`s, totalled in `HashMap`s, then every order written in
//! `std.pack`'s compact form and read back, and written as JSON and read back, by hand in the shape
//! serde's derive gives, with the totals as JSON too.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hint::black_box;

const CUSTOMERS: i64 = 1_500;
const REGIONS: [&str; 5] = ["north", "south", "east", "west", "online"];
const PRODUCTS: [&str; 7] = ["tea", "coffee", "cocoa", "mugs", "kettles", "filters", "beans"];
/// Each note as a CSV field: in quotes, with its quotes doubled, when it holds a comma or a quote.
const FIELDS: [&str; 6] = ["", "gift", "\"rush, please\"", "\"leave at \"\"door\"\"\"", "call first", ""];

#[derive(Debug, PartialEq)]
struct Order {
    id: i64,
    region: String,
    product: String,
    quantity: i64,
    cents: i64,
    customer: String,
    note: String,
}

struct Total {
    key: String,
    orders: i64,
    units: i64,
    revenue: i64,
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// The orders as CSV text.
fn orders_csv(seed: i64, rows: i64) -> String {
    let mut state = seed;
    let mut lines = vec!["id,region,product,quantity,unit_cents,customer,note".to_owned()];
    for id in 0..rows {
        state = next(state);
        let roll = below(state, 1_000);
        state = next(state);
        let region = REGIONS[below(state, REGIONS.len() as i64) as usize];
        let product = PRODUCTS[(below(state, PRODUCTS.len() as i64 * 11) % PRODUCTS.len() as i64) as usize];
        state = next(state);
        let quantity = if roll < 7 { "n/a".to_owned() } else { (1 + below(state, 12)).to_string() };
        let cents = 150 + below(state, 4_000);
        state = next(state);
        let customer = below(state, CUSTOMERS);
        let note = FIELDS[(roll % FIELDS.len() as i64) as usize];
        lines.push(format!("{id},{region},{product},{quantity},{cents},cust-{customer},{note}"));
    }
    lines.join("\n") + "\n"
}

/// The rows of CSV text: fields split at commas, a quoted field's doubled quotes made one.
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// `std.pack`'s form for a count: 7 bits a byte, low first.
fn count(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// `std.pack`'s form for an int: zigzag, then as a count.
fn int(out: &mut Vec<u8>, value: i64) {
    count(out, ((value << 1) ^ (value >> 63)) as u64);
}

/// `std.pack`'s form for text: its length, then its bytes.
fn text(out: &mut Vec<u8>, value: &str) {
    count(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

/// Every order in `std.pack`'s form, without the header that holds the type's layout.
fn packed(orders: &[Order]) -> Vec<u8> {
    let mut out = Vec::new();
    count(&mut out, orders.len() as u64);
    for order in orders {
        int(&mut out, order.id);
        text(&mut out, &order.region);
        text(&mut out, &order.product);
        int(&mut out, order.quantity);
        int(&mut out, order.cents);
        text(&mut out, &order.customer);
        text(&mut out, &order.note);
    }
    out
}

/// Reads back what `packed` wrote.
struct Unpacker<'a> {
    data: &'a [u8],
    at: usize,
}

impl Unpacker<'_> {
    fn count(&mut self) -> u64 {
        let mut value = 0;
        let mut shift = 0;
        loop {
            let byte = self.data[self.at];
            self.at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return value;
            }
            shift += 7;
        }
    }

    fn int(&mut self) -> i64 {
        let value = self.count();
        (value >> 1) as i64 ^ -((value & 1) as i64)
    }

    fn text(&mut self) -> String {
        let length = self.count() as usize;
        let found = std::str::from_utf8(&self.data[self.at..self.at + length]).expect("pack holds text");
        self.at += length;
        found.to_owned()
    }

    fn orders(&mut self) -> Vec<Order> {
        let length = self.count();
        (0..length)
            .map(|_| Order {
                id: self.int(),
                region: self.text(),
                product: self.text(),
                quantity: self.int(),
                cents: self.int(),
                customer: self.text(),
                note: self.text(),
            })
            .collect()
    }
}

/// `value` as a JSON string.
fn quoted(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
}

/// Every order as JSON.
fn orders_json(orders: &[Order]) -> String {
    let mut out = String::from("[");
    for (at, order) in orders.iter().enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{{\"id\": {}, \"region\": ", order.id);
        quoted(&mut out, &order.region);
        out.push_str(", \"product\": ");
        quoted(&mut out, &order.product);
        let _ = write!(out, ", \"quantity\": {}, \"cents\": {}, \"customer\": ", order.quantity, order.cents);
        quoted(&mut out, &order.customer);
        out.push_str(", \"note\": ");
        quoted(&mut out, &order.note);
        out.push('}');
    }
    out.push(']');
    out
}

/// The totals as JSON.
fn totals_json(totals: &[&Total]) -> String {
    let mut out = String::from("[");
    for (at, total) in totals.iter().enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        out.push_str("{\"key\": ");
        quoted(&mut out, &total.key);
        let _ = write!(
            out,
            ", \"orders\": {}, \"units\": {}, \"revenue\": {}}}",
            total.orders, total.units, total.revenue
        );
    }
    out.push(']');
    out
}

/// Reads back a list of orders written as JSON, as serde's derive would: each field by its name.
struct JsonReader<'a> {
    text: &'a [u8],
    at: usize,
}

impl JsonReader<'_> {
    fn space(&mut self) {
        while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) {
        self.space();
        assert_eq!(self.text[self.at], byte, "the JSON is what was written");
        self.at += 1;
    }

    fn string(&mut self) -> String {
        self.expect(b'"');
        let mut out = String::new();
        loop {
            let byte = self.text[self.at];
            self.at += 1;
            match byte {
                b'"' => return out,
                b'\\' => {
                    let escaped = self.text[self.at];
                    self.at += 1;
                    out.push(match escaped {
                        b'n' => '\n',
                        b't' => '\t',
                        other => other as char,
                    });
                }
                _ => {
                    let start = self.at - 1;
                    while self.text[self.at] != b'"' && self.text[self.at] != b'\\' {
                        self.at += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.text[start..self.at]).expect("JSON is text"));
                }
            }
        }
    }

    fn number(&mut self) -> i64 {
        self.space();
        let start = self.at;
        while self.text[self.at] == b'-' || self.text[self.at].is_ascii_digit() {
            self.at += 1;
        }
        std::str::from_utf8(&self.text[start..self.at]).expect("digits").parse().expect("a number")
    }

    fn order(&mut self) -> Order {
        self.expect(b'{');
        let mut order =
            Order { id: 0, region: String::new(), product: String::new(), quantity: 0, cents: 0, customer: String::new(), note: String::new() };
        loop {
            let name = self.string();
            self.expect(b':');
            match name.as_str() {
                "id" => order.id = self.number(),
                "region" => order.region = self.string(),
                "product" => order.product = self.string(),
                "quantity" => order.quantity = self.number(),
                "cents" => order.cents = self.number(),
                "customer" => order.customer = self.string(),
                "note" => order.note = self.string(),
                other => panic!("an order has no field {other}"),
            }
            self.space();
            if self.text[self.at] == b'}' {
                self.at += 1;
                return order;
            }
            self.expect(b',');
        }
    }

    fn orders(&mut self) -> Vec<Order> {
        self.expect(b'[');
        let mut orders = Vec::new();
        self.space();
        if self.text[self.at] == b']' {
            return orders;
        }
        loop {
            orders.push(self.order());
            self.space();
            if self.text[self.at] == b']' {
                return orders;
            }
            self.expect(b',');
        }
    }
}

/// A hash of some bytes.
fn hashed(data: &[u8]) -> i64 {
    let mut hash = data.len() as i64;
    for &byte in data {
        hash = (hash * 31 + i64::from(byte)) % 1_000_000_007;
    }
    hash
}

fn main() {
    let started = std::time::Instant::now();
    let text = orders_csv(black_box(31), black_box(5_000));
    let rows = csv_rows(&text);
    let mut orders = Vec::new();
    let mut bad = 0;
    for row in &rows[1..] {
        let Ok(quantity) = row[3].parse::<i64>() else {
            bad += 1;
            continue;
        };
        orders.push(Order {
            id: row[0].parse().expect("an id"),
            region: row[1].clone(),
            product: row[2].clone(),
            quantity,
            cents: row[4].parse().expect("a price"),
            customer: row[5].clone(),
            note: row[6].clone(),
        });
    }
    let mut totals: HashMap<String, Total> = HashMap::new();
    let mut customers: HashMap<String, i64> = HashMap::new();
    for order in &orders {
        let key = format!("{}/{}", order.region, order.product);
        let revenue = order.quantity * order.cents;
        let total =
            totals.entry(key.clone()).or_insert_with(|| Total { key, orders: 0, units: 0, revenue: 0 });
        total.orders += 1;
        total.units += order.quantity;
        total.revenue += revenue;
        *customers.entry(order.customer.clone()).or_insert(0) += revenue;
    }
    let mut keys: Vec<&String> = totals.keys().collect();
    keys.sort();
    let report: Vec<&Total> = keys.iter().map(|key| &totals[*key]).collect();
    let mut best: Vec<(i64, &String)> = customers.iter().map(|(customer, revenue)| (-revenue, customer)).collect();
    best.sort();

    let packed = packed(&orders);
    let unpacked = Unpacker { data: &packed, at: 0 }.orders();
    let written = orders_json(&orders);
    let read = JsonReader { text: written.as_bytes(), at: 0 }.orders();
    let summary = totals_json(&report);

    println!("{} rows {bad} bad {} orders {} totals", rows.len() - 1, orders.len(), totals.len());
    println!("pack {} {}", packed.len(), hashed(&packed));
    println!("json {} {}", written.len(), hashed(written.as_bytes()));
    println!("summary {} {}", summary.len(), hashed(summary.as_bytes()));
    let sold: i64 = unpacked.iter().map(|order| order.quantity * order.cents).sum();
    let seen: i64 = read.iter().map(|order| order.note.len() as i64 + order.id).sum();
    println!("back {sold} {seen}");
    for (revenue, customer) in best.iter().take(3) {
        println!("best {customer} {}", -revenue);
    }
    println!("{}", &summary[..120]);
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
