//! The twin of `echo.fen`: each connection a thread of its own.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

fn answer(connection: TcpStream) -> std::io::Result<()> {
    let mut writer = connection.try_clone()?;
    let mut lines = BufReader::new(connection);
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line)? == 0 {
            return Ok(());
        }
        writer.write_all(line.as_bytes())?;
    }
}

fn main() -> std::io::Result<()> {
    let server = TcpListener::bind("127.0.0.1:0")?;
    println!("{}", server.local_addr()?.port());
    for connection in server.incoming() {
        let connection = connection?;
        std::thread::spawn(move || answer(connection));
    }
    Ok(())
}
