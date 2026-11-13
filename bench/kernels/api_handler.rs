//! A JSON API's request handler, the twin of `api_handler.fen`: requests parsed from their text
//! with borrowed slices, bodies read into structs by a reader of the shape serde's derive writes
//! (each field by name, unknown and repeated fields refused, then the struct's rules checked),
//! users in a `HashMap` of structs written in place, orders in a `HashMap` of `Vec`s, and JSON
//! responses written into `String`s.

use std::collections::HashMap;
use std::fmt::Write;
use std::hint::black_box;

const CORPUS: usize = 4_096;
const DAY: i64 = 2_500;
const SEEN: i64 = 4_000;
const SHOWN: usize = 20;
const ITEMS: [&str; 8] = ["arrow", "potion", "bone", "herb", "gem", "scroll", "key", "lantern"];

struct NewUser {
    name: String,
    age: i64,
    email: String,
}

struct NewOrder {
    user: i64,
    item: String,
    quantity: i64,
    price: i64,
}

struct AgeChange {
    age: i64,
}

struct User {
    id: i64,
    name: String,
    age: i64,
    email: String,
    spent: i64,
    orders: i64,
}

struct Order {
    id: i64,
    item: String,
    quantity: i64,
    price: i64,
}

struct Response {
    status: i64,
    body: String,
}

struct Request<'a> {
    method: &'a str,
    path: &'a str,
    headers: HashMap<&'a str, &'a str>,
    body: &'a str,
}

#[derive(Default)]
struct Service {
    users: HashMap<i64, User>,
    orders: HashMap<i64, Vec<Order>>,
    cache: HashMap<i64, String>,
    hits: HashMap<String, i64>,
    stats: String,
    next_user: i64,
    next_order: i64,
}

/// Reads JSON text a value at a time, as a deserializer does.
struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(text: &'a str) -> Self {
        Reader { text: text.as_bytes(), at: 0 }
    }

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
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let hex = std::str::from_utf8(self.text.get(self.at..self.at + 4)?).ok()?;
                    self.at += 4;
                    char::from_u32(u32::from_str_radix(hex, 16).ok()?)?
                }
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

    fn end(&mut self) -> Option<()> {
        self.space();
        (self.at == self.text.len()).then_some(())
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

impl NewUser {
    fn load(text: &str) -> Option<Self> {
        let mut reader = Reader::new(text);
        let (mut name, mut age, mut email) = (None, None, None);
        let mut first = true;
        while let Some(field) = reader.field(first)? {
            first = false;
            match field.as_str() {
                "name" => once(&mut name, reader.text())?,
                "age" => once(&mut age, reader.int())?,
                "email" => once(&mut email, reader.text())?,
                _ => return None,
            }
        }
        reader.end()?;
        let made = NewUser { name: name?, age: age?, email: email? };
        let fits = !made.name.is_empty()
            && made.name.chars().count() <= 40
            && (13..=120).contains(&made.age)
            && made.email.contains('@');
        fits.then_some(made)
    }
}

impl NewOrder {
    fn load(text: &str) -> Option<Self> {
        let mut reader = Reader::new(text);
        let (mut user, mut item, mut quantity, mut price) = (None, None, None, None);
        let mut first = true;
        while let Some(field) = reader.field(first)? {
            first = false;
            match field.as_str() {
                "user" => once(&mut user, reader.int())?,
                "item" => once(&mut item, reader.text())?,
                "quantity" => once(&mut quantity, reader.int())?,
                "price" => once(&mut price, reader.int())?,
                _ => return None,
            }
        }
        reader.end()?;
        let made = NewOrder { user: user?, item: item?, quantity: quantity?, price: price? };
        let fits = made.user >= 0 && !made.item.is_empty() && (1..=100).contains(&made.quantity) && made.price >= 0;
        fits.then_some(made)
    }
}

impl AgeChange {
    fn load(text: &str) -> Option<Self> {
        let mut reader = Reader::new(text);
        let mut age = None;
        let mut first = true;
        while let Some(field) = reader.field(first)? {
            first = false;
            match field.as_str() {
                "age" => once(&mut age, reader.int())?,
                _ => return None,
            }
        }
        reader.end()?;
        let made = AgeChange { age: age? };
        (13..=120).contains(&made.age).then_some(made)
    }
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

impl User {
    fn dump(&self) -> String {
        let mut out = String::new();
        let _ = write!(out, "{{\"id\": {}, \"name\": ", self.id);
        quoted(&mut out, &self.name);
        let _ = write!(out, ", \"age\": {}, \"email\": ", self.age);
        quoted(&mut out, &self.email);
        let _ = write!(out, ", \"spent\": {}, \"orders\": {}}}", self.spent, self.orders);
        out
    }
}

impl Order {
    fn dump_into(&self, out: &mut String) {
        let _ = write!(out, "{{\"id\": {}, \"item\": ", self.id);
        quoted(out, &self.item);
        let _ = write!(out, ", \"quantity\": {}, \"price\": {}}}", self.quantity, self.price);
    }
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// A request as a client writes it: a request line, headers and, with a body, its length.
fn written(method: &str, path: &str, body: &str) -> String {
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: api.den.example\r\n");
    if !body.is_empty() {
        text.push_str("Content-Type: application/json\r\n");
        let _ = write!(text, "Content-Length: {}\r\n", body.len());
    }
    text.push_str("\r\n");
    text.push_str(body);
    text
}

/// The corpus: every kind of request, a few of them broken the ways clients break them.
fn corpus(seed: i64) -> Vec<String> {
    let mut state = seed;
    let mut requests = Vec::new();
    for _ in 0..CORPUS {
        state = next(state);
        let kind = below(state, 100);
        state = next(state);
        let who = below(state, SEEN);
        state = next(state);
        let amount = below(state, 130);
        requests.push(if kind < 30 {
            // Every twentieth new user is too young, and so refused.
            let age = if who % 20 == 0 { 9 } else { 13 + amount % 90 };
            let body = format!("{{\"name\": \"fox-{who}\", \"age\": {age}, \"email\": \"fox-{who}@den.example\"}}");
            written("POST", "/users", &body)
        } else if kind < 55 {
            written("GET", &format!("/users/{who}"), "")
        } else if kind < 80 {
            let item = ITEMS[(amount as usize) % ITEMS.len()];
            let quantity = amount % 12;
            let price = amount * 3;
            let body = format!("{{\"user\": {who}, \"item\": \"{item}\", \"quantity\": {quantity}, \"price\": {price}}}");
            written("POST", "/orders", &body)
        } else if kind < 90 {
            written("GET", &format!("/orders/{who}"), "")
        } else if kind < 95 {
            written("PATCH", &format!("/users/{who}"), &format!("{{\"age\": {amount}}}"))
        } else if kind < 97 {
            written("GET", "/stats", "")
        } else if kind < 98 {
            written("POST", "/users", "{\"name\": \"fox\", \"age\": }")
        } else if kind < 99 {
            written("DELETE", &format!("/users/{who}"), "")
        } else {
            written("GET", &format!("/admin/{who}"), "")
        });
    }
    requests
}

/// Splits a request's text into its head's parts and its body, or gives nothing when it isn't
/// a request.
fn parsed(text: &str) -> Option<Request<'_>> {
    let split = text.find("\r\n\r\n")?;
    let mut lines = text[..split].split("\r\n");
    let words: Vec<&str> = lines.next()?.split(' ').collect();
    if words.len() != 3 {
        return None;
    }
    let mut headers = HashMap::new();
    for line in lines {
        let colon = line.find(": ")?;
        headers.insert(&line[..colon], &line[colon + 2..]);
    }
    Some(Request { method: words[0], path: words[1], headers, body: &text[split + 4..] })
}

/// Whether `request` says its body is JSON and gives its length right.
fn checked_body(request: &Request) -> bool {
    let kind = request.headers.get("Content-Type").copied().unwrap_or("");
    let length = request.headers.get("Content-Length").copied().unwrap_or("-");
    let Ok(says) = length.parse::<usize>() else {
        return false;
    };
    kind == "application/json" && says == request.body.len()
}

fn failed(status: i64, message: &str) -> Response {
    Response { status, body: format!("{{\"error\": \"{message}\"}}") }
}

fn create_user(service: &mut Service, request: &Request) -> Response {
    if !checked_body(request) {
        return failed(415, "a JSON body is needed");
    }
    let Some(made) = NewUser::load(request.body) else {
        return failed(400, "the body isn't a user");
    };
    let id = service.next_user;
    service.next_user += 1;
    let user = User { id, name: made.name, age: made.age, email: made.email, spent: 0, orders: 0 };
    let text = user.dump();
    service.users.insert(id, user);
    Response { status: 201, body: text }
}

fn show_user(service: &mut Service, id: i64) -> Response {
    if let Some(cached) = service.cache.get(&id) {
        return Response { status: 200, body: cached.clone() };
    }
    let Some(user) = service.users.get(&id) else {
        return failed(404, &format!("no user {id}"));
    };
    let text = user.dump();
    service.cache.insert(id, text.clone());
    Response { status: 200, body: text }
}

fn change_age(service: &mut Service, id: i64, request: &Request) -> Response {
    if !checked_body(request) {
        return failed(415, "a JSON body is needed");
    }
    let Some(change) = AgeChange::load(request.body) else {
        return failed(400, "the body isn't an age");
    };
    let Some(user) = service.users.get_mut(&id) else {
        return failed(404, &format!("no user {id}"));
    };
    user.age = change.age;
    let body = user.dump();
    service.cache.remove(&id);
    Response { status: 200, body }
}

fn place_order(service: &mut Service, request: &Request) -> Response {
    if !checked_body(request) {
        return failed(415, "a JSON body is needed");
    }
    let Some(made) = NewOrder::load(request.body) else {
        return failed(400, "the body isn't an order");
    };
    let Some(user) = service.users.get_mut(&made.user) else {
        return failed(404, &format!("no user {}", made.user));
    };
    let order = Order { id: service.next_order, item: made.item, quantity: made.quantity, price: made.price };
    service.next_order += 1;
    user.spent += made.quantity * made.price;
    user.orders += 1;
    service.cache.remove(&made.user);
    let mut body = String::new();
    order.dump_into(&mut body);
    service.orders.entry(made.user).or_default().push(order);
    Response { status: 201, body }
}

/// A user's latest orders, newest last, with how many there are and what they cost.
fn list_orders(service: &Service, id: i64) -> Response {
    if !service.users.contains_key(&id) {
        return failed(404, &format!("no user {id}"));
    }
    let orders = service.orders.get(&id).map_or(&[][..], Vec::as_slice);
    let total: i64 = orders.iter().map(|order| order.quantity * order.price).sum();
    let mut text = format!("{{\"user\": {id}, \"count\": {}, \"total\": {total}, \"orders\": [", orders.len());
    let first = orders.len().saturating_sub(SHOWN);
    for (at, order) in orders.iter().enumerate().skip(first) {
        if at > first {
            text.push_str(", ");
        }
        order.dump_into(&mut text);
    }
    text.push_str("]}");
    Response { status: 200, body: text }
}

fn route(service: &mut Service, request: &Request) -> Response {
    let path = request.path;
    if path == "/users" {
        if request.method != "POST" {
            return failed(405, "use POST");
        }
        return create_user(service, request);
    }
    if path == "/orders" {
        if request.method != "POST" {
            return failed(405, "use POST");
        }
        return place_order(service, request);
    }
    if path == "/stats" {
        return Response { status: 200, body: service.stats.clone() };
    }
    if let Some(rest) = path.strip_prefix("/users/") {
        let Ok(id) = rest.parse::<i64>() else {
            return failed(400, "a user is a number");
        };
        return match request.method {
            "GET" => show_user(service, id),
            "PATCH" => change_age(service, id, request),
            _ => failed(405, "use GET or PATCH"),
        };
    }
    if let Some(rest) = path.strip_prefix("/orders/") {
        let Ok(id) = rest.parse::<i64>() else {
            return failed(400, "a user is a number");
        };
        return list_orders(service, id);
    }
    failed(404, &format!("nothing at {path}"))
}

fn reason(status: i64) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Unsupported Media Type",
    }
}

/// Handles one request's text, giving the response's status and text.
fn handle(service: &mut Service, text: &str) -> (i64, String) {
    let mut response = failed(400, "not a request");
    if let Some(request) = parsed(text) {
        response = route(service, &request);
        let section = request.path.split('/').nth(1).expect("a path starts with /");
        *service.hits.entry(format!("{} {section}", request.method)).or_insert(0) += 1;
    }
    let mut out = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
    out.push_str("Content-Type: application/json\r\n");
    let _ = write!(out, "Content-Length: {}\r\n\r\n", response.body.len());
    out.push_str(&response.body);
    (response.status, out)
}

/// The end of a day: what the users spent, who orders most, and how often each route was asked
/// for, which then starts again from nothing.
fn end_day(service: &mut Service, day: i64) {
    let spent: i64 = service.users.values().map(|user| user.spent).sum();
    let placed: usize = service.orders.values().map(Vec::len).sum();
    let busy: Vec<i64> = service.orders.iter().filter(|(_, orders)| orders.len() >= 10).map(|(id, _)| *id).collect();
    let mut text = format!("{{\"day\": {day}, \"users\": {}, \"spent\": {spent}, ", service.users.len());
    let _ = write!(text, "\"orders\": {placed}, \"busy\": {}, \"hits\": {{", busy.len());
    let mut names: Vec<&String> = service.hits.keys().collect();
    names.sort();
    for (at, name) in names.into_iter().enumerate() {
        if at > 0 {
            text.push_str(", ");
        }
        let _ = write!(text, "\"{name}\": {}", service.hits[name]);
    }
    text.push_str("}}");
    service.stats = text;
    service.hits.clear();
    service.cache.clear();
}

fn main() {
    let started = std::time::Instant::now();
    let count = black_box(50_000_i64);
    let requests = corpus(black_box(7));
    let mut service = Service { stats: "{}".to_string(), ..Service::default() };
    let mut checksum: i64 = 0;
    let mut statuses: i64 = 0;
    for at in 0..count {
        let (status, out) = handle(&mut service, &requests[at as usize % CORPUS]);
        checksum = (checksum * 31 + out.len() as i64 * 7 + status) % 1_000_000_007;
        statuses += status;
        if (at + 1) % DAY == 0 {
            end_day(&mut service, (at + 1) / DAY);
        }
    }
    println!("{checksum} {statuses} {} {}", service.users.len(), service.stats.len());
    println!("{}", service.stats);
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
