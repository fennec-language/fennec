//! Messages as they travel between editor and server: a `Content-Length` header, a blank
//! line, then that many bytes of JSON.

use std::io::{self, BufRead, Write};

use crate::json::Json;

/// The most a message may hold, so a broken header can't make the server read forever.
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Reads the next message. None at the end of the input. A message that isn't JSON reads as
/// `null`, which the server ignores.
pub fn read(input: &mut dyn BufRead) -> io::Result<Option<Json>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.unwrap_or(0);
    if length > MAX_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a message too big to read"));
    }
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    Ok(Some(String::from_utf8(body).ok().and_then(|text| Json::parse(&text)).unwrap_or(Json::Null)))
}

/// Writes one message.
pub fn write(output: &mut dyn Write, message: &Json) -> io::Result<()> {
    let body = message.to_string();
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}
