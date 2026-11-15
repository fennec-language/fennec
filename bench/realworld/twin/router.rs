//! The router benchmark as a Rust web app routes, with `matchit`: the same 64 routes and two
//! million requests as `router.fen`, a parameter decoded and parsed as `std.net`'s `Captured`
//! does, a 404 for a path nothing's at and a 405 saying the methods a path takes. It prints the
//! same summary.

use std::collections::BTreeMap;
use std::hint::black_box;

use matchit::{Params, Router};

const RESOURCES: [&str; 8] = ["users", "notes", "teams", "files", "orders", "items", "posts", "tags"];
const KINDS: usize = 1_024;

fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

struct Response {
    status: i64,
    body: Vec<u8>,
}

fn said(status: i64, text: String) -> Response {
    Response { status, body: text.into_bytes() }
}

fn not_found() -> Response {
    said(404, "there's nothing here".to_owned())
}

/// A parameter with its percent-escapes decoded, as `Captured.text_at` gives it.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                ((bytes[at + 1] as char).to_digit(16), (bytes[at + 2] as char).to_digit(16))
        {
            out.push((high * 16 + low) as u8);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

type Handler = Box<dyn Fn(&Params) -> Response>;

/// What a path answers, by method, in the order they were added.
#[derive(Default)]
struct Methods {
    handlers: Vec<(&'static str, Handler)>,
}

fn one(resource: &'static str, params: &Params, status: i64) -> Response {
    let Ok(id) = decoded(params.get("id").unwrap_or_default()).parse::<i64>() else { return not_found() };
    said(status, format!("{resource} {id}"))
}

fn comment(resource: &'static str, params: &Params) -> Response {
    let Ok(id) = decoded(params.get("id").unwrap_or_default()).parse::<i64>() else { return not_found() };
    let name = decoded(params.get("name").unwrap_or_default());
    said(200, format!("{resource} {id} comment {name}"))
}

fn routed() -> Router<Methods> {
    let mut paths: BTreeMap<String, Methods> = BTreeMap::new();
    let mut add = |method: &'static str, path: String, handler: Handler| {
        paths.entry(path).or_default().handlers.push((method, handler));
    };
    for resource in RESOURCES {
        add("GET", format!("/{resource}"), Box::new(move |_| said(200, resource.to_owned())));
        add("POST", format!("/{resource}"), Box::new(move |_| said(201, resource.to_owned())));
        add("GET", format!("/{resource}/search"), Box::new(|_| said(200, "search".to_owned())));
        add("GET", format!("/{resource}/{{id}}"), Box::new(move |params| one(resource, params, 200)));
        add("PUT", format!("/{resource}/{{id}}"), Box::new(move |params| one(resource, params, 200)));
        add("DELETE", format!("/{resource}/{{id}}"), Box::new(move |params| one(resource, params, 204)));
        add(
            "GET",
            format!("/{resource}/{{id}}/comments"),
            Box::new(move |params| one(resource, params, 200)),
        );
        add(
            "GET",
            format!("/{resource}/{{id}}/comments/{{name}}"),
            Box::new(move |params| comment(resource, params)),
        );
    }
    let mut router = Router::new();
    for (path, methods) in paths {
        router.insert(path, methods).expect("the routes don't clash");
    }
    router
}

fn respond(router: &Router<Methods>, method: &str, path: &str) -> Response {
    let Ok(found) = router.at(path) else { return not_found() };
    let asked = if method == "HEAD" && !found.value.handlers.iter().any(|(own, _)| *own == "HEAD") {
        "GET"
    } else {
        method
    };
    if let Some((_, handler)) = found.value.handlers.iter().find(|(own, _)| *own == asked) {
        return handler(&found.params);
    }
    let mut allowed: Vec<&str> = found.value.handlers.iter().map(|(own, _)| *own).collect();
    allowed.sort_unstable();
    let allowed = allowed.join(", ");
    said(405, format!("this takes {allowed}"))
}

fn request_of(seed: i64) -> (&'static str, String) {
    let resource = RESOURCES[below(seed, RESOURCES.len() as i64) as usize];
    let id = below(next(seed), 100_000);
    let roll = below(next(next(seed)), 100);
    match roll {
        0..15 => ("GET", format!("/{resource}")),
        15..20 => ("POST", format!("/{resource}")),
        20..25 => ("GET", format!("/{resource}/search")),
        25..45 => ("GET", format!("/{resource}/{id}")),
        45..52 => ("PUT", format!("/{resource}/{id}")),
        52..57 => ("DELETE", format!("/{resource}/{id}")),
        57..62 => ("GET", format!("/{resource}/{id}/comments")),
        62..70 => ("GET", format!("/{resource}/{id}/comments/c{roll}")),
        70..75 => ("GET", format!("/widgets/{id}")),
        75..80 => ("GET", format!("/{resource}/x{id}")),
        80..85 => ("GET", format!("/{resource}/{id}/comments/c{roll}/more")),
        85..90 => ("PATCH", format!("/{resource}")),
        90..95 => ("POST", format!("/{resource}/{id}")),
        _ => ("DELETE", format!("/{resource}/{id}/comments")),
    }
}

fn main() {
    let router = routed();
    let mut requests = Vec::with_capacity(KINDS);
    let mut seed = 2026;
    for _ in 0..KINDS {
        seed = next(seed);
        requests.push(request_of(seed));
    }
    let total: usize = black_box(2_000_000);
    let mut statuses: BTreeMap<i64, i64> = BTreeMap::new();
    let mut bytes_back = 0;
    for number in 0..total {
        let (method, path) = &requests[number % KINDS];
        let response = respond(&router, method, path);
        *statuses.entry(response.status).or_insert(0) += 1;
        bytes_back += response.body.len();
    }
    for (status, count) in statuses {
        println!("{status}: {count}");
    }
    println!("bytes back: {bytes_back}");
}
