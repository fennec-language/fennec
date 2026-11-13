//! A chat server's core, the twin of `chat_server.fen`: a hub thread owning the rooms in a
//! `HashMap`, fanning each message out over bounded channels to four shard threads, each owning
//! its users' mailboxes in a `HashMap`. Each thread gives back what it found when it's joined.

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

const USERS: i64 = 400;
const ROOMS: i64 = 40;
const HISTORY: usize = 50;
const KEPT: usize = 16;
const WORDS: [&str; 12] = ["hey", "anyone", "seen", "the", "new", "map", "lag", "again", "gg", "raid", "at", "nine"];

/// What a client sends, with the room by number.
enum Event {
    Join { user: i64, room: i64 },
    Leave { user: i64, room: i64 },
    Say { user: i64, room: i64, text: String },
    Whisper { user: i64, to: i64, text: String },
}

/// A message on its way to one user's mailbox.
struct Message {
    user: i64,
    room: String,
    author: i64,
    text: String,
}

/// One room, as the hub keeps it.
#[derive(Default)]
struct Room {
    members: Vec<i64>,
    history: Vec<String>,
    said: i64,
}

/// One user's mailbox, as a shard keeps it.
#[derive(Default)]
struct Mailbox {
    received: i64,
    bytes: i64,
    mentions: i64,
    recent: Vec<String>,
}

/// What a shard gives back about one of its users.
struct Summary {
    user: i64,
    received: i64,
    bytes: i64,
    mentions: i64,
    last: String,
}

/// What the hub gives back when every event has been handled.
struct HubReport {
    refused: i64,
    rooms: Vec<(i64, String)>,
    history: usize,
}

/// The next state of a small linear congruential generator, the same on every machine.
fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

/// A number from 0 up to `top` taken from a generator's state.
fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

/// Every client's event, in the order the server takes them.
fn events(seed: i64, count: i64) -> Vec<Event> {
    let mut state = seed;
    let mut made = Vec::new();
    for number in 0..count {
        state = next(state);
        let roll = below(state, 100);
        state = next(state);
        let user = below(state, USERS);
        state = next(state);
        let room = below(state, ROOMS);
        if roll < 14 {
            made.push(Event::Join { user, room });
        } else if roll < 18 {
            made.push(Event::Leave { user, room });
        } else {
            state = next(state);
            let first = below(state, 1_000);
            let count = 1 + below(state, 6);
            let mention = if roll % 10 == 0 { below(next(state), USERS) } else { -1 };
            if roll < 22 {
                let to = below(next(state), USERS);
                made.push(Event::Whisper { user, to, text: rendered(number, first, count, mention) });
            } else {
                made.push(Event::Say { user, room, text: rendered(number, first, count, mention) });
            }
        }
    }
    made
}

/// What a client said, as text.
fn rendered(number: i64, first: i64, count: i64, mention: i64) -> String {
    let mut words = Vec::new();
    for at in 0..count {
        words.push(WORDS[((first + at * 7) % WORDS.len() as i64) as usize].to_owned());
    }
    if mention >= 0 {
        words.push(format!("@u{mention}"));
    }
    format!("{number}: ") + &words.join(" ")
}

/// A hash of a line's bytes.
fn hashed(line: &str) -> i64 {
    let mut hash = line.len() as i64;
    for byte in line.bytes() {
        hash = (hash * 31 + byte as i64) % 1_000_000_007;
    }
    hash
}

/// The hub: owns the rooms, checks each event and fans each message out to the shards.
fn hub(inbox: Receiver<Event>, shards: Vec<SyncSender<Message>>) -> HubReport {
    let mut rooms: HashMap<String, Room> = HashMap::new();
    let mut refused = 0;
    for event in inbox {
        match event {
            Event::Join { user, room } => {
                let room = rooms.entry(format!("room-{room}")).or_default();
                if room.members.contains(&user) {
                    refused += 1;
                } else {
                    room.members.push(user);
                }
            }
            Event::Leave { user, room } => match rooms.get_mut(&format!("room-{room}")) {
                Some(room) if room.members.contains(&user) => room.members.retain(|&member| member != user),
                _ => refused += 1,
            },
            Event::Say { user, room, text } => {
                let name = format!("room-{room}");
                let Some(room) = rooms.get_mut(&name).filter(|room| room.members.contains(&user)) else {
                    refused += 1;
                    continue;
                };
                room.said += 1;
                room.history.push(format!("u{user}: {text}"));
                if room.history.len() > 2 * HISTORY {
                    room.history.drain(..HISTORY);
                }
                for &member in &room.members {
                    if member != user {
                        let message = Message { user: member, room: name.clone(), author: user, text: text.clone() };
                        shards[(member % shards.len() as i64) as usize].send(message).expect("the shard is running");
                    }
                }
            }
            Event::Whisper { user, to, text } => {
                let message = Message { user: to, room: "dm".to_owned(), author: user, text };
                shards[(to % shards.len() as i64) as usize].send(message).expect("the shard is running");
            }
        }
    }
    let mut busiest: Vec<(i64, String)> = rooms.iter().map(|(name, room)| (-room.said, name.clone())).collect();
    busiest.sort();
    busiest.truncate(5);
    let history = rooms.values().map(|room| room.history.len()).sum();
    HubReport { refused, rooms: busiest, history }
}

/// A shard: the mailboxes of the users it owns. Gives back what each got once its inbox closes.
fn shard(inbox: Receiver<Message>) -> Vec<Summary> {
    let mut boxes: HashMap<i64, Mailbox> = HashMap::new();
    for message in inbox {
        let line = format!("[{}] u{}: {}", message.room, message.author, message.text);
        let mailbox = boxes.entry(message.user).or_default();
        mailbox.received += 1;
        mailbox.bytes += line.len() as i64;
        if message.text.contains(&format!("@u{}", message.user)) {
            mailbox.mentions += 1;
        }
        mailbox.recent.push(line);
        if mailbox.recent.len() > 2 * KEPT {
            mailbox.recent.drain(..KEPT);
        }
    }
    boxes
        .into_iter()
        .map(|(user, mailbox)| Summary {
            user,
            received: mailbox.received,
            bytes: mailbox.bytes,
            mentions: mailbox.mentions,
            last: mailbox.recent.last().cloned().unwrap_or_default(),
        })
        .collect()
}

fn main() {
    let started = std::time::Instant::now();
    let sent = events(black_box(2026), black_box(20_000));
    let (mut everyone, report) = std::thread::scope(|scope| {
        let mut senders = Vec::new();
        let mut shards = Vec::new();
        for _ in 0..4 {
            let (sender, inbox) = sync_channel(256);
            senders.push(sender);
            shards.push(scope.spawn(move || shard(inbox)));
        }
        let (to_hub, inbox) = sync_channel(64);
        let hub = scope.spawn(move || hub(inbox, senders));
        for event in sent {
            to_hub.send(event).expect("the hub is running");
        }
        drop(to_hub);
        let report = hub.join().expect("the hub finished");
        let everyone: Vec<Summary> =
            shards.into_iter().flat_map(|shard| shard.join().expect("the shard finished")).collect();
        (everyone, report)
    });
    everyone.sort_by_key(|summary| summary.user);
    let mut received = 0;
    let mut bytes = 0;
    let mut mentions = 0;
    let mut checksum = 0;
    for summary in &everyone {
        received += summary.received;
        bytes += summary.bytes;
        mentions += summary.mentions;
        checksum = (checksum * 31 + summary.user * 7 + summary.received + hashed(&summary.last)) % 1_000_000_007;
    }
    println!("{} users {received} delivered {bytes} bytes {mentions} mentions", everyone.len());
    println!("{} refused {} kept in history", report.refused, report.history);
    for (said, name) in &report.rooms {
        println!("{name} {}", -said);
    }
    println!("{}", everyone[0].last);
    println!("{}", everyone[everyone.len() - 1].last);
    println!("checksum {checksum}");
    if std::env::args().len() > 1 {
        println!("kernel-ns {}", started.elapsed().as_nanos());
    }
}
