//! The `router` target: `std.net`'s `Router`, built natively, asked for any request target with
//! each of `METHODS`, as the server would hand it a request: the path before any `?`, kept as it
//! was sent and decoded. A walk of the same routes here, by what `Router`'s documentation says,
//! gives each answer: the route's handler with its parameters decoded, a 404 for a path no route
//! has, or a 405 naming the methods the path takes.

use std::fmt::Write as _;

/// The routes the router has: each one's method and path, and the name its handler answers with.
const ROUTES: [(&str, &str, &str); 13] = [
    ("GET", "/", "root"),
    ("GET", "/notes", "notes"),
    ("POST", "/notes", "add"),
    ("GET", "/notes/new", "form"),
    ("GET", "/notes/{id}", "note"),
    ("DELETE", "/notes/{id}", "drop"),
    ("GET", "/notes/{id}/tags/{tag}", "tag"),
    ("GET", "/files/{name}", "file"),
    ("HEAD", "/files/{name}", "file-head"),
    ("PUT", "/files/{name}/", "file-put"),
    ("GET", "/é/{x}", "accent"),
    ("GET", "/a/{1}/{2}/{3}/{4}/{5}/{6}/{7}/{8}", "eight"),
    ("GET", "/a/{1}/b/{2}", "ab"),
];

/// The methods each target is asked with.
const METHODS: [&str; 5] = ["GET", "HEAD", "POST", "DELETE", "PUT"];

/// The program that routes its file's text, as a request target, with each of `METHODS`, a line
/// each: the status, then the body and the `allow` header in hex, after `b` and `a`.
pub(crate) fn reader() -> String {
    let mut source = "\
from std.net import Captured, Request, Response, Router
from std.url import unquote

def said(name: str, found: Captured, count: int) -> Response:
    mut text = name
    for at in range(count):
        text += \"|\" + found.text_at(at)
    return Response(status=200, body=text.encode())

def made() -> Router:
    mut router = Router()
"
    .to_owned();
    for (method, path, name) in ROUTES {
        let count = path.matches('{').count();
        let _ = writeln!(
            source,
            "    router.handle(\"{method} {path}\", (request, found) => said(\"{name}\", found, {count}))"
        );
    }
    source.push_str(
        "    return router

def main(fs: Fs, args: Args, stdout: Stdout) fails IoError:
    target = fs.read_text(args[0])?
    mut path = target
    at = target.find(\"?\")
    if at is not None:
        path = target[:at]
    decoded = unquote(path)
    router = made()
",
    );
    let methods: Vec<String> = METHODS.iter().map(|method| format!("\"{method}\"")).collect();
    let _ = write!(
        source,
        "    for method in [{}]:
        response = router.respond(Request(method=method, path=decoded, raw_path=path))
        allowed = response.headers.get(\"allow\") else \"\"
        stdout.print(f\"{{response.status}} b{{response.body.hex()}} a{{allowed.encode().hex()}}\")
",
        methods.join(", ")
    );
    source
}

/// One place in the tree of routes: the segments written out that can follow it, the parameter
/// that can, and the routes that end there, by method.
#[derive(Default)]
struct Node {
    statics: Vec<(String, usize)>,
    parameter: Option<usize>,
    methods: Vec<(&'static str, &'static str, usize)>,
}

/// The routes' tree, made as `Router.handle` makes it.
fn tree() -> Vec<Node> {
    let mut nodes = vec![Node::default()];
    for (method, path, name) in ROUTES {
        let mut node = 0;
        let mut count = 0;
        for segment in path.split('/').filter(|segment| !segment.is_empty()) {
            let parameter = segment.starts_with('{') && segment.ends_with('}');
            count += usize::from(parameter);
            // The place a new node would take, if this step needs one.
            let fresh = nodes.len();
            let Some(here) = nodes.get_mut(node) else { break };
            let next = if parameter {
                *here.parameter.get_or_insert(fresh)
            } else if let Some((_, next)) = here.statics.iter().find(|(written, _)| written == segment) {
                *next
            } else {
                here.statics.push((segment.to_owned(), fresh));
                fresh
            };
            if next == fresh {
                nodes.push(Node::default());
            }
            node = next;
        }
        if let Some(here) = nodes.get_mut(node) {
            here.methods.push((method, name, count));
        }
    }
    nodes
}

/// The most parameters a path can fill, as `MOST_ROUTE_PARAMETERS` has it.
const MOST_PARAMETERS: usize = 8;

/// What the router should answer `method` on `path`: the status, the body when it's a route's,
/// and the `allow` header.
fn answer(nodes: &[Node], method: &str, path: &str) -> (u16, Option<String>, String) {
    let not_found = (404, None, String::new());
    // The path's segments, each after a `/`, with one `/` at the end passed over. An empty path
    // has none, and one that doesn't start with `/` is found nowhere.
    let mut segments: Vec<&str> = Vec::new();
    if !path.is_empty() {
        let Some(after) = path.strip_prefix('/') else { return not_found };
        segments = after.split('/').collect();
        if segments.last() == Some(&"") {
            segments.pop();
        }
    }
    let mut node = 0;
    let mut found: Vec<&str> = Vec::new();
    for segment in segments {
        let Some(here) = nodes.get(node) else { return not_found };
        if let Some((_, next)) = here.statics.iter().find(|(written, _)| written == segment) {
            node = *next;
        } else if let Some(next) = here.parameter
            && !segment.is_empty()
            && found.len() < MOST_PARAMETERS
        {
            found.push(segment);
            node = next;
        } else {
            return not_found;
        }
    }
    let Some(here) = nodes.get(node) else { return not_found };
    let methods = &here.methods;
    if methods.is_empty() {
        return not_found;
    }
    let has = |wanted: &str| methods.iter().find(|(method, _, _)| *method == wanted);
    let asked = if method == "HEAD" && has("HEAD").is_none() { "GET" } else { method };
    if let Some((_, name, count)) = has(asked) {
        let mut body = (*name).to_owned();
        for segment in found.iter().take(*count) {
            body.push('|');
            body.push_str(&if segment.contains('%') { unquote(segment) } else { (*segment).to_owned() });
        }
        return (200, Some(body), String::new());
    }
    let mut allowed: Vec<&str> = methods.iter().map(|(method, _, _)| *method).collect();
    allowed.sort_unstable();
    (405, None, allowed.join(", "))
}

/// A path's percent-escapes decoded as `std.url.unquote` decodes them: `%` and two hex digits is
/// a byte, anything else is itself, and text that doesn't decode to UTF-8 is left as it was.
fn unquote(text: &str) -> String {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while let Some(&byte) = raw.get(at) {
        let digits = (raw.get(at + 1).and_then(|b| hex(*b)), raw.get(at + 2).and_then(|b| hex(*b)));
        if byte == b'%'
            && let (Some(high), Some(low)) = digits
        {
            out.push(high * 16 + low);
            at += 3;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).and_then(|digit| u8::try_from(digit).ok())
}

/// Text from the hex the reader printed, or `None` if it isn't hex of UTF-8.
fn from_hex(text: &str) -> Option<String> {
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|at| text.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()))
        .collect();
    String::from_utf8(bytes?).ok()
}

/// The target: each method on the input, taken as a request target, gets what the routes say.
pub(crate) fn router(input: &[u8]) -> Result<(), String> {
    let target = String::from_utf8_lossy(input).into_owned();
    let Some(printed) = crate::llvm::read_natively("router", target.as_bytes())? else { return Ok(()) };
    let printed = String::from_utf8_lossy(&printed).into_owned();
    let lines: Vec<&str> = printed.lines().collect();
    if lines.len() != METHODS.len() {
        return Err(format!("the reader printed something else:\n{printed}"));
    }
    let path = target.split_once('?').map_or(target.as_str(), |(path, _)| path);
    // An empty path is routed by the decoded one, which is empty too.
    let nodes = tree();
    for (method, line) in METHODS.iter().zip(&lines) {
        let parts: Vec<&str> = line.split(' ').collect();
        let [status, body, allowed] = parts.as_slice() else {
            return Err(format!("the reader printed something else: {line}"));
        };
        let (Some(body), Some(allowed)) =
            (body.strip_prefix('b').and_then(from_hex), allowed.strip_prefix('a').and_then(from_hex))
        else {
            return Err(format!("the reader printed something else: {line}"));
        };
        let (want_status, want_body, want_allowed) = answer(&nodes, method, path);
        let got_status = status.parse::<u16>().unwrap_or(0);
        if got_status != want_status
            || want_body.as_ref().is_some_and(|wanted| *wanted != body)
            || allowed != want_allowed
        {
            return Err(format!(
                "{method} {target:?}: answered {got_status} {body:?} (allow {allowed:?}), \
                 where the routes say {want_status} {want_body:?} (allow {want_allowed:?})"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{answer, tree, unquote};

    /// The walk here answers as `Router`'s documentation says: a segment written out before a
    /// parameter, one `/` at the end passed over, a 405 naming what a path takes, and `HEAD`
    /// answered by `GET`.
    #[test]
    fn the_walk_here_routes_as_the_router_says() {
        let nodes = tree();
        assert_eq!(answer(&nodes, "GET", "/"), (200, Some("root".to_owned()), String::new()));
        assert_eq!(answer(&nodes, "GET", "/notes/new"), (200, Some("form".to_owned()), String::new()));
        assert_eq!(answer(&nodes, "GET", "/notes/n%65w/"), (200, Some("note|new".to_owned()), String::new()));
        assert_eq!(answer(&nodes, "HEAD", "/notes/7"), (200, Some("note|7".to_owned()), String::new()));
        assert_eq!(answer(&nodes, "PUT", "/notes"), (405, None, "GET, POST".to_owned()));
        assert_eq!(answer(&nodes, "GET", "/notes//"), (404, None, String::new()));
        assert_eq!(answer(&nodes, "GET", "notes"), (404, None, String::new()));
        assert_eq!(answer(&nodes, "GET", "/notes/new/tags/x").0, 404);
        assert_eq!(unquote("%C3%a9%"), "é%");
        assert_eq!(unquote("%e9"), "%e9");
    }
}
