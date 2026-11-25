//! A failure stays fixed: every input the fuzzer has kept runs clean. A short fixed run of each
//! target checks the fuzzer itself still works, and that nothing it would soon find has crept in.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};

/// Whether a file beside the inputs is an output pinned for one.
fn pinned_output(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension == "out" || extension == "fails")
}

fn fuzz() -> Command {
    Command::new(env!("CARGO_BIN_EXE_fennec-fuzz"))
}

/// How a `fennec-fuzz run` ended, for a failing test to say: its exit code, or the signal that
/// stopped it from outside, the target it was in then, and all it wrote, standard error too.
fn ended(output: &std::process::Output) -> String {
    let printed = String::from_utf8_lossy(&output.stdout);
    let said = String::from_utf8_lossy(&output.stderr);
    // Each target says it's starting, and then what it ran: the last to start with nothing
    // after it is the one the run ended in.
    let last = printed.lines().rev().find_map(|line| line.strip_suffix(": starting"));
    let finished = |target: &str| {
        printed.lines().any(|line| line.starts_with(&format!("{target}: ")) && line.contains(" inputs, "))
    };
    let when = match last {
        Some(target) if !finished(target) => format!("while {target} was running"),
        Some(target) => format!("after {target} finished"),
        None => "before any target started".to_owned(),
    };
    format!("`fennec-fuzz run` ended with {} {when}\n--- printed\n{printed}--- said\n{said}", output.status)
}

#[test]
fn every_kept_input_runs_clean() {
    let found = Path::new(env!("CARGO_MANIFEST_DIR")).join("found");
    for folder in std::fs::read_dir(found).unwrap() {
        let folder = folder.unwrap().path();
        if !folder.is_dir() {
            continue;
        }
        let target = folder.file_name().unwrap().to_string_lossy().into_owned();
        for input in std::fs::read_dir(&folder).unwrap() {
            let input = input.unwrap().path();
            if pinned_output(&input) {
                continue;
            }
            let output = fuzz().args(["one", &target]).arg(&input).output().unwrap();
            assert!(
                output.status.success(),
                "{}: {}\n{}{}",
                input.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
}

/// Every target runs the same hundred inputs, however busy the machine is: the run has no time
/// limit, and an input counts as stuck only after two minutes of CPU time, far more than any takes.
#[test]
fn a_short_run_of_every_target_finds_nothing() {
    let dir = std::env::temp_dir().join(format!("fennec-fuzz-short-{}", std::process::id()));
    let output = fuzz()
        .args(["run", "--seed", "1", "--inputs", "100", "--stuck", "120", "--found"])
        .arg(&dir)
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{}", ended(&output));
    let targets = [
        "lexer",
        "parser",
        "formatter",
        "checker",
        "lengths",
        "ranges",
        "csv",
        "json",
        "json-rules",
        "pack",
        "toml",
        "toml-rules",
        "http-client",
        "http-client-scripts",
        "http-client-splits",
        "http-server",
        "http-sessions",
        "http-splits",
        "router",
        "html",
        "postgres",
        "regex",
        "url",
        "runtime",
        "utf8",
        "numbers",
        "time",
        "base64",
    ];
    // Those that run natively are left out, saying so, where nothing can be built.
    let native = [
        "base64",
        "csv",
        "html",
        "http-client",
        "http-client-scripts",
        "http-client-splits",
        "http-server",
        "http-sessions",
        "http-splits",
        "json",
        "json-rules",
        "numbers",
        "pack",
        "postgres",
        "router",
        "time",
        "toml",
        "toml-rules",
        "url",
        "utf8",
    ];
    let left_out = printed.contains(
        "base64, csv, html, http-client, http-client-scripts, http-client-splits, http-server, http-sessions, http-splits, \
         json, json-rules, numbers, pack, postgres, router, time, toml, toml-rules, url, utf8: left out",
    );
    for target in targets {
        if left_out && native.contains(&target) {
            continue;
        }
        assert!(printed.contains(&format!("{target}: 100 inputs, 0 failures")), "{target}: {printed}");
    }
}

/// Large inputs a stranger could send, each through the target that reads it, all run clean. A
/// reader takes time and memory in proportion to what it's given, never that squared, and never
/// what a length in it claims. `fennec-fuzz one` stops a reader past 10 seconds of CPU time or
/// 512 MB, so one gone quadratic fails here rather than in a server.
///
/// Fuzzing's own inputs are a few hundred bytes, too small for that to show. These found that
/// `json-rules` looked for each key among every key before it, the target's own mistake. Lengths
/// that lie, past what any integer holds or below zero, lines and headers past each limit, text
/// that isn't UTF-8 and offsets that take a time past the years it holds are among them too.
/// Where nothing can be built natively, each passes without running.
#[test]
fn large_hostile_inputs_run_clean() {
    // A packed `list[int]`'s header, up to where the count of its items starts, from its seed.
    let ints = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/pack/ints.bin")).unwrap();
    let inputs = [large_data(ints.get(..16).unwrap_or_default()), large_messages(), large_text()].concat();
    let dir = std::env::temp_dir().join(format!("fennec-fuzz-large-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (at, (target, what, input)) in inputs.iter().enumerate() {
        let file = dir.join(format!("{at}"));
        std::fs::write(&file, input).unwrap();
        let output = fuzz().args(["one", target]).arg(&file).output().unwrap();
        assert!(
            output.status.success(),
            "{target}, {what}: {}\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every request the `http-server` target starts from or has kept, and each large hostile one, is
/// answered the same split at every byte (`http-splits`): a head cut across two reads, or two in
/// one, gets what the whole of it gets.
#[test]
fn every_http_seed_and_kept_input_is_heard_the_same_split_anywhere() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for folder in [here.join("seeds/http"), here.join("found/http-server")] {
        for input in std::fs::read_dir(&folder).unwrap() {
            let input = input.unwrap().path();
            if !pinned_output(&input) {
                files.push((input.display().to_string(), input));
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("fennec-fuzz-splits-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (at, (target, what, input)) in large_messages().into_iter().enumerate() {
        if target == "http-server" {
            let file = dir.join(format!("{at}"));
            std::fs::write(&file, input).unwrap();
            files.push((what.to_owned(), file));
        }
    }
    for (what, file) in &files {
        let output = fuzz().args(["one", "http-splits"]).arg(file).output().unwrap();
        assert!(
            output.status.success(),
            "{what}: {}\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every script the `http-client-scripts` target starts from or has kept is read the same with each
/// server's answer split at every byte (`http-client-splits`): a status line, a header, a length, a
/// chunk or a body cut across two reads gets what the whole of it gets.
#[test]
fn every_client_seed_and_kept_input_reads_the_same_split_anywhere() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    for folder in [here.join("seeds/http-client-scripts"), here.join("found/http-client-scripts")] {
        for input in std::fs::read_dir(&folder).unwrap() {
            let input = input.unwrap().path();
            if pinned_output(&input) {
                continue;
            }
            let output = fuzz().args(["one", "http-client-splits"]).arg(&input).output().unwrap();
            assert!(
                output.status.success(),
                "{}: {}\n{}{}",
                input.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
    }
}

/// Large inputs for the readers of data: JSON, TOML, `std.pack` after the `header` of a packed
/// list, routes, HTML, URLs and CSV.
fn large_data(header: &[u8]) -> Vec<(&'static str, &'static str, Vec<u8>)> {
    let varint = |mut n: u64| {
        let mut out = Vec::new();
        while n >= 0x80 {
            out.push(u8::try_from(n & 0x7f).unwrap_or_default() | 0x80);
            n >>= 7;
        }
        out.push(u8::try_from(n).unwrap_or_default());
        out
    };
    let keys: String = (0..200_000).map(|n| format!("\"k{n}\":\"v\"")).collect::<Vec<_>>().join(",");
    vec![
        ("json", "100,000 arrays open", "[".repeat(100_000).into_bytes()),
        ("json", "a million digits", "1".repeat(1_000_000).into_bytes()),
        ("json", "a 5 MB string", format!("\"{}\"", "a".repeat(5_000_000)).into_bytes()),
        ("json-rules", "200,000 keys", format!("{{{keys}}}").into_bytes()),
        ("json-rules", "100,000 objects open", "{\"a\":".repeat(100_000).into_bytes()),
        ("toml", "100,000 arrays open", format!("a = {}", "[".repeat(100_000)).into_bytes()),
        ("toml-rules", "a key of 100,000 parts", format!("a{} = 1", ".a".repeat(100_000)).into_bytes()),
        ("pack", "a count of 2^63", [header, &varint(u64::MAX >> 1)].concat()),
        ("pack", "a count of 10 million, and no items", [header, &varint(10_000_000)].concat()),
        ("router", "a million slashes", "/".repeat(1_000_000).into_bytes()),
        ("router", "a million escapes", format!("/notes/{}", "%41".repeat(1_000_000)).into_bytes()),
        ("html", "a million `<`", "<".repeat(1_000_000).into_bytes()),
        (
            "html",
            "a link after a million spaces",
            format!("{}javascript:x", " ".repeat(1_000_000)).into_bytes(),
        ),
        ("url", "a million characters", format!("http://a/{}", "a".repeat(1_000_000)).into_bytes()),
        ("url", "a million escapes", "%41".repeat(1_000_000).into_bytes()),
        ("url", "a port of a million digits", format!("http://a:{}/", "9".repeat(1_000_000)).into_bytes()),
        ("csv", "a million quotes", "\"".repeat(1_000_000).into_bytes()),
        ("csv", "a 5 MB field", format!("\"{}\"", "a".repeat(5_000_000)).into_bytes()),
        ("csv", "a million rows", "a,b\n".repeat(1_000_000).into_bytes()),
        ("toml", "a 5 MB string", format!("a = \"{}\"", "x".repeat(5_000_000)).into_bytes()),
        (
            "toml-rules",
            "100,000 tables",
            (0..100_000).map(|n| format!("[t{n}]\nk = 1\n")).collect::<Vec<_>>().concat().into_bytes(),
        ),
        ("toml-rules", "100,000 inline tables open", format!("a = {}", "{b = ".repeat(100_000)).into_bytes()),
        ("json-rules", "a million items", format!("[{}1]", "1,".repeat(999_999)).into_bytes()),
    ]
}

/// Large inputs for the readers of what comes over a connection: HTTP both ways and Postgres.
fn large_messages() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    vec![
        ("http-server", "a million blank lines", "\r\n".repeat(1_000_000).into_bytes()),
        (
            "http-server",
            "10,000 headers",
            format!("GET / HTTP/1.1\r\n{}\r\n", "x-a: b\r\n".repeat(10_000)).into_bytes(),
        ),
        ("http-server", "20,000 requests in a row", "GET / HTTP/1.1\r\n\r\n".repeat(20_000).into_bytes()),
        (
            "http-server",
            "a header line of a million bytes",
            format!("GET / HTTP/1.1\r\nx-a: {}\r\n\r\n", "b".repeat(1_000_000)).into_bytes(),
        ),
        (
            "http-server",
            "a request line of a million bytes",
            format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(1_000_000)).into_bytes(),
        ),
        (
            "http-server",
            "a body said to be 2^63 bytes",
            b"POST / HTTP/1.1\r\nContent-Length: 9223372036854775807\r\n\r\nab".to_vec(),
        ),
        (
            "http-server",
            "a body said to be 2^64 bytes",
            b"POST / HTTP/1.1\r\nContent-Length: 18446744073709551616\r\n\r\nab".to_vec(),
        ),
        ("http-server", "a negative length", b"POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\nab".to_vec()),
        (
            "http-server",
            "two lengths",
            b"POST / HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nabc".to_vec(),
        ),
        (
            "http-server",
            "a chunk said to be 2^72 bytes",
            b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffff\r\nab".to_vec(),
        ),
        ("http-server", "a header that isn't UTF-8", b"GET / HTTP/1.1\r\nx-a: \xff\xfe\r\n\r\n".to_vec()),
        ("http-server", "a path that isn't UTF-8", b"GET /\xff\xfe HTTP/1.1\r\n\r\n".to_vec()),
        (
            "http-client",
            "a status line of a million bytes",
            format!("HTTP/1.1 200 {}\r\n\r\n", "a".repeat(1_000_000)).into_bytes(),
        ),
        (
            "http-client",
            "a million headers",
            format!("HTTP/1.1 200 OK\r\n{}\r\n", "x-a: b\r\n".repeat(1_000_000)).into_bytes(),
        ),
        (
            "http-client",
            "a header line of a million bytes",
            format!("HTTP/1.1 200 OK\r\nx-a: {}\r\n\r\n", "b".repeat(1_000_000)).into_bytes(),
        ),
        (
            "http-client",
            "a body said to be 2^63 bytes",
            b"HTTP/1.1 200 OK\r\nContent-Length: 9223372036854775807\r\n\r\nab".to_vec(),
        ),
        (
            "http-client",
            "a chunk said to be 2^72 bytes",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffffff\r\nab".to_vec(),
        ),
        ("postgres", "a message said to be 2 GB", b"R\x7f\xff\xff\xff".to_vec()),
        ("postgres", "a message said to be -1 bytes", b"R\xff\xff\xff\xff".to_vec()),
    ]
}

/// Large inputs for the readers of short text: numbers, times and base64.
fn large_text() -> Vec<(&'static str, &'static str, Vec<u8>)> {
    vec![
        ("numbers", "a million digits", "1".repeat(1_000_000).into_bytes()),
        ("numbers", "a float of a million digits", format!("0.{}", "1".repeat(1_000_000)).into_bytes()),
        ("numbers", "an exponent of a million digits", format!("1e{}", "9".repeat(1_000_000)).into_bytes()),
        ("numbers", "a million digits that aren't ASCII", "\u{663}".repeat(1_000_000).into_bytes()),
        (
            "time",
            "a fraction of a million digits",
            format!("2026-11-06T21:55:56.{}Z", "1".repeat(1_000_000)).into_bytes(),
        ),
        ("time", "a million characters that aren't ASCII", "é".repeat(1_000_000).into_bytes()),
        ("time", "an offset past the first year", b"1678-01-01T00:00:00+23:59".to_vec()),
        ("time", "an offset past the last year", b"2261-12-31T23:59:59.999999999-23:59".to_vec()),
        ("base64", "5 MB", "QUJD".repeat(1_250_000).into_bytes()),
        ("base64", "a million `=`", "=".repeat(1_000_000).into_bytes()),
    ]
}

/// Each program the `llvm` target starts from builds both ways and prints what's pinned for it,
/// where LLVM's tools are here.
#[test]
fn every_program_to_start_from_prints_what_is_pinned() {
    let seeds = Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds/llvm");
    for input in std::fs::read_dir(seeds).unwrap() {
        let input = input.unwrap().path();
        if pinned_output(&input) {
            continue;
        }
        assert!(input.with_extension("out").exists(), "{} has no output pinned", input.display());
        let output = fuzz().args(["one", "llvm"]).arg(&input).output().unwrap();
        assert!(
            output.status.success(),
            "{}: {}\n{}",
            input.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

/// A run stopped from outside says so, with how it ended and the target it was in, so a failure of
/// the test above shows whether the run crashed or was killed. Killed, it ends by SIGKILL on Unix,
/// and on Windows, which has no signals, with the exit code `TerminateProcess` gives it, 1.
#[test]
fn a_run_stopped_from_outside_says_where() {
    let mut child = fuzz()
        .args(["run", "--seed", "1", "--inputs", "300", "lexer"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut printed = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    printed.read_line(&mut first).unwrap();
    assert_eq!(first, "lexer: starting\n");
    child.kill().unwrap();
    let mut rest = Vec::new();
    printed.read_to_end(&mut rest).unwrap();
    let mut said = Vec::new();
    child.stderr.take().unwrap().read_to_end(&mut said).unwrap();
    let status = child.wait().unwrap();
    let stdout = [first.into_bytes(), rest].concat();
    let shown = ended(&std::process::Output { status, stdout, stderr: said });
    #[cfg(unix)]
    assert!(shown.contains("signal: 9 (SIGKILL)"), "{shown}");
    #[cfg(windows)]
    assert!(shown.contains("exit code: 1"), "{shown}");
    assert!(shown.contains("while lexer was running") || shown.contains("after lexer finished"), "{shown}");
}

#[test]
fn a_target_that_isnt_there_is_an_error() {
    let output = fuzz().args(["run", "no such target"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}
