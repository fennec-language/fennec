//! `Messaged` statements: a task is sent enums over its inbox, and prints what each one held. Half
//! the shapes are flat, so they go as words once enums do (cheap messages, step 5): cases with no
//! fields, cases holding numbers and texts, an enum holding another, and a struct holding one. The
//! other half aren't, and are copied as blocks: a case holding a list, a case holding a dict, an
//! error enum, whose cases hold their notes, and an enum holding itself. A text is short, long
//! enough to need a block of its own, empty, or the sender's own, which it changes after each send,
//! so a message has to arrive as it was sent, not as the sender's text is by then.

use std::fmt::Write as _;

/// An enum's shape, and the type a statement sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Flat: cases with no fields, sent as their tags.
    Names,
    /// Flat: cases holding numbers and texts, and one with none.
    Events,
    /// Flat: a case holding an `Events` enum and a text, and one holding a number.
    Wrapped,
    /// Flat: a struct holding a number and an `Events` enum.
    Said,
    /// Not flat: a case holding a list of texts.
    Listing,
    /// Not flat: a case holding a dict.
    Tally,
    /// Not flat: an error enum.
    Failure,
    /// Not flat: an enum holding itself, which is boxed.
    Tree,
}

/// Every shape, to pick from.
const SHAPES: [Shape; 8] = [
    Shape::Names,
    Shape::Events,
    Shape::Wrapped,
    Shape::Said,
    Shape::Listing,
    Shape::Tally,
    Shape::Failure,
    Shape::Tree,
];

/// A text a message holds.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Text {
    /// `s` and a number: short enough to be kept in place.
    Short(u8),
    /// Long enough to need a block of its own.
    Long(u8),
    Empty,
    /// The sender's own text as it is when the message is sent.
    Kept,
}

/// A case of the `Events` enum.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Event {
    Join(i64, i64),
    Say(i64, Text),
    Quiet,
}

/// A value of the `Tree` enum.
#[derive(Clone, Debug)]
pub(crate) enum Tree {
    Leaf(i64),
    Pair(Box<Tree>, Box<Tree>),
}

/// One message, of the statement's shape.
#[derive(Clone, Debug)]
pub(crate) enum Message {
    /// `Names`: the case at this place, `A`, `B` or `C`.
    Name(usize),
    Event(Event),
    /// `Wrapped`.
    Inner(Event, Text),
    Plain(i64),
    /// `Said`: its number and its event.
    Said(i64, Event),
    /// `Listing`.
    Items(Vec<Text>),
    Empty,
    /// `Tally`: the counts at some of `KEYS`.
    Counts(Vec<(usize, i64)>),
    Nothing,
    /// `Failure`.
    Bad(i64),
    Worse(Text),
    Tree(Tree),
}

/// A `Messaged` statement: messages of one shape sent to a task, the sender changing its own text
/// after each send when `changes`.
#[derive(Clone, Debug)]
pub(crate) struct Messaging {
    pub(crate) id: usize,
    pub(crate) shape: Shape,
    pub(crate) messages: Vec<Message>,
    pub(crate) changes: bool,
}

/// The keys a `Tally`'s counts may hold.
const KEYS: [&str; 3] = ["a", "b", "c"];

/// What the sender's own text starts as.
const KEPT: &str = "the sender's own text, long enough to need a block of its own";

fn number(choose: &mut impl FnMut(usize) -> usize) -> i64 {
    i64::try_from(choose(120)).unwrap_or(0) - 20
}

fn text(choose: &mut impl FnMut(usize) -> usize) -> Text {
    let at = u8::try_from(choose(10)).unwrap_or(0);
    match choose(4) {
        0 => Text::Short(at),
        1 => Text::Long(at),
        2 => Text::Empty,
        _ => Text::Kept,
    }
}

fn event(choose: &mut impl FnMut(usize) -> usize) -> Event {
    match choose(3) {
        0 => Event::Join(number(choose), number(choose)),
        1 => Event::Say(number(choose), text(choose)),
        _ => Event::Quiet,
    }
}

fn tree(depth: usize, choose: &mut impl FnMut(usize) -> usize) -> Tree {
    if depth == 0 || choose(2) == 0 {
        return Tree::Leaf(number(choose));
    }
    Tree::Pair(Box::new(tree(depth - 1, choose)), Box::new(tree(depth - 1, choose)))
}

fn message(shape: Shape, choose: &mut impl FnMut(usize) -> usize) -> Message {
    match shape {
        Shape::Names => Message::Name(choose(3)),
        Shape::Events => Message::Event(event(choose)),
        Shape::Wrapped if choose(3) == 0 => Message::Plain(number(choose)),
        Shape::Wrapped => Message::Inner(event(choose), text(choose)),
        Shape::Said => Message::Said(number(choose), event(choose)),
        Shape::Listing if choose(4) == 0 => Message::Empty,
        Shape::Listing => Message::Items((0..choose(5)).map(|_| text(choose)).collect()),
        Shape::Tally if choose(4) == 0 => Message::Nothing,
        Shape::Tally => {
            let mut counts = Vec::new();
            for key in 0..KEYS.len() {
                if choose(2) == 0 {
                    counts.push((key, number(choose)));
                }
            }
            Message::Counts(counts)
        }
        Shape::Failure if choose(2) == 0 => Message::Bad(number(choose)),
        Shape::Failure => Message::Worse(text(choose)),
        Shape::Tree => Message::Tree(tree(3, choose)),
    }
}

/// A `Messaged` statement of a shape `choose` picks, with 2 to 8 messages.
pub(crate) fn picked(id: usize, choose: &mut impl FnMut(usize) -> usize) -> Messaging {
    let shape = SHAPES.get(choose(SHAPES.len())).copied().unwrap_or(Shape::Events);
    let messages = (0..2 + choose(7)).map(|_| message(shape, choose)).collect();
    Messaging { id, shape, messages, changes: choose(4) != 0 }
}

/// What a text is: its value as the program has it, given the sender's own.
fn text_value(text: Text, kept: &str) -> String {
    match text {
        Text::Short(at) => format!("s{at}"),
        Text::Long(at) => format!("a text long enough to need a block of its own, number {at}"),
        Text::Empty => String::new(),
        Text::Kept => kept.to_owned(),
    }
}

/// A text as Fennec writes it: a literal, or the sender's own.
fn text_written(text: Text) -> String {
    match text {
        Text::Kept => "text".to_owned(),
        other => format!("\"{}\"", text_value(other, "")),
    }
}

fn event_shown(event: Event, kept: &str) -> String {
    match event {
        Event::Join(user, room) => format!("join {user} {room}"),
        Event::Say(user, text) => format!("say {user} [{}]", text_value(text, kept)),
        Event::Quiet => "quiet".to_owned(),
    }
}

fn event_written(event: Event) -> String {
    match event {
        Event::Join(user, room) => format!(".Join(user={user}, room={room})"),
        Event::Say(user, text) => format!(".Say(user={user}, text={})", text_written(text)),
        Event::Quiet => ".Quiet()".to_owned(),
    }
}

fn tree_shown(tree: &Tree) -> String {
    match tree {
        Tree::Leaf(value) => value.to_string(),
        Tree::Pair(left, right) => format!("({} {})", tree_shown(left), tree_shown(right)),
    }
}

fn tree_written(tree: &Tree) -> String {
    match tree {
        Tree::Leaf(value) => format!(".Leaf(value={value})"),
        Tree::Pair(left, right) => {
            format!(".Pair(left={}, right={})", tree_written(left), tree_written(right))
        }
    }
}

/// What the task prints for a message, given the sender's own text when it was sent.
fn shown(message: &Message, kept: &str) -> String {
    match message {
        Message::Name(at) => ["a", "b", "c"].get(*at).copied().unwrap_or("a").to_owned(),
        Message::Event(event) => event_shown(*event, kept),
        Message::Inner(event, tag) => {
            format!("inner {} [{}]", event_shown(*event, kept), text_value(*tag, kept))
        }
        Message::Plain(number) => format!("plain {number}"),
        Message::Said(seq, event) => format!("said {seq} {}", event_shown(*event, kept)),
        Message::Items(items) => {
            let mut out = format!("items {}", items.len());
            for item in items {
                let _ = write!(out, " [{}]", text_value(*item, kept));
            }
            out
        }
        Message::Empty => "empty".to_owned(),
        Message::Counts(counts) => {
            let at =
                |key: usize| counts.iter().find(|(held, _)| *held == key).map_or(-1, |(_, count)| *count);
            format!("counts {} {} {} {}", at(0), at(1), at(2), counts.len())
        }
        Message::Nothing => "nothing".to_owned(),
        Message::Bad(code) => format!("bad {code}"),
        Message::Worse(why) => format!("worse [{}]", text_value(*why, kept)),
        Message::Tree(tree) => tree_shown(tree),
    }
}

/// A message as Fennec writes it, to send.
fn message_written(id: usize, message: &Message) -> String {
    match message {
        Message::Name(at) => format!(".{}()", ["A", "B", "C"].get(*at).copied().unwrap_or("A")),
        Message::Event(event) => event_written(*event),
        Message::Inner(event, tag) => {
            format!(".Inner(event={}, tag={})", event_written(*event), text_written(*tag))
        }
        Message::Plain(number) => format!(".Plain(number={number})"),
        Message::Said(seq, event) => format!("Said{id}(seq={seq}, event={})", event_written(*event)),
        Message::Items(items) => {
            let items: Vec<String> = items.iter().map(|item| text_written(*item)).collect();
            format!(".Items(items=[{}])", items.join(", "))
        }
        Message::Empty => ".Empty()".to_owned(),
        Message::Counts(counts) => {
            let counts: Vec<String> = counts
                .iter()
                .map(|(key, count)| format!("\"{}\": {count}", KEYS.get(*key).copied().unwrap_or("a")))
                .collect();
            format!(".Counts(counts={{{}}})", counts.join(", "))
        }
        Message::Nothing => ".Nothing()".to_owned(),
        Message::Bad(code) => format!(".Bad(code={code})"),
        Message::Worse(why) => format!(".Worse(why={})", text_written(*why)),
        Message::Tree(tree) => tree_written(tree),
    }
}

/// What a `Messaged` statement prints: what the task heard, in order, then the sender's own text.
pub(crate) fn printed(messaging: &Messaging) -> Vec<String> {
    let mut kept = KEPT.to_owned();
    let mut lines = Vec::new();
    for (at, message) in messaging.messages.iter().enumerate() {
        lines.push(format!("got {}", shown(message, &kept)));
        if messaging.changes {
            let _ = write!(kept, " {at}");
        }
    }
    lines.push(format!("kept [{kept}]"));
    lines
}

/// The `Events` enum and its `shown` function, which `Events`, `Wrapped` and `Said` use.
fn events_written(id: usize) -> String {
    format!(
        "enum Event{id}:\n    Join(user: int, room: int)\n    Say(user: int, text: str)\n    Quiet\n\n\
         def event_shown{id}(event: Event{id}) -> str:\n    match event:\n        case .Join(user, room):\n            \
         return f\"join {{user}} {{room}}\"\n        case .Say(user, text):\n            return f\"say {{user}} [{{text}}]\"\n        \
         case .Quiet:\n            return \"quiet\"\n\n"
    )
}

/// The types a shape declares, the type it sends, and the body of its `shown` function.
fn shape_written(id: usize, shape: Shape) -> (String, String, String) {
    let (declared, sent, cases) = match shape {
        Shape::Names => (
            format!("enum Names{id}:\n    A\n    B\n    C\n\n"),
            format!("Names{id}"),
            "        case .A:\n            return \"a\"\n        case .B:\n            return \"b\"\n        case .C:\n            \
             return \"c\""
                .to_owned(),
        ),
        Shape::Events => (events_written(id), format!("Event{id}"), String::new()),
        Shape::Wrapped => (
            format!("{}enum Wrapped{id}:\n    Inner(event: Event{id}, tag: str)\n    Plain(number: int)\n\n", events_written(id)),
            format!("Wrapped{id}"),
            format!(
                "        case .Inner(event, tag):\n            return f\"inner {{event_shown{id}(event)}} [{{tag}}]\"\n        \
                 case .Plain(number):\n            return f\"plain {{number}}\""
            ),
        ),
        Shape::Said => (
            format!("{}struct Said{id}:\n    seq: int\n    event: Event{id}\n\n", events_written(id)),
            format!("Said{id}"),
            String::new(),
        ),
        Shape::Listing => (
            format!("enum Listing{id}:\n    Empty\n    Items(items: list[str])\n\n"),
            format!("Listing{id}"),
            "        case .Empty:\n            return \"empty\"\n        case .Items(items):\n            \
             mut out = f\"items {len(items)}\"\n            for item in items:\n                out += f\" [{item}]\"\n            \
             return out"
                .to_owned(),
        ),
        Shape::Tally => (
            format!("enum Tally{id}:\n    Counts(counts: dict[str, int])\n    Nothing\n\n"),
            format!("Tally{id}"),
            "        case .Counts(counts):\n            first = counts.get(\"a\") else -1\n            \
             second = counts.get(\"b\") else -1\n            third = counts.get(\"c\") else -1\n            \
             return f\"counts {first} {second} {third} {len(counts)}\"\n        case .Nothing:\n            \
             return \"nothing\""
                .to_owned(),
        ),
        Shape::Failure => (
            format!("error Oops{id}:\n    Bad(code: int)\n    Worse(why: str)\n\n"),
            format!("Oops{id}"),
            "        case .Bad(code):\n            return f\"bad {code}\"\n        case .Worse(why):\n            \
             return f\"worse [{why}]\""
                .to_owned(),
        ),
        Shape::Tree => (
            format!("enum Tree{id}:\n    Leaf(value: int)\n    Pair(left: Tree{id}, right: Tree{id})\n\n"),
            format!("Tree{id}"),
            format!(
                "        case .Leaf(value):\n            return f\"{{value}}\"\n        case .Pair(left, right):\n            \
                 return f\"({{shown{id}(left)}} {{shown{id}(right)}})\""
            ),
        ),
    };
    let body = match shape {
        Shape::Events => format!("    return event_shown{id}(got)\n"),
        // A struct isn't matched: its fields are read.
        Shape::Said => format!("    return f\"said {{got.seq}} {{event_shown{id}(got.event)}}\"\n"),
        _ => format!("    match got:\n{cases}\n"),
    };
    (declared, sent, body)
}

/// `messaged{id}`, the function a `Messaged` calls, after the types and functions it uses.
pub(crate) fn written(messaging: &Messaging) -> String {
    let Messaging { id, shape, messages, changes } = messaging;
    let (declared, sent, body) = shape_written(*id, *shape);
    let changeable = if *changes { "mut " } else { "" };
    let mut out = format!(
        "{declared}def shown{id}(got: {sent}) -> str:\n{body}\n\
         def heard{id}(inbox: Inbox[{sent}], stdout: Stdout):\n    for got in inbox:\n        stdout.print(\"got\", shown{id}(got))\n\n\
         def messaged{id}(stdout: Stdout) fails Closed | Crashed:\n    {changeable}text = \"{KEPT}\"\n    \
         with launch((inbox) => heard{id}(inbox, stdout))? as sends:\n"
    );
    for (at, message) in messages.iter().enumerate() {
        let _ = writeln!(out, "        sends.send({})?", message_written(*id, message));
        if *changes {
            let _ = writeln!(out, "        text += \" {at}\"");
        }
    }
    let _ = write!(out, "    stdout.print(\"kept\", f\"[{{text}}]\")\n    return\n\n");
    out
}
