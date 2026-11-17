//! The twin of `echo.fen`: each connection a thread of its own.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

use socket2::{Domain, Socket, Type};

fn answer(connection: TcpStream) -> std::io::Result<()> {
    // Read and written through the one socket, as `echo.fen`'s connections are.
    let mut writer = &connection;
    let mut lines = BufReader::new(&connection);
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line)? == 0 {
            return Ok(());
        }
        writer.write_all(line.as_bytes())?;
    }
}

/// A listener with as long a queue of connections waiting to be taken as the system allows, as
/// `echo.fen` and the Go twin get. std's `bind` asks for 128, so on Linux 900 clients connecting
/// at once overflowed it, and the kernel reset the connections whose handshakes it had dropped.
fn listener() -> std::io::Result<TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
    socket.listen(i32::MAX)?;
    Ok(socket.into())
}

fn main() -> std::io::Result<()> {
    let server = listener()?;
    println!("{}", server.local_addr()?.port());
    for connection in server.incoming() {
        // A connection that failed before it was taken is passed over, as std's own example for
        // `incoming` does, rather than stopping the server and every connection it holds.
        let Ok(connection) = connection else { continue };
        std::thread::spawn(move || answer(connection));
    }
    Ok(())
}
