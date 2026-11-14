//! The game server as a Rust program writes it, with the crates one would use: a thread for each
//! connection over `std::net`, `std::sync::mpsc` channels between threads, the sessions in a
//! `Mutex`, rayon for the tick's work over the monsters, serde_json for the messages, and rusqlite
//! for the snapshots. It does what `game_server.fen` does, step for step, and prints the same
//! summary. JSON is written with a space after `:` and `,`, as `json.dumps` does, so both sides
//! send the same bytes.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use rayon::prelude::*;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::ser::{Formatter, Serializer};

const PLAYERS: i64 = 48;
const TICKS: i64 = 240;
const MONSTERS: i64 = 3_000;
const SIZE: i64 = 1_024;
const CELL: i64 = 128;
const SIDE: i64 = SIZE / CELL;
const SPEED: i64 = 6;
const REACH: i64 = 24;
const SIGHT: i64 = 120;
const HEARING: i64 = 200;
const SAVE_EVERY: i64 = 30;
const WORDS: [&str; 12] =
    ["hello", "anyone", "near", "the", "old", "tower", "need", "help", "gg", "loot", "here", "run"];

/// Writes `, ` and `: `, the way `json.dumps` does.
struct Spaced;

impl Formatter for Spaced {
    fn begin_array_value<W: ?Sized + Write>(&mut self, out: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { out.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, out: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { out.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, out: &mut W) -> io::Result<()> {
        out.write_all(b": ")
    }
}

fn dumps<T: Serialize>(value: &T) -> String {
    let mut out = Vec::new();
    value.serialize(&mut Serializer::with_formatter(&mut out, Spaced)).expect("it writes");
    String::from_utf8(out).expect("JSON is text")
}

fn clamped(at: i64) -> i64 {
    at.clamp(0, SIZE - 1)
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Spot {
    x: i64,
    y: i64,
}

impl Spot {
    fn squared_distance(self, other: Spot) -> i64 {
        let across = self.x - other.x;
        let down = self.y - other.y;
        across * across + down * down
    }

    fn near(self, other: Spot, reach: i64) -> bool {
        self.squared_distance(other) <= reach * reach
    }

    fn cell(self) -> usize {
        (self.y / CELL * SIDE + self.x / CELL) as usize
    }

    fn moved(self, east: i64, south: i64) -> Spot {
        Spot { x: clamped(self.x + east), y: clamped(self.y + south) }
    }

    fn toward(self, other: Spot, most: i64) -> Spot {
        let east = (other.x - self.x).clamp(-most, most);
        let south = (other.y - self.y).clamp(-most, most);
        Spot { x: self.x + east, y: self.y + south }
    }
}

#[derive(Serialize, Deserialize)]
enum Ask {
    Join { name: String },
    Act { east: i64, south: i64, say: String },
    Who,
    Leave,
}

#[derive(Clone, Serialize, Deserialize)]
struct View {
    tick: i64,
    spot: Spot,
    health: i64,
    score: i64,
    monsters: i64,
    heard: Vec<String>,
}

#[derive(Serialize, Deserialize)]
enum Tell {
    Welcome { spot: Spot },
    Saw { view: View },
    Online { names: Vec<String> },
    Bye { score: i64, last: bool },
}

#[derive(Clone, Default)]
struct Input {
    east: i64,
    south: i64,
    say: String,
    leaving: bool,
}

struct Session {
    pending: Option<Input>,
    score: i64,
}

/// Every player's session, by name, in order.
struct Lobby {
    sessions: BTreeMap<String, Session>,
    open: bool,
    dropped: Vec<String>,
}

impl Lobby {
    fn join(&mut self, name: &str) -> bool {
        if !self.open || self.sessions.contains_key(name) {
            return false;
        }
        self.sessions.insert(name.to_owned(), Session { pending: None, score: 0 });
        true
    }

    fn ask(&mut self, name: &str, input: Input) {
        self.sessions.get_mut(name).expect("joined").pending = Some(input);
    }

    fn take_inputs(&mut self) -> Vec<(String, Input)> {
        self.sessions
            .iter_mut()
            .map(|(name, session)| (name.clone(), session.pending.take().unwrap_or_default()))
            .collect()
    }

    fn online(&self) -> Vec<String> {
        self.sessions.iter().map(|(name, session)| format!("{name} {}", session.score)).collect()
    }
}

enum Note {
    Joined(String, SyncSender<Tell>),
    Ready,
}

#[derive(Clone)]
struct Avatar {
    name: String,
    spot: Spot,
    home: Spot,
    health: i64,
    score: i64,
    kills: i64,
    deaths: i64,
}

#[derive(Clone, Copy)]
struct Monster {
    number: i64,
    spot: Spot,
    health: i64,
    down: i64,
}

struct Thought {
    monster: Monster,
    target: i64,
    damage: i64,
    hurt: i64,
    first_hitter: i64,
}

impl Thought {
    fn of(monster: Monster) -> Thought {
        Thought { monster, target: -1, damage: 0, hurt: 0, first_hitter: -1 }
    }
}

struct World {
    avatars: Vec<Avatar>,
    monsters: Vec<Monster>,
    turn: i64,
}

#[derive(Serialize)]
struct Saved {
    name: String,
    spot: Spot,
    health: i64,
    score: i64,
    kills: i64,
    deaths: i64,
}

struct Save {
    turn: i64,
    avatars: Vec<Saved>,
    up: i64,
}

fn next(seed: i64) -> i64 {
    (seed * 1_103_515_245 + 12_345) % 2_147_483_648
}

fn below(seed: i64, top: i64) -> i64 {
    seed / 65_536 % top
}

fn started(names: &[String]) -> World {
    let avatars = names
        .iter()
        .enumerate()
        .map(|(at, name)| {
            let at = at as i64;
            let home = Spot { x: (at * 7_919 + 101) % SIZE, y: (at * 104_729 + 37) % SIZE };
            Avatar { name: name.clone(), spot: home, home, health: 100, score: 0, kills: 0, deaths: 0 }
        })
        .collect();
    let mut monsters = Vec::new();
    let mut state = 2026;
    for number in 0..MONSTERS {
        state = next(state);
        let x = below(state, SIZE);
        state = next(state);
        monsters.push(Monster { number, spot: Spot { x, y: below(state, SIZE) }, health: 60, down: 0 });
    }
    World { avatars, monsters, turn: 0 }
}

fn nearby_avatars(avatars: &[Avatar]) -> Vec<Vec<i64>> {
    let mut nearby = vec![Vec::new(); (SIDE * SIDE) as usize];
    for (at, avatar) in avatars.iter().enumerate() {
        let column = avatar.spot.x / CELL;
        let row = avatar.spot.y / CELL;
        for around in (row - 1).max(0)..(row + 2).min(SIDE) {
            for beside in (column - 1).max(0)..(column + 2).min(SIDE) {
                nearby[(around * SIDE + beside) as usize].push(at as i64);
            }
        }
    }
    nearby
}

fn think(monster: Monster, avatars: &[Avatar], nearby: &[Vec<i64>], turn: i64) -> Thought {
    if monster.down > 1 {
        return Thought::of(Monster { down: monster.down - 1, ..monster });
    }
    if monster.down == 1 {
        let seed = next(monster.number * 31 + turn);
        let spot = Spot { x: below(seed, SIZE), y: below(next(seed), SIZE) };
        return Thought::of(Monster { number: monster.number, spot, health: 60, down: 0 });
    }
    let mut target = -1;
    let mut best = SIGHT * SIGHT + 1;
    let mut hurt = 0;
    let mut first_hitter = -1;
    for &at in &nearby[monster.spot.cell()] {
        let distance = monster.spot.squared_distance(avatars[at as usize].spot);
        if distance < best {
            best = distance;
            target = at;
        }
        if distance <= REACH * REACH {
            hurt += 8 + at % 5;
            if first_hitter < 0 {
                first_hitter = at;
            }
        }
    }
    if target < 0 {
        let seed = next(monster.number * 131 + turn / 16);
        let spot = monster.spot.moved(below(seed, 3) - 1, below(next(seed), 3) - 1);
        return Thought { hurt, first_hitter, ..Thought::of(Monster { spot, ..monster }) };
    }
    let goal = avatars[target as usize].spot;
    let spot = monster.spot.toward(goal, 3);
    let moved = Monster { spot, ..monster };
    if !spot.near(goal, REACH) {
        return Thought { hurt, first_hitter, ..Thought::of(moved) };
    }
    Thought { monster: moved, target, damage: 2 + monster.number % 3, hurt, first_hitter }
}

fn struck(avatar: &mut Avatar, damage: i64) {
    avatar.health -= damage;
    if avatar.health > 0 {
        return;
    }
    avatar.deaths += 1;
    avatar.score = (avatar.score - 5).max(0);
    avatar.health = 100;
    avatar.spot = avatar.home;
}

fn settle(world: &mut World, at: usize, thought: &Thought) {
    if world.monsters[at].down > 0 {
        world.monsters[at] = thought.monster;
        return;
    }
    let mut monster = thought.monster;
    if thought.hurt > 0 {
        monster.health -= thought.hurt;
        if monster.health <= 0 {
            monster.down = 25;
            let hitter = &mut world.avatars[thought.first_hitter as usize];
            hitter.kills += 1;
            hitter.score += 10;
        }
    }
    world.monsters[at] = monster;
    if thought.target >= 0 {
        struck(&mut world.avatars[thought.target as usize], thought.damage);
    }
}

fn seen(avatar: &Avatar, monsters: &[Monster]) -> i64 {
    monsters.iter().filter(|monster| monster.down == 0 && avatar.spot.near(monster.spot, SIGHT)).count()
        as i64
}

fn heard_by(avatars: &[Avatar], at: usize, said: &[(usize, String)]) -> Vec<String> {
    said.iter()
        .filter(|(speaker, _)| *speaker != at && avatars[at].spot.near(avatars[*speaker].spot, HEARING))
        .map(|(speaker, text)| format!("{}: {text}", avatars[*speaker].name))
        .collect()
}

fn tick(world: &mut World, inputs: &[Input]) -> Vec<View> {
    world.turn += 1;
    let mut said = Vec::new();
    for (at, input) in inputs.iter().enumerate() {
        let avatar = &mut world.avatars[at];
        avatar.spot = avatar.spot.moved(input.east * SPEED, input.south * SPEED);
        if !input.say.is_empty() {
            said.push((at, input.say.clone()));
        }
    }
    let nearby = nearby_avatars(&world.avatars);
    let turn = world.turn;
    let thoughts: Vec<Thought> =
        world.monsters.par_iter().map(|monster| think(*monster, &world.avatars, &nearby, turn)).collect();
    for (at, thought) in thoughts.iter().enumerate() {
        settle(world, at, thought);
    }
    for avatar in &mut world.avatars {
        avatar.health = (avatar.health + 1).min(100);
    }
    let counts: Vec<i64> = world.avatars.par_iter().map(|avatar| seen(avatar, &world.monsters)).collect();
    world
        .avatars
        .iter()
        .enumerate()
        .map(|(at, avatar)| View {
            tick: world.turn,
            spot: avatar.spot,
            health: avatar.health,
            score: avatar.score,
            monsters: counts[at],
            heard: heard_by(&world.avatars, at, &said),
        })
        .collect()
}

fn saved(world: &World) -> Save {
    let avatars = world
        .avatars
        .iter()
        .map(|avatar| Saved {
            name: avatar.name.clone(),
            spot: avatar.spot,
            health: avatar.health,
            score: avatar.score,
            kills: avatar.kills,
            deaths: avatar.deaths,
        })
        .collect();
    let up = world.monsters.iter().filter(|monster| monster.down == 0).count() as i64;
    Save { turn: world.turn, avatars, up }
}

fn ticker(inbox: Receiver<Note>, lobby: &Mutex<Lobby>, saves: SyncSender<Save>, party: usize) {
    let mut replies: HashMap<String, SyncSender<Tell>> = HashMap::new();
    let mut world = World { avatars: Vec::new(), monsters: Vec::new(), turn: 0 };
    let mut ready = 0;
    for note in inbox {
        match note {
            Note::Joined(name, to_player) => {
                replies.insert(name, to_player);
                if replies.len() == party {
                    lobby.lock().expect("not poisoned").open = false;
                    let mut names: Vec<String> = replies.keys().cloned().collect();
                    names.sort();
                    world = started(&names);
                    for avatar in &world.avatars {
                        let _ = replies[&avatar.name].send(Tell::Welcome { spot: avatar.spot });
                    }
                }
            }
            Note::Ready => ready += 1,
        }
        if world.avatars.is_empty() || ready < world.avatars.len() {
            continue;
        }
        ready = 0;
        let taken = lobby.lock().expect("not poisoned").take_inputs();
        let inputs: Vec<Input> = taken.iter().map(|(_, input)| input.clone()).collect();
        let views = tick(&mut world, &inputs);
        {
            let mut current = lobby.lock().expect("not poisoned");
            for avatar in &world.avatars {
                current.sessions.get_mut(&avatar.name).expect("joined").score = avatar.score;
            }
            for (name, input) in &taken {
                if input.leaving {
                    current.sessions.remove(name);
                }
            }
        }
        let mut staying = Vec::new();
        let mut leaving = Vec::new();
        for (at, avatar) in world.avatars.iter().enumerate() {
            if taken[at].1.leaving {
                leaving.push(avatar.clone());
            } else {
                staying.push(avatar.clone());
                let _ = replies[&avatar.name].send(Tell::Saw { view: views[at].clone() });
            }
        }
        if world.turn % SAVE_EVERY == 0 || staying.is_empty() {
            saves.send(saved(&world)).expect("the saver is there");
        }
        world.avatars = staying;
        for avatar in leaving {
            let to_player = replies.remove(&avatar.name).expect("playing");
            let _ = to_player.send(Tell::Bye { score: avatar.score, last: replies.is_empty() });
        }
        if replies.is_empty() {
            return;
        }
    }
}

fn saver(inbox: Receiver<Save>, db: &Mutex<Connection>) -> rusqlite::Result<()> {
    for save in inbox {
        let body = dumps(&save.avatars);
        let mut db = db.lock().expect("not poisoned");
        let tx = db.transaction()?;
        tx.execute(
            "insert into snapshots (tick, players, up, body) values (?1, ?2, ?3, ?4)",
            params![save.turn, save.avatars.len() as i64, save.up, body],
        )?;
        for avatar in &save.avatars {
            tx.execute(
                "insert into players (name, x, y, health, score, kills, deaths, tick) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) on conflict (name) do update set x = excluded.x, y = excluded.y, health = excluded.health, score = excluded.score, kills = excluded.kills, deaths = excluded.deaths, tick = excluded.tick",
                params![
                    avatar.name,
                    avatar.spot.x,
                    avatar.spot.y,
                    avatar.health,
                    avatar.score,
                    avatar.kills,
                    avatar.deaths,
                    save.turn
                ],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

/// A failure of a connection: the network's, or a message that isn't one.
enum Failed {
    Net(io::Error),
    Parse(serde_json::Error),
    Gone,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failed::Net(why) => write!(out, "{why}"),
            Failed::Parse(why) => write!(out, "{why}"),
            Failed::Gone => write!(out, "the other side hung up"),
        }
    }
}

impl From<io::Error> for Failed {
    fn from(why: io::Error) -> Failed {
        Failed::Net(why)
    }
}

impl From<serde_json::Error> for Failed {
    fn from(why: serde_json::Error) -> Failed {
        Failed::Parse(why)
    }
}

fn told(conn: &mut TcpStream, what: &Tell) -> io::Result<()> {
    conn.write_all((dumps(what) + "\n").as_bytes())
}

fn passed_on(
    lobby: &Mutex<Lobby>,
    to_ticker: &SyncSender<Note>,
    name: &str,
    input: Input,
) -> Result<(), Failed> {
    lobby.lock().expect("not poisoned").ask(name, input);
    to_ticker.send(Note::Ready).map_err(|_| Failed::Gone)
}

fn playing(
    conn: &mut TcpStream,
    lines: &mut BufReader<TcpStream>,
    lobby: &Mutex<Lobby>,
    to_ticker: &SyncSender<Note>,
    replies: &Receiver<Tell>,
    name: &str,
) -> Result<Option<Tell>, Failed> {
    let mut line = String::new();
    loop {
        line.clear();
        if lines.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        match serde_json::from_str::<Ask>(line.trim_end_matches('\n'))? {
            Ask::Act { east, south, say } => {
                passed_on(lobby, to_ticker, name, Input { east, south, say, leaving: false })?;
                let Ok(view) = replies.recv() else { return Ok(None) };
                told(conn, &view)?;
            }
            Ask::Who => {
                let names = lobby.lock().expect("not poisoned").online();
                told(conn, &Tell::Online { names })?;
            }
            Ask::Leave => {
                passed_on(lobby, to_ticker, name, Input { leaving: true, ..Input::default() })?;
                let Ok(bye) = replies.recv() else { return Ok(None) };
                told(conn, &bye)?;
                return Ok(Some(bye));
            }
            Ask::Join { .. } => {}
        }
    }
}

fn attend(mut conn: TcpStream, lobby: &Mutex<Lobby>, to_ticker: &SyncSender<Note>) -> Result<bool, Failed> {
    let mut lines = BufReader::new(conn.try_clone()?);
    let mut first = String::new();
    if lines.read_line(&mut first)? == 0 {
        return Ok(false);
    }
    let Ask::Join { name } = serde_json::from_str::<Ask>(first.trim_end_matches('\n'))? else {
        return Ok(false);
    };
    if !lobby.lock().expect("not poisoned").join(&name) {
        return Ok(false);
    }
    let (to_player, replies) = sync_channel(4);
    to_ticker.send(Note::Joined(name.clone(), to_player)).map_err(|_| Failed::Gone)?;
    let Ok(welcome) = replies.recv() else { return Ok(false) };
    told(&mut conn, &welcome)?;
    let bye = match playing(&mut conn, &mut lines, lobby, to_ticker, &replies, &name) {
        Ok(bye) => bye,
        Err(why) => {
            lobby.lock().expect("not poisoned").dropped.push(format!("{name}: {why}"));
            None
        }
    };
    let bye = match bye {
        Some(bye) => Some(bye),
        None => {
            passed_on(lobby, to_ticker, &name, Input { leaving: true, ..Input::default() })?;
            replies.recv().ok()
        }
    };
    Ok(matches!(bye, Some(Tell::Bye { last: true, .. })))
}

fn admit(server: &TcpListener, lobby: &Mutex<Lobby>, to_ticker: &SyncSender<Note>) {
    let address = server.local_addr().expect("listening");
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for conn in server.incoming() {
            if done.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let done = &done;
            scope.spawn(move || {
                let last = match attend(conn, lobby, to_ticker) {
                    Ok(last) => last,
                    Err(why) => {
                        lobby.lock().expect("not poisoned").dropped.push(format!("a connection: {why}"));
                        false
                    }
                };
                if last {
                    done.store(true, Ordering::SeqCst);
                    // Wakes the accept loop, so it sees it's done.
                    let _ = TcpStream::connect(address);
                }
            });
        }
    });
}

#[derive(Default)]
struct Played {
    name: String,
    ticks: i64,
    heard: i64,
    monsters: i64,
    online: i64,
    score: i64,
    health: i64,
    trail: i64,
    failed: String,
}

fn ask(conn: &mut TcpStream, what: &Ask) -> io::Result<()> {
    conn.write_all((dumps(what) + "\n").as_bytes())
}

fn answer(lines: &mut BufReader<TcpStream>) -> Result<Tell, Failed> {
    let mut line = String::new();
    if lines.read_line(&mut line)? == 0 {
        return Err(Failed::Gone);
    }
    Ok(serde_json::from_str(line.trim_end_matches('\n'))?)
}

fn mixed(hash: i64, value: i64) -> i64 {
    (hash * 31 + value) % 1_000_000_007
}

fn text_mixed(hash: i64, text: &str) -> i64 {
    text.bytes().fold(hash, |hash, byte| mixed(hash, i64::from(byte)))
}

fn steer(number: i64, turn: i64) -> (i64, i64) {
    let seed = next(number * 7_919 + turn / 8);
    (below(seed, 3) - 1, below(next(seed), 3) - 1)
}

fn chatter(number: i64, turn: i64) -> String {
    if (number * 7 + turn) % 23 != 0 {
        return String::new();
    }
    let first = below(next(number * 31 + turn), WORDS.len() as i64) as usize;
    format!("{} {} at {turn}", WORDS[first], WORDS[(first + 5) % WORDS.len()])
}

fn stay(number: i64) -> i64 {
    TICKS - number % 4 * 40
}

fn watched(played: &mut Played, view: &View) {
    let mut trail = mixed(mixed(played.trail, view.spot.x), view.spot.y);
    for line in &view.heard {
        trail = text_mixed(trail, line);
    }
    played.ticks += 1;
    played.heard += view.heard.len() as i64;
    played.monsters += view.monsters;
    played.health = view.health;
    played.score = view.score;
    played.trail = trail;
}

fn play(address: SocketAddr, number: i64) -> Result<Played, Failed> {
    let mut played = Played { name: format!("player{number}"), ..Played::default() };
    let mut conn = TcpStream::connect(address)?;
    let mut lines = BufReader::new(conn.try_clone()?);
    ask(&mut conn, &Ask::Join { name: played.name.clone() })?;
    let Tell::Welcome { spot } = answer(&mut lines)? else { return Ok(played) };
    played.trail = mixed(spot.x, spot.y);
    for turn in 1..=stay(number) {
        if (number + turn) % 50 == 0 {
            ask(&mut conn, &Ask::Who)?;
            let Tell::Online { names } = answer(&mut lines)? else { return Ok(played) };
            played.online += names.len() as i64;
        }
        let (east, south) = steer(number, turn);
        ask(&mut conn, &Ask::Act { east, south, say: chatter(number, turn) })?;
        let Tell::Saw { view } = answer(&mut lines)? else { return Ok(played) };
        watched(&mut played, &view);
    }
    if number % 16 == 5 {
        // Hangs up without a word.
        return Ok(played);
    }
    ask(&mut conn, &Ask::Leave)?;
    if let Tell::Bye { score, .. } = answer(&mut lines)? {
        played.score = score;
    }
    Ok(played)
}

fn client(address: SocketAddr, number: i64) -> Played {
    play(address, number).unwrap_or_else(|why| Played {
        name: format!("player{number}"),
        failed: format!("{why}"),
        ..Played::default()
    })
}

fn run_instance(server: &TcpListener, lobby: &Mutex<Lobby>, db: &Mutex<Connection>) -> Vec<Played> {
    let address = server.local_addr().expect("listening");
    let (saves, saves_inbox) = sync_channel(8);
    let (to_ticker, ticker_inbox) = sync_channel(PLAYERS as usize * 2);
    std::thread::scope(|scope| {
        let saving = scope.spawn(move || saver(saves_inbox, db));
        scope.spawn(move || ticker(ticker_inbox, lobby, saves, PLAYERS as usize));
        scope.spawn(move || admit(server, lobby, &to_ticker));
        let clients: Vec<_> =
            (0..PLAYERS).map(|number| scope.spawn(move || client(address, number))).collect();
        let played = clients.into_iter().map(|one| one.join().expect("a client ran")).collect();
        saving.join().expect("the saver ran").expect("the saver wrote every snapshot");
        played
    })
}

fn made(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "create table if not exists snapshots (tick integer primary key, players integer, up integer, body text);
         create table if not exists players (name text primary key, x integer, y integer, health integer, score integer, kills integer, deaths integer, tick integer);
         delete from snapshots;
         delete from players;",
    )
}

fn report(played: &[Played], db: &Connection, dropped: &[String]) -> rusqlite::Result<Vec<String>> {
    let failed: Vec<&Played> = played.iter().filter(|one| !one.failed.is_empty()).collect();
    let mut lines =
        vec![format!("{} players, {} failed, {} dropped", played.len(), failed.len(), dropped.len())];
    for one in &failed {
        lines.push(format!("  {}: {}", one.name, one.failed));
    }
    lines.push(format!("ticks played: {}", played.iter().map(|one| one.ticks).sum::<i64>()));
    lines.push(format!("lines heard: {}", played.iter().map(|one| one.heard).sum::<i64>()));
    lines.push(format!("monsters seen: {}", played.iter().map(|one| one.monsters).sum::<i64>()));
    lines.push(format!("online seen: {}", played.iter().map(|one| one.online).sum::<i64>()));
    lines.push(format!(
        "scores: {}, health {}",
        played.iter().map(|one| one.score).sum::<i64>(),
        played.iter().map(|one| one.health).sum::<i64>()
    ));
    let trail = played.iter().fold(0, |trail, one| mixed(trail, one.trail));
    lines.push(format!("trail: {trail}"));
    let (snapshots, last, up): (i64, i64, i64) = db.query_row(
        "select count(*) as snapshots, max(tick) as last, min(up) as up from snapshots",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    lines.push(format!("snapshots: {snapshots}, last at tick {last}, fewest monsters up {up}"));
    let mut best =
        db.prepare("select name, score, kills, deaths from players order by score desc, name limit 3")?;
    let mut rows: Vec<(String, i64, i64, i64)> = best
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .collect::<Result<_, _>>()?;
    rows.push(db.query_row(
        "select 'everyone' as name, sum(score) as score, sum(kills) as kills, sum(deaths) as deaths from players",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?);
    for (name, score, kills, deaths) in rows {
        lines.push(format!("{name}: score {score}, kills {kills}, deaths {deaths}"));
    }
    Ok(lines)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let lobby = Mutex::new(Lobby { sessions: BTreeMap::new(), open: true, dropped: Vec::new() });
    let folder = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_owned());
    let server = TcpListener::bind("127.0.0.1:0")?;
    let path = format!("{folder}/rust-game-server-{}.db", server.local_addr()?.port());
    let lines = {
        let db = Connection::open(&path)?;
        let _: String = db.query_row("pragma journal_mode=wal", [], |row| row.get(0))?;
        made(&db)?;
        let db = Mutex::new(db);
        let played = run_instance(&server, &lobby, &db);
        let dropped = lobby.lock().expect("not poisoned").dropped.clone();
        let db = db.lock().expect("not poisoned");
        report(&played, &db, &dropped)?
    };
    for leftover in [path.clone(), format!("{path}-wal"), format!("{path}-shm")] {
        if std::path::Path::new(&leftover).exists() {
            std::fs::remove_file(&leftover)?;
        }
    }
    let mut out = io::stdout().lock();
    for line in lines {
        writeln!(out, "{line}")?;
    }
    Ok(())
}
