//! The load for `http.fen` and its twin: `http_load PORT CONNECTIONS REQUESTS` opens that many
//! connections, each kept open, and sends each one's requests one after another, waiting for each
//! answer before the next, in turn: `GET /`, `GET /users/{n}` and `POST /orders` with a small JSON
//! order. Every answer's status and body are checked, and it prints how many it got and a checksum
//! of their bodies, the same for any server that answers right. It's a process of its own, so a
//! server's count of instructions holds only the server's work.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;

/// The `n`th request a connection sends, and the status and body it should get back.
fn request(n: u64) -> (String, u16, String) {
    match n % 3 {
        0 => ("GET / HTTP/1.1\r\nhost: localhost\r\n\r\n".to_string(), 200, "hello".to_string()),
        1 => (
            format!("GET /users/{n} HTTP/1.1\r\nhost: localhost\r\n\r\n"),
            200,
            format!("{{\"id\": {n}, \"name\": \"user {n}\", \"active\": {}}}", n.is_multiple_of(2)),
        ),
        _ => {
            let count = n % 7 + 1;
            let order = format!("{{\"item\":\"tea\",\"count\":{count}}}");
            (
                format!(
                    "POST /orders HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{order}",
                    order.len()
                ),
                201,
                format!("{{\"item\": \"tea\", \"count\": {count}, \"total\": {}}}", count * 250),
            )
        }
    }
}

/// A response's status and body.
fn response(reader: &mut BufReader<TcpStream>) -> io::Result<(u16, Vec<u8>)> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let status = line.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok((status, body))
}

fn visit(port: u16, first: u64, requests: u64) -> io::Result<u64> {
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut sum = 0_u64;
    for n in first..first + requests {
        let (sent, status, wanted) = request(n);
        writer.write_all(sent.as_bytes())?;
        let (got, body) = response(&mut reader)?;
        assert_eq!((got, body.as_slice()), (status, wanted.as_bytes()), "request {n}");
        sum = sum.wrapping_mul(31).wrapping_add(body.iter().map(|&b| u64::from(b)).sum::<u64>());
    }
    Ok(sum)
}

fn main() {
    let args: Vec<u64> = std::env::args().skip(1).map(|a| a.parse().expect("a number")).collect();
    let [port, connections, requests] = args[..] else {
        panic!("usage: http_load PORT CONNECTIONS REQUESTS");
    };
    let port = u16::try_from(port).expect("a port");
    let visits: Vec<_> = (0..connections)
        .map(|c| thread::spawn(move || visit(port, c * requests, requests).expect("answered")))
        .collect();
    let sums: Vec<u64> = visits.into_iter().map(|v| v.join().expect("a visit")).collect();
    let sum = sums.iter().fold(0_u64, |all, s| all.wrapping_mul(1_000_003).wrapping_add(*s));
    println!("{} answered, checksum {sum}", connections * requests);
}
