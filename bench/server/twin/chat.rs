//! The twin of `chat.fen` on tokio, as a Rust server's author would write it: a hub task owns the
//! rooms, each connection is split into a task reading it and a task writing to it, and they talk
//! over `tokio::sync::mpsc` channels of the same sizes as `chat.fen`'s. The runtime has a worker
//! for each core, as Fennec's pool and Go's scheduler do.

use std::collections::HashMap;
use std::net::SocketAddr;

use socket2::{Domain, Socket, Type};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{Receiver, Sender, channel};

/// What the hub hears from the connections.
enum Note {
    Joined { name: String, room: String, mailbox: Sender<String> },
    Said { room: String, line: String },
    Left { name: String, room: String },
}

/// Writes each line the hub sends one connection, until the hub lets go of its mailbox.
async fn deliver(mut inbox: Receiver<String>, mut writer: OwnedWriteHalf) {
    while let Some(line) = inbox.recv().await {
        if writer.write_all(line.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// The hub: each room's members, by name, and each message sent on to all of them. A member that
/// has gone is passed over: its connection's own task tells the hub it has left.
async fn hub(mut inbox: Receiver<Note>) {
    let mut rooms: HashMap<String, HashMap<String, Sender<String>>> = HashMap::new();
    while let Some(note) = inbox.recv().await {
        match note {
            Note::Joined { name, room, mailbox } => {
                let _ = mailbox.send(format!("JOINED {room}\n")).await;
                rooms.entry(room).or_default().insert(name, mailbox);
            }
            Note::Said { room, line } => {
                for member in rooms.get(&room).into_iter().flat_map(HashMap::values) {
                    let _ = member.send(line.clone()).await;
                }
            }
            Note::Left { name, room } => {
                if let Some(members) = rooms.get_mut(&room) {
                    members.remove(&name);
                }
            }
        }
    }
}

/// One connection: it joins its room, then hands the hub each line it sends, until it hangs up.
async fn attend(connection: TcpStream, to_hub: Sender<Note>) {
    // Each line goes out as it's written, as Fennec's and Go's do: waiting to fill a packet held
    // a line back until the client's acknowledgement of the last, 40 ms on Linux.
    let _ = connection.set_nodelay(true);
    let (reader, writer) = connection.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Ok(Some(first)) = lines.next_line().await else { return };
    let parts: Vec<&str> = first.split(' ').collect();
    let [kind, room, name] = parts.as_slice() else { return };
    if *kind != "JOIN" {
        return;
    }
    let (room, name) = (room.to_string(), name.to_string());
    let (mailbox, inbox) = channel(64);
    let writing = tokio::spawn(deliver(inbox, writer));
    if to_hub.send(Note::Joined { name: name.clone(), room: room.clone(), mailbox }).await.is_err() {
        return;
    }
    while let Ok(Some(line)) = lines.next_line().await {
        let said = Note::Said { room: room.clone(), line: format!("{name} {line}\n") };
        if to_hub.send(said).await.is_err() {
            return;
        }
    }
    let _ = to_hub.send(Note::Left { name, room }).await;
    let _ = writing.await;
}

/// A listener with as long a queue of connections waiting to be taken as the system allows, as
/// `chat.fen` and the Go twin get. tokio's own `bind` asks for 1,024.
fn listener() -> std::io::Result<std::net::TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
    socket.listen(i32::MAX)?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

/// Prints the port, then takes each connection as it comes.
async fn serve() -> std::io::Result<()> {
    let server = TcpListener::from_std(listener()?)?;
    println!("{}", server.local_addr()?.port());
    let (to_hub, inbox) = channel(1024);
    tokio::spawn(hub(inbox));
    loop {
        // A connection that failed before it was taken is passed over.
        let Ok((connection, _)) = server.accept().await else { continue };
        tokio::spawn(attend(connection, to_hub.clone()));
    }
}

fn main() -> std::io::Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_io().build()?.block_on(serve())
}
