//! `fennec-fetch`, beside the launcher, does its network's work, started only when something has
//! to be fetched. Checking certificates against what the operating system trusts loads, on macOS,
//! the Security and CoreFoundation frameworks, which cost every program that links them
//! milliseconds to start; kept here, the launcher links none and starts as fast as a program can:
//!
//! ```text
//! fennec-fetch fetch <url> <most>   the bytes at the URL, at most so many, on its output
//! fennec-fetch redirect <url>       where the URL redirects to, without following it
//! ```
#![expect(clippy::print_stdout, reason = "where a redirect goes is said on its output")]
#![expect(clippy::print_stderr, reason = "and why a fetch failed on its errors")]

mod web;

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let done = match words.as_slice() {
        ["fetch", url, most] => most
            .parse()
            .map_err(|_| format!("`{most}` isn't a number of bytes"))
            .and_then(|most| web::fetch_at_most(url, most))
            .and_then(|bytes| {
                std::io::stdout()
                    .write_all(&bytes)
                    .map_err(|err| format!("couldn't hand on what it fetched: {err}"))
            }),
        ["redirect", url] => web::redirect(url).map(|to| println!("{to}")),
        _ => Err("`fennec-fetch` takes `fetch <url> <most>` or `redirect <url>`".to_owned()),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::FAILURE
        }
    }
}
