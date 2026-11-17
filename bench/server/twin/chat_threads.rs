//! A twin of `chat.fen` on std threads, beside the tokio one in `chat.rs`: a hub thread owns the
//! rooms, each connection has a thread reading it and a thread writing to it, and they talk over
//! channels of the same sizes as `chat.fen`'s.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use socket2::{Domain, Socket, Type};

/// What the hub hears from the connections.
enum Note {
    Joined { name: String, room: String, mailbox: SyncSender<String> },
    Said { room: String, line: String },
    Left { name: String, room: String },
}

/// Writes each line the hub sends one connection, until the hub lets go of its mailbox.
fn deliver(inbox: Receiver<String>, connection: Arc<TcpStream>) {
    let mut writer = &*connection;
    for line in inbox {
        if writer.write_all(line.as_bytes()).is_err() {
            return;
        }
    }
}

/// The hub: each room's members, by name, and each message sent on to all of them. A member that
/// has gone is passed over: its connection's own thread tells the hub it has left.
fn hub(inbox: Receiver<Note>) {
    let mut rooms: HashMap<String, HashMap<String, SyncSender<String>>> = HashMap::new();
    for note in inbox {
        match note {
            Note::Joined { name, room, mailbox } => {
                let _ = mailbox.send(format!("JOINED {room}\n"));
                rooms.entry(room).or_default().insert(name, mailbox);
            }
            Note::Said { room, line } => {
                for member in rooms.get(&room).into_iter().flat_map(HashMap::values) {
                    let _ = member.send(line.clone());
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
fn attend(connection: TcpStream, to_hub: SyncSender<Note>) {
    // Each line goes out as it's written, as Fennec's and Go's do: waiting to fill a packet held
    // a line back until the client's acknowledgement of the last, 40 ms on Linux.
    let _ = connection.set_nodelay(true);
    let connection = Arc::new(connection);
    let mut lines = BufReader::new(&*connection).lines();
    let Some(Ok(first)) = lines.next() else { return };
    let parts: Vec<&str> = first.split(' ').collect();
    let [kind, room, name] = parts.as_slice() else { return };
    if *kind != "JOIN" {
        return;
    }
    let (room, name) = (room.to_string(), name.to_string());
    let (mailbox, inbox) = sync_channel(64);
    let writer = {
        let connection = Arc::clone(&connection);
        std::thread::spawn(move || deliver(inbox, connection))
    };
    if to_hub.send(Note::Joined { name: name.clone(), room: room.clone(), mailbox }).is_err() {
        return;
    }
    for line in lines {
        let Ok(line) = line else { break };
        if to_hub.send(Note::Said { room: room.clone(), line: format!("{name} {line}\n") }).is_err() {
            return;
        }
    }
    let _ = to_hub.send(Note::Left { name, room });
    let _ = writer.join();
}

/// A listener with as long a queue of connections waiting to be taken as the system allows, as
/// `chat.fen` and the Go twin get.
fn listener() -> std::io::Result<TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())?;
    socket.listen(i32::MAX)?;
    Ok(socket.into())
}

fn main() -> std::io::Result<()> {
    let server = listener()?;
    println!("{}", server.local_addr()?.port());
    let (to_hub, inbox) = sync_channel(1024);
    std::thread::spawn(move || hub(inbox));
    for connection in server.incoming() {
        // A connection that failed before it was taken is passed over, as std's own example does.
        let Ok(connection) = connection else { continue };
        let to_hub = to_hub.clone();
        std::thread::spawn(move || attend(connection, to_hub));
    }
    Ok(())
}
