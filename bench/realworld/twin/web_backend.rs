//! The web backend as a Rust program writes it on the standard library, with the crates the
//! game server's twin uses: a thread for each connection over `std::net`, HTTP/1.1 read and
//! written by hand, serde_json for the bodies, rusqlite for the users and notes, HTML escaped by
//! hand, and the runtime's own SHA-256 and HMAC, since Rust's standard library has none. It does
//! what `web_backend.fen` does, step for step, and prints the same summary. JSON is written with
//! a space after `:` and `,`, as `json.dumps` does, so both sides send the same bytes.

#[path = "sha256.rs"]
mod sha256;

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::ser::{Formatter, Serializer};

const CLIENTS: i64 = 32;
const NOTES: i64 = 12;
const STRETCH: i64 = 1_000;
const TOKEN_LIFE: i64 = 3_600;
const MOST_BODY: usize = 65_536;
const SEARCHES: [&str; 5] = ["fox", "<b>", "&", "river", "nothing like this"];
const WORDS: [&str; 10] = [
    "fox",
    "den",
    "river",
    "<b>bold</b>",
    "fish & chips",
    "\"quoted\"",
    "it's",
    "<script>alert(1)</script>",
    "moss",
    "stone",
];

/// Writes `, ` and `: `, the way `json.dumps` does.
struct Spaced;

impl Formatter for Spaced {
    fn begin_array_value<W: ?Sized + Write>(&mut self, out: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { out.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, out: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { out.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.write_all(b": ")
    }
}

fn dumps<T: Serialize>(value: &T) -> String {
    let mut out = Vec::new();
    value.serialize(&mut Serializer::with_formatter(&mut out, Spaced)).expect("it writes");
    String::from_utf8(out).expect("JSON is text")
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    name: String,
    password: String,
}

#[derive(Serialize, Deserialize)]
struct Note {
    title: String,
    body: String,
}

#[derive(Serialize, Deserialize)]
struct Signed {
    token: String,
}

#[derive(Serialize)]
struct Account {
    name: String,
}

#[derive(Serialize)]
struct Problem {
    error: String,
}

/// What every connection's thread reaches: the database and the key tokens are signed with.
struct App {
    db: Mutex<Connection>,
    key: String,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn hmac_sha256(key: &str, data: &str) -> String {
    hex(&sha256::hmac(key.as_bytes(), data.as_bytes()))
}

/// Compares in time that depends only on the lengths, as `std.crypto.equal` does.
fn equal(first: &str, second: &str) -> bool {
    let (first, second) = (first.as_bytes(), second.as_bytes());
    if first.len() != second.len() {
        return false;
    }
    first.iter().zip(second).fold(0u8, |differ, (a, b)| differ | (a ^ b)) == 0
}

fn random_hex(count: usize) -> String {
    let mut bytes = vec![0u8; count];
    std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).expect("random bytes");
    hex(&bytes)
}

fn seconds_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("after 1970").as_secs() as i64
}

fn stretched(password: &str, salt: &str) -> String {
    let mut digest = hmac_sha256(salt, password);
    for _ in 0..STRETCH - 1 {
        digest = hmac_sha256(password, &digest);
    }
    digest
}

fn signed_token(key: &str, name: &str, expires: i64) -> String {
    let claim = format!("{name}.{expires}");
    let signature = hmac_sha256(key, &claim);
    format!("{claim}.{signature}")
}

fn signed_in(key: &str, request: &Request) -> Option<String> {
    let header = request.headers.get("authorization")?;
    let parts: Vec<&str> = header.strip_prefix("Bearer ").unwrap_or(header).split('.').collect();
    let [name, expires, signature] = parts[..] else { return None };
    let expires_at: i64 = expires.parse().ok()?;
    if expires_at < seconds_now() {
        return None;
    }
    if !equal(&hmac_sha256(key, &format!("{name}.{expires}")), signature) {
        return None;
    }
    Some(name.to_owned())
}

fn good_name(name: &str) -> bool {
    (3..=20).contains(&name.chars().count())
        && name.chars().all(|letter| letter.is_ascii_lowercase() || letter.is_ascii_digit())
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for letter in text.chars() {
        match letter {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(letter),
        }
    }
    out
}

struct Request {
    method: String,
    path: String,
    query: BTreeMap<String, String>,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

struct Response {
    status: i64,
    content_type: &'static str,
    body: Vec<u8>,
}

fn answer(status: i64, text: String) -> Response {
    Response { status, content_type: "application/json", body: text.into_bytes() }
}

fn problem(status: i64, error: &str) -> Response {
    answer(status, dumps(&Problem { error: error.to_owned() }))
}

/// A failure of the server's own, which its client gets as a 500.
type Broke = rusqlite::Error;

fn sign_up(app: &App, request: &Request) -> Result<Response, Broke> {
    let wanted: Credentials = match serde_json::from_slice(&request.body) {
        Ok(wanted) => wanted,
        Err(why) => return Ok(problem(400, &why.to_string())),
    };
    if !good_name(&wanted.name) {
        return Ok(problem(400, "a name is 3 to 20 lowercase letters and digits"));
    }
    if wanted.password.chars().count() < 8 {
        return Ok(problem(400, "a password has at least 8 characters"));
    }
    let salt = random_hex(16);
    let hash = stretched(&wanted.password, &salt);
    let added = app.db.lock().expect("not poisoned").execute(
        "insert into users (name, salt, hash) values (?1, ?2, ?3) on conflict (name) do nothing",
        params![wanted.name, salt, hash],
    )?;
    if added == 0 {
        return Ok(problem(409, &format!("{} is taken", wanted.name)));
    }
    Ok(answer(201, dumps(&Account { name: wanted.name })))
}

fn sign_in(app: &App, request: &Request) -> Result<Response, Broke> {
    let given: Credentials = match serde_json::from_slice(&request.body) {
        Ok(given) => given,
        Err(why) => return Ok(problem(400, &why.to_string())),
    };
    let found: Option<(String, String)> = app
        .db
        .lock()
        .expect("not poisoned")
        .query_row("select salt, hash from users where name = ?1", params![given.name], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    // A name nobody has takes as long to refuse as a wrong password.
    let (salt, hash) = found.clone().unwrap_or_else(|| ("nobody".to_owned(), String::new()));
    let tried = stretched(&given.password, &salt);
    if found.is_none() || !equal(&tried, &hash) {
        return Ok(problem(401, "that name and password don't match"));
    }
    let token = signed_token(&app.key, &given.name, seconds_now() + TOKEN_LIFE);
    Ok(answer(200, dumps(&Signed { token })))
}

fn add_note(app: &App, owner: &str, request: &Request) -> Result<Response, Broke> {
    let note: Note = match serde_json::from_slice(&request.body) {
        Ok(note) => note,
        Err(why) => return Ok(problem(400, &why.to_string())),
    };
    if note.title.is_empty() {
        return Ok(problem(400, "a note needs a title"));
    }
    app.db.lock().expect("not poisoned").execute(
        "insert into notes (owner, title, body) values (?1, ?2, ?3)",
        params![owner, note.title, note.body],
    )?;
    Ok(answer(201, dumps(&note)))
}

fn notes_where(app: &App, query: &str, values: &[&str]) -> Result<Vec<Note>, Broke> {
    let db = app.db.lock().expect("not poisoned");
    let mut statement = db.prepare(query)?;
    let rows = statement.query_map(rusqlite::params_from_iter(values), |row| {
        Ok(Note { title: row.get(0)?, body: row.get(1)? })
    })?;
    rows.collect()
}

fn notes_of(app: &App, owner: &str) -> Result<Response, Broke> {
    let found = notes_where(app, "select title, body from notes where owner = ?1 order by id", &[owner])?;
    Ok(answer(200, dumps(&found)))
}

fn search(app: &App, owner: &str, request: &Request) -> Result<Response, Broke> {
    let words = request.query.get("q").map_or("", String::as_str);
    let found = notes_where(
        app,
        "select title, body from notes where owner = ?1 and (instr(lower(title), lower(?2)) > 0 or instr(lower(body), lower(?2)) > 0) order by id",
        &[owner, words],
    )?;
    let rows: String = found
        .iter()
        .map(|note| format!("<li><b>{}</b> {}</li>", escape(&note.title), escape(&note.body)))
        .collect();
    let page = format!(
        "<h1>{}'s notes with “{}”</h1><p>{} found</p><ul>{rows}</ul>",
        escape(owner),
        escape(words),
        found.len()
    );
    Ok(Response { status: 200, content_type: "text/html; charset=utf-8", body: page.into_bytes() })
}

fn respond(app: &App, request: &Request) -> Result<Response, Broke> {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/signup") => return sign_up(app, request),
        ("POST", "/signin") => return sign_in(app, request),
        _ => {}
    }
    let Some(owner) = signed_in(&app.key, request) else { return Ok(problem(401, "sign in first")) };
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/notes") => add_note(app, &owner, request),
        ("GET", "/notes") => notes_of(app, &owner),
        ("GET", "/search") => search(app, &owner, request),
        _ => Ok(problem(404, "there's nothing here")),
    }
}

/// HTTP/1.1 this program can't read, from either side, or the network failing.
#[derive(Debug)]
enum HttpError {
    Net(io::Error),
    Bad(&'static str),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Net(why) => write!(out, "{why}"),
            HttpError::Bad(reason) => write!(out, "{reason}"),
        }
    }
}

impl From<io::Error> for HttpError {
    fn from(why: io::Error) -> HttpError {
        HttpError::Net(why)
    }
}

fn line_of(reader: &mut BufReader<TcpStream>) -> Result<Option<String>, HttpError> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let trimmed = line.strip_suffix('\n').unwrap_or(&line);
    Ok(Some(trimmed.strip_suffix('\r').unwrap_or(trimmed).to_owned()))
}

fn rest_of(reader: &mut BufReader<TcpStream>) -> Result<(BTreeMap<String, String>, Vec<u8>), HttpError> {
    let mut headers = BTreeMap::new();
    loop {
        let header = line_of(reader)?.ok_or(HttpError::Bad("the headers didn't end"))?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').ok_or(HttpError::Bad("a header has no `:`"))?;
        headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
    }
    let length: usize = headers
        .get("content-length")
        .map_or(Ok(0), |length| length.parse())
        .map_err(|_| HttpError::Bad("its length isn't a number"))?;
    if length > MOST_BODY {
        return Err(HttpError::Bad("its body is too big"));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).map_err(|_| HttpError::Bad("its body ended early"))?;
    Ok((headers, body))
}

fn unhex(digit: u8) -> Option<u8> {
    (digit as char).to_digit(16).map(|value| value as u8)
}

fn decoded(text: &str, plus: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                if let (Some(high), Some(low)) = (unhex(bytes[at + 1]), unhex(bytes[at + 2])) {
                    out.push(high * 16 + low);
                    at += 3;
                    continue;
                }
                out.push(b'%');
            }
            b'+' if plus => out.push(b' '),
            byte => out.push(byte),
        }
        at += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn decode_query(text: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for pair in text.split('&').filter(|pair| !pair.is_empty()) {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        found.entry(decoded(name, true)).or_insert_with(|| decoded(value, true));
    }
    found
}

fn encode_query(name: &str, value: &str) -> String {
    let form = |text: &str| -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    };
    format!("{}={}", form(name), form(value))
}

fn request_from(reader: &mut BufReader<TcpStream>) -> Result<Request, HttpError> {
    let line = line_of(reader)?.ok_or(HttpError::Bad("the client sent nothing"))?;
    let pieces: Vec<&str> = line.split(' ').collect();
    let [method, target, "HTTP/1.1"] = pieces[..] else {
        return Err(HttpError::Bad("that isn't an HTTP/1.1 request line"));
    };
    let (headers, body) = rest_of(reader)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Request {
        method: method.to_owned(),
        path: decoded(path, false),
        query: if target.contains('?') { decode_query(query) } else { BTreeMap::new() },
        headers,
        body,
    })
}

fn reason(status: i64) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    }
}

fn written(conn: &mut TcpStream, response: &Response) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
    if !response.content_type.is_empty() {
        head += &format!("content-type: {}\r\n", response.content_type);
    }
    head += &format!("content-length: {}\r\nconnection: close\r\n\r\n", response.body.len());
    conn.write_all(head.as_bytes())?;
    conn.write_all(&response.body)
}

/// Answers a connection's one request. Gives whether the program asked the server to stop.
fn answered(mut conn: TcpStream, app: &App, stop: &str) -> Result<bool, HttpError> {
    let mut reader = BufReader::new(conn.try_clone()?);
    let request = match request_from(&mut reader) {
        Ok(request) => request,
        Err(HttpError::Bad(reason)) => {
            written(&mut conn, &problem(400, reason))?;
            return Ok(false);
        }
        Err(why) => return Err(why),
    };
    if request.path == "/stop" {
        let asked = request.headers.get("authorization").map_or("", |header| {
            header.strip_prefix("Bearer ").unwrap_or(header)
        });
        if equal(asked, stop) {
            written(&mut conn, &Response { status: 204, content_type: "", body: Vec::new() })?;
            return Ok(true);
        }
    }
    let made = respond(app, &request).unwrap_or_else(|why| problem(500, &why.to_string()));
    written(&mut conn, &made)?;
    Ok(false)
}

fn serving(server: &TcpListener, app: &App, stop: &str, dropped: &Mutex<Vec<String>>) {
    std::thread::scope(|scope| {
        let (done_sender, done) = std::sync::mpsc::channel::<()>();
        for conn in server.incoming() {
            if done.try_recv().is_ok() {
                break;
            }
            let Ok(conn) = conn else { continue };
            let done_sender = done_sender.clone();
            let address = server.local_addr().expect("listening");
            scope.spawn(move || match answered(conn, app, stop) {
                Ok(true) => {
                    let _ = done_sender.send(());
                    // Wakes the accept loop, so it sees it's done.
                    let _ = TcpStream::connect(address);
                }
                Ok(false) => {}
                Err(why) => dropped.lock().expect("not poisoned").push(why.to_string()),
            });
        }
    });
}

#[derive(Default)]
struct Visit {
    name: String,
    statuses: BTreeMap<i64, i64>,
    listed: i64,
    found: i64,
    escaped: i64,
    trail: i64,
    failed: String,
}

impl Visit {
    fn saw(&mut self, answer: &Answer) {
        *self.statuses.entry(answer.status).or_insert(0) += 1;
    }
}

fn mixed(hash: i64, value: i64) -> i64 {
    (hash * 31 + value) % 1_000_000_007
}

fn text_mixed(hash: i64, text: &str) -> i64 {
    text.bytes().fold(hash, |hash, byte| mixed(hash, i64::from(byte)))
}

fn word(at: i64) -> &'static str {
    WORDS[(at % WORDS.len() as i64) as usize]
}

struct Answer {
    status: i64,
    text: String,
}

fn asked(port: u16, method: &str, path: &str, body: &str, token: &str) -> Result<Answer, HttpError> {
    let mut conn = TcpStream::connect(("127.0.0.1", port))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\n");
    if !token.is_empty() {
        head += &format!("authorization: Bearer {token}\r\n");
    }
    head += &format!("content-length: {}\r\nconnection: close\r\n\r\n", body.len());
    conn.write_all(head.as_bytes())?;
    conn.write_all(body.as_bytes())?;
    let mut reader = BufReader::new(conn);
    let line = line_of(&mut reader)?.ok_or(HttpError::Bad("the server sent nothing"))?;
    let status = line
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or(HttpError::Bad("that isn't a status line"))?;
    let (_, body) = rest_of(&mut reader)?;
    Ok(Answer { status, text: String::from_utf8(body).unwrap_or_default() })
}

fn posted(port: u16, path: &str, body: &str, token: &str) -> Result<Answer, HttpError> {
    asked(port, "POST", path, body, token)
}

fn got(port: u16, path: &str, token: &str) -> Result<Answer, HttpError> {
    asked(port, "GET", path, "", token)
}

/// A client's failure: the network's or the server's HTTP, or a body that isn't what it should be.
fn visit(port: u16, number: i64) -> Result<Visit, String> {
    let fail = |why: HttpError| why.to_string();
    let credentials =
        Credentials { name: format!("user{number}"), password: format!("correct horse {number} battery") };
    let signing_up = dumps(&credentials);
    let mut visit = Visit { name: credentials.name.clone(), ..Visit::default() };
    visit.saw(&posted(port, "/signup", &signing_up, "").map_err(fail)?);
    if number % 5 == 0 {
        visit.saw(&posted(port, "/signup", &signing_up, "").map_err(fail)?);
    }
    if number % 3 == 0 {
        let wrong = Credentials { name: credentials.name.clone(), password: "not the password".to_owned() };
        visit.saw(&posted(port, "/signin", &dumps(&wrong), "").map_err(fail)?);
    }
    let signed = posted(port, "/signin", &signing_up, "").map_err(fail)?;
    visit.saw(&signed);
    let token = serde_json::from_str::<Signed>(&signed.text).map_err(|why| why.to_string())?.token;
    for at in 0..NOTES {
        let note = Note {
            title: format!("{} {at}", word(number + at)),
            body: format!("{} and {}", word(number * 3 + at), word(at * 7)),
        };
        visit.saw(&posted(port, "/notes", &dumps(&note), &token).map_err(fail)?);
    }
    let listed = got(port, "/notes", &token).map_err(fail)?;
    visit.saw(&listed);
    for note in serde_json::from_str::<Vec<Note>>(&listed.text).map_err(|why| why.to_string())? {
        visit.listed += 1;
        visit.trail = text_mixed(visit.trail, &(note.title + &note.body));
    }
    for words in SEARCHES {
        let page = got(port, &format!("/search?{}", encode_query("q", words)), &token).map_err(fail)?;
        visit.saw(&page);
        visit.found += page.text.matches("<li>").count() as i64;
        visit.escaped += page.text.matches("&lt;").count() as i64;
        visit.trail = text_mixed(visit.trail, &page.text);
    }
    // No token, and a token changed by hand, get nothing.
    visit.saw(&got(port, "/notes", "").map_err(fail)?);
    visit.saw(&got(port, "/notes", &format!("{token}0")).map_err(fail)?);
    Ok(visit)
}

fn client(port: u16, number: i64) -> Visit {
    visit(port, number).unwrap_or_else(|why| Visit { name: format!("user{number}"), failed: why, ..Visit::default() })
}

fn run_load(server: &TcpListener, app: &App, dropped: &Mutex<Vec<String>>) -> Vec<Visit> {
    let port = server.local_addr().expect("listening").port();
    let stop = random_hex(32);
    std::thread::scope(|scope| {
        let stop = stop.as_str();
        scope.spawn(move || serving(server, app, stop, dropped));
        let clients: Vec<_> = (0..CLIENTS).map(|number| scope.spawn(move || client(port, number))).collect();
        let visits = clients.into_iter().map(|one| one.join().expect("a client ran")).collect();
        let _ = posted(port, "/stop", "", stop);
        visits
    })
}

fn made(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "create table if not exists users (name text primary key, salt text, hash text);
         create table if not exists notes (id integer primary key, owner text, title text, body text);
         delete from users;
         delete from notes;",
    )
}

fn report(visits: &[Visit], db: &Connection, dropped: &[String]) -> rusqlite::Result<Vec<String>> {
    let failed: Vec<&Visit> = visits.iter().filter(|one| !one.failed.is_empty()).collect();
    let mut lines = vec![format!("{} clients, {} failed, {} dropped", visits.len(), failed.len(), dropped.len())];
    for one in &failed {
        lines.push(format!("  {}: {}", one.name, one.failed));
    }
    let mut statuses = BTreeMap::new();
    for one in visits {
        for (status, count) in &one.statuses {
            *statuses.entry(*status).or_insert(0) += count;
        }
    }
    lines.push(statuses.iter().map(|(status, count)| format!("{status}: {count}")).collect::<Vec<_>>().join(", "));
    lines.push(format!("notes listed: {}", visits.iter().map(|one| one.listed).sum::<i64>()));
    lines.push(format!("search results: {}", visits.iter().map(|one| one.found).sum::<i64>()));
    lines.push(format!("escaped: {}", visits.iter().map(|one| one.escaped).sum::<i64>()));
    let trail = visits.iter().fold(0, |trail, one| mixed(trail, one.trail));
    lines.push(format!("trail: {trail}"));
    let (users, notes, owners): (i64, i64, i64) = db.query_row(
        "select (select count(*) from users) as users, count(*) as notes, count(distinct owner) as owners from notes",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    lines.push(format!("kept: {users} users, {notes} notes from {owners}"));
    Ok(lines)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dropped = Mutex::new(Vec::new());
    let folder = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
    let server = TcpListener::bind("127.0.0.1:0")?;
    let path = format!("{folder}/rust-web-backend-{}.db", server.local_addr()?.port());
    let lines = {
        let db = Connection::open(&path)?;
        let _: String = db.query_row("pragma journal_mode=wal", [], |row| row.get(0))?;
        made(&db)?;
        let app = App { db: Mutex::new(db), key: random_hex(32) };
        let visits = run_load(&server, &app, &dropped);
        let dropped = dropped.lock().expect("not poisoned").clone();
        let db = app.db.lock().expect("not poisoned");
        report(&visits, &db, &dropped)?
    };
    for leftover in [path.clone(), format!("{path}-wal"), format!("{path}-shm")] {
        if std::path::Path::new(&leftover).exists() {
            std::fs::remove_file(&leftover)?;
        }
    }
    let mut out = io::stdout().lock();
    for line in lines {
        writeln!(out, "{line}")?;
    }
    Ok(())
}
