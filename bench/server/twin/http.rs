//! The twin of `http.fen`: the same three routes over HTTP/1.1, each connection kept open for its
//! requests. A thread for each connection over `std::net`, as `echo` and `chat_threads` have, the
//! request read with a `BufReader` into its method, path, headers by their names in lower case and
//! body, routed with `matchit` (what axum routes with), and JSON read and written with serde. It
//! prints its port, then serves until it's killed.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::Serializer;
use serde_json::ser::Formatter;

const PRICE: i64 = 250;

#[derive(Serialize)]
struct User {
    id: i64,
    name: String,
    active: bool,
}

#[derive(Deserialize)]
struct Order {
    item: String,
    count: i64,
}

#[derive(Serialize)]
struct Placed {
    item: String,
    count: i64,
    total: i64,
}

struct Request {
    method: String,
    path: String,
    #[expect(dead_code, reason = "read as Fennec's server reads them, though no route looks")]
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    json: bool,
    body: Vec<u8>,
}

#[derive(Clone, Copy)]
enum Route {
    Hello,
    User,
    Orders,
}

/// Writes `, ` and `: `, the way `json.dumps` does, as the web_backend twin's does.
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

fn answer(status: u16, body: String) -> Response {
    Response { status, json: true, body: body.into_bytes() }
}

fn user(id: &str) -> Response {
    let Ok(number) = id.parse::<i64>() else {
        return Response { status: 404, json: false, body: b"there's nothing here".to_vec() };
    };
    let made = User { id: number, name: format!("user {number}"), active: number % 2 == 0 };
    answer(200, dumps(&made))
}

fn placed(request: &Request) -> Response {
    let Ok(wanted) = serde_json::from_slice::<Order>(&request.body) else {
        return Response { status: 400, json: false, body: b"that isn't an order".to_vec() };
    };
    let total = wanted.count.checked_mul(PRICE).expect("a total in range");
    let made = Placed { item: wanted.item, count: wanted.count, total };
    answer(201, dumps(&made))
}

fn respond(router: &matchit::Router<Route>, request: &Request) -> Response {
    let Ok(found) = router.at(&request.path) else {
        return Response { status: 404, json: false, body: b"there's nothing here".to_vec() };
    };
    match (*found.value, request.method.as_str()) {
        (Route::Hello, "GET") => Response { status: 200, json: false, body: b"hello".to_vec() },
        (Route::User, "GET") => user(found.params.get("id").unwrap_or("")),
        (Route::Orders, "POST") => placed(request),
        _ => Response { status: 405, json: false, body: b"not allowed".to_vec() },
    }
}

/// The next request on a connection, with whether it stays open after, or `None` once the client
/// is done.
fn request_from(reader: &mut BufReader<TcpStream>) -> io::Result<Option<(Request, bool)>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.trim_end().splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("");
    let path = target.split('?').next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length = headers.get("content-length").and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
    let mut body = vec![0; length];
    io::Read::read_exact(reader, &mut body)?;
    let keep = headers.get("connection").is_none_or(|asked| asked != "close");
    Ok(Some((Request { method, path, headers, body }, keep)))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Method Not Allowed",
    }
}

fn answer_all(stream: TcpStream, router: &matchit::Router<Route>) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some((request, keep)) = request_from(&mut reader)? {
        let response = respond(router, &request);
        let mut out = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
        if response.json {
            out.push_str("content-type: application/json\r\n");
        }
        out.push_str(&format!("content-length: {}\r\n", response.body.len()));
        if !keep {
            out.push_str("connection: close\r\n");
        }
        out.push_str("\r\n");
        let mut whole = out.into_bytes();
        whole.extend_from_slice(&response.body);
        writer.write_all(&whole)?;
        if !keep {
            break;
        }
    }
    Ok(())
}

fn main() {
    let mut router = matchit::Router::new();
    router.insert("/", Route::Hello).expect("a route");
    router.insert("/users/{id}", Route::User).expect("a route");
    router.insert("/orders", Route::Orders).expect("a route");
    let router: &'static matchit::Router<Route> = Box::leak(Box::new(router));
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
    println!("{}", listener.local_addr().expect("listening").port());
    io::stdout().flush().expect("the port printed");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        thread::spawn(move || {
            let _ = answer_all(stream, router);
        });
    }
}
