//! The reference evaluator: runs a program tree as the language says it runs, independently of
//! the compiler, giving what it prints and whether it stops with a failure.
//!
//! What it holds to: ints are 64 bits, and overflow, dividing by zero and an index outside a list
//! stop the program. `//` rounds toward minus infinity and `%` takes the divisor's sign. A negative
//! index counts from the end. Every name holds its own value. An assignment works out its place
//! first, then reads the old value for `+=`, then works out the new one, so a `ref` call on the
//! right sees the place as it was. Where the language doesn't say, as for a sum that overflows
//! partway but not at the end, it says it's unsure rather than guess.

#![expect(clippy::too_many_lines, reason = "one match runs each statement, and one each expression")]

use std::fmt::Write as _;

use super::ast::{
    Alias, Aliasing, Amount, Arith, Arrow, BufOp, Case, Compare, Constants, Expr, FLOATS, FloatHeld, KEYS,
    Kept, LEDGER, Leave, Nested, Op, Place, Program, RefPlace, Size, Stmt, StreamMap, StreamRead, Streaming,
    TABLE, Turn, Turning, Writing, XS, YS, alias_name, starting,
};
use super::ast::{ArrayWay, Arraying};
use super::ast::{Between, Looping, Reading};
use super::ast::{
    Calling, CaptureWay, Capturing, Field, FieldCall, FieldCalls, Listening, REF_TEXTS, RefCasing, RefWay,
    SWAP_KEYS, ShelfPlace, Shelving, SwapField, Swapping, Waking, Yielding, ZipSumming, ZipWay,
};
use super::ast::{FILE_PIECES, FileWay, Filing};
use super::ast::{GenStep, GenWay, Generating};
use super::ast::{GenericStep, Genericking};
use super::ast::{IfaceStep, Interfacing};
use super::ast::{InOrder, OrderStep};
use super::ast::{LockWay, LockingTogether};
use super::ast::{NarrowWay, Narrowing};
use super::ast::{OwnIndexing, OwnStep};
use super::ast::{PopWay, Popping};
use super::ast::{RuleStep, RuleWay, Ruling};
use super::ast::{SizedStep, SizedWay, Sizing, sized_step};
use super::ast::{Sticking, StuckWay};
use super::source::LONG_PIECE;

/// A `Narrowed` statement's union as the evaluator holds it: a `Crate{id}`'s list or a `Heap{id}`'s
/// int.
#[derive(Clone, Debug)]
enum Member {
    Crate(Vec<i64>),
    Heap(i64),
}

impl Member {
    /// What `grow` does to it, and what it gives back.
    fn grow(&mut self) -> i64 {
        match self {
            Member::Crate(items) => {
                let length = |items: &Vec<i64>| i64::try_from(items.len()).unwrap_or(i64::MAX);
                items.push(length(items) + 7);
                length(items)
            }
            Member::Heap(held) => {
                *held += 3;
                *held
            }
        }
    }

    /// What `shown{id}` gives back for it.
    fn shown(&self) -> String {
        match self {
            Member::Crate(items) => {
                let items: Vec<String> = items.iter().map(ToString::to_string).collect();
                format!("crate [{}]", items.join(", "))
            }
            Member::Heap(held) => format!("heap {held}"),
        }
    }

    /// The other member, holding what `swapped{id}` gives it: `grown`.
    fn swapped(&self, grown: i64) -> Member {
        match self {
            Member::Crate(_) => Member::Heap(grown),
            Member::Heap(_) => Member::Crate(vec![grown]),
        }
    }
}

/// A `Swapped`'s struct as the evaluator holds it: its counts and its lists, each in the order
/// their keys went in.
type Desk = (Vec<(String, i64)>, Vec<(String, Vec<i64>)>);

/// Puts `value` at `key`, where it was or last.
fn put<T>(pairs: &mut Vec<(String, T)>, key: &str, value: T) {
    match pairs.iter_mut().find(|(held, _)| held == key) {
        Some(slot) => slot.1 = value,
        None => pairs.push((key.to_owned(), value)),
    }
}

/// A `Swapped`'s struct as Fennec prints it.
fn desk_shown(id: usize, (counts, lists): &Desk) -> String {
    let counts: Vec<String> = counts.iter().map(|(key, n)| format!("\"{key}\": {n}")).collect();
    let lists: Vec<String> = lists
        .iter()
        .map(|(key, items)| format!("\"{key}\": {}", shown(&Value::Ints(items.clone()))))
        .collect();
    format!("Desk{id}(counts={{{}}}, lists={{{}}})", counts.join(", "), lists.join(", "))
}

/// A value while the program runs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    Ints(Vec<i64>),
    Point(i64, i64),
    Shape(Case, i64),
    /// A sized integer, its value whatever its size.
    Sized(Size, i128),
    /// A `shared Point`, by where it is on the heap.
    Shared(usize),
    /// A `shared Bag`, by where it is among the bags.
    Bag(usize),
    Holder(Vec<i64>, i64),
    Lists(Vec<(String, Vec<i64>)>),
    /// A function summing the list it captured.
    Count(Vec<i64>),
    Float(u64),
    Tally(i64),
    /// A `dict[str, int]`, in the order its keys went in.
    Counts(Vec<(String, i64)>),
    /// A `Buffer[int]`'s filled slots.
    Buf(Vec<i64>),
    /// A `shared Outer`, by where its number is.
    Outer(usize),
    Node(Tree),
    Nodes(Vec<Tree>),
    Texts(Vec<String>),
    /// A tree from `deep`, by how deep it is.
    Deep(i64),
    Named(Named),
    /// A `Named` from `chain`, by how deep it is.
    Chain(i64),
}

/// A `Named`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Named {
    Leaf(String),
    Pair(Box<Named>, Box<Named>, String, Vec<String>),
}

impl Named {
    /// How deep it goes and how many it holds, walking without recursing, so a deep one can't
    /// overflow the evaluator's stack.
    fn measure(&self) -> (usize, usize) {
        let mut waiting = vec![(self, 1)];
        let (mut deepest, mut count) = (0, 0);
        while let Some((named, depth)) = waiting.pop() {
            deepest = deepest.max(depth);
            count += 1;
            if let Named::Pair(left, right, _, _) = named {
                waiting.extend([(left.as_ref(), depth + 1), (right.as_ref(), depth + 1)]);
            }
        }
        (deepest, count)
    }

    /// As the program's `named_digest` writes it.
    fn digest(&self, text: &mut String) {
        self.digest_as(text, ('l', 'p'));
    }

    /// As the program's `held_digest` writes a `Held` of the same shape.
    fn held_digest(&self, text: &mut String) {
        self.digest_as(text, ('h', 'q'));
    }

    /// A digest whose leaves and pairs start with `leaf` and `pair`.
    fn digest_as(&self, text: &mut String, (leaf, pair): (char, char)) {
        match self {
            Named::Leaf(name) => {
                let _ = write!(text, "{leaf}({name})");
            }
            Named::Pair(left, right, label, tags) => {
                let _ = write!(text, "{pair}({label}");
                for tag in tags {
                    text.push(',');
                    text.push_str(tag);
                }
                text.push(' ');
                left.digest_as(text, (leaf, pair));
                text.push(' ');
                right.digest_as(text, (leaf, pair));
                text.push(')');
            }
        }
    }
}

/// A `Named` made, or unsure past what the evaluator follows.
fn kept_named(named: Named) -> Ran<Named> {
    let (depth, count) = named.measure();
    if depth > MOST_TREE_DEPTH || count > MOST_TREE_NODES {
        return Err(Stop::Unsure);
    }
    Ok(named)
}

/// A `Node`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tree {
    Leaf(String),
    Number(i64),
    Branch(String, Vec<Tree>),
    Tagged(Vec<String>, Vec<Tree>),
}

/// How deep a tree the evaluator follows, and how many nodes it holds, past which it's unsure:
/// the program is about memory and its own stack there, not what it computes.
const MOST_TREE_DEPTH: usize = 2000;
const MOST_TREE_NODES: usize = 1 << 16;

impl Tree {
    /// How deep it goes and how many nodes it holds.
    fn measure(&self) -> (usize, usize) {
        let (texts, children) = match self {
            Tree::Leaf(_) | Tree::Number(_) => return (1, 1),
            Tree::Branch(_, children) => (0, children),
            Tree::Tagged(tags, inner) => (tags.len(), inner),
        };
        children.iter().fold((1, 1 + texts), |(depth, nodes), child| {
            let (below, held) = child.measure();
            (depth.max(below + 1), nodes + held)
        })
    }

    /// As `stdout.print` writes it.
    fn shown(&self) -> String {
        match self {
            Tree::Leaf(name) => format!("Node.Leaf(name=\"{name}\")"),
            Tree::Number(value) => format!("Node.Number(value={value})"),
            Tree::Branch(label, children) => {
                format!("Node.Branch(label=\"{label}\", children={})", trees_shown(children))
            }
            Tree::Tagged(tags, inner) => {
                format!("Node.Tagged(tags={}, inner={})", texts_shown(tags), trees_shown(inner))
            }
        }
    }

    /// As the program's `digest` writes it.
    fn digest(&self, text: &mut String) {
        match self {
            Tree::Leaf(name) => {
                let _ = write!(text, "L({name})");
            }
            Tree::Number(value) => {
                let _ = write!(text, "N({value})");
            }
            Tree::Branch(label, children) => {
                let _ = write!(text, "B({label}");
                for child in children {
                    text.push(' ');
                    child.digest(text);
                }
                text.push(')');
            }
            Tree::Tagged(tags, inner) => {
                text.push_str("T(");
                for tag in tags {
                    text.push_str(tag);
                    text.push(',');
                }
                for child in inner {
                    text.push(' ');
                    child.digest(text);
                }
                text.push(')');
            }
        }
    }
}

fn trees_shown(trees: &[Tree]) -> String {
    let trees: Vec<String> = trees.iter().map(Tree::shown).collect();
    format!("[{}]", trees.join(", "))
}

fn texts_shown(texts: &[String]) -> String {
    let texts: Vec<String> = texts.iter().map(|text| format!("\"{text}\"")).collect();
    format!("[{}]", texts.join(", "))
}

/// A tree made, or unsure past what the evaluator follows.
fn kept(tree: Tree) -> Ran<Tree> {
    let (depth, nodes) = tree.measure();
    if depth > MOST_TREE_DEPTH || nodes > MOST_TREE_NODES {
        return Err(Stop::Unsure);
    }
    Ok(tree)
}

/// Where a local `ref` points once its index is worked out.
enum Resolved {
    Item(String, usize),
    Field(String, bool),
    HeldItem(String, usize),
    HeldList(String),
    Tag(String),
    NodeItem(String, usize),
}

/// Why running stopped before the end.
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// A failure the language says stops the program.
    Failed,
    /// Something the language doesn't promise an outcome for.
    Unsure,
    Break,
    Continue,
}

/// How a program ran: what it printed, whether it ran to the end, and what a failure that stopped
/// it says first, where the evaluator knows. None where the evaluator can't be sure.
pub(crate) fn run_saying(program: &Program) -> Option<(String, bool, Option<String>)> {
    let mut machine = Machine {
        scopes: vec![Vec::new()],
        out: String::new(),
        said: None,
        heap: Vec::new(),
        bags: Vec::new(),
        outers: Vec::new(),
    };
    match machine.block(&program.body) {
        Ok(()) => {
            machine.out.push_str("done\n");
            Some((machine.out, true, None))
        }
        Err(Stop::Failed) => Some((machine.out, false, machine.said)),
        // A `break` or `continue` outside a loop never gets written.
        Err(Stop::Unsure | Stop::Break | Stop::Continue) => None,
    }
}

struct Machine {
    /// Each block's names and values, innermost last.
    scopes: Vec<Vec<(String, Value)>>,
    out: String,
    /// What a program stopped by a failure says first, where the evaluator knows it.
    said: Option<String>,
    /// What each `shared Point` holds.
    heap: Vec<(i64, i64)>,
    /// What each `shared Bag` holds.
    bags: Vec<Bag>,
    /// What each `shared Outer`'s number is.
    outers: Vec<i64>,
}

/// What a `Bag` holds: its list, and its dict in the order keys went in.
#[derive(Clone, Debug, Default)]
struct Bag {
    items: Vec<i64>,
    counts: Vec<(String, i64)>,
}

impl Bag {
    /// As `stdout.print` writes it.
    fn shown(&self) -> String {
        let items: Vec<String> = self.items.iter().map(ToString::to_string).collect();
        let counts: Vec<String> = self.counts.iter().map(|(key, n)| format!("\"{key}\": {n}")).collect();
        format!("Bag(items=[{}], counts={{{}}})", items.join(", "), counts.join(", "))
    }

    /// Makes each change in turn, with `number` what a task was sent, or None where one crashes.
    fn changed(mut self, ops: &[Op], number: i64) -> Option<Bag> {
        let amount = |value: Amount| match value {
            Amount::Literal(n) => Some(n),
            Amount::Sent(by) => number.checked_mul(by),
        };
        for op in ops {
            match op {
                Op::Append(value) => self.items.push(amount(*value)?),
                Op::Pop => {
                    self.items.pop();
                }
                Op::SetItem(at, value, add) => {
                    let len = i64::try_from(self.items.len()).ok()?;
                    let at = if *at < 0 { at + len } else { *at };
                    let value = amount(*value)?;
                    let item = self.items.get_mut(usize::try_from(at).ok()?)?;
                    *item = if *add { item.checked_add(value)? } else { value };
                }
                Op::SetCount(key, value, add) => {
                    let value = amount(*value)?;
                    match self.counts.iter_mut().find(|(held, _)| held == key) {
                        Some((_, n)) => *n = if *add { n.checked_add(value)? } else { value },
                        None if *add => return None,
                        None => self.counts.push(((*key).to_owned(), value)),
                    }
                }
                Op::Remove(key) => self.counts.retain(|(held, _)| held != key),
                Op::AddTwice(value) => {
                    let value = amount(*value)?;
                    self.items.extend([value, value]);
                }
                Op::GrowItems(value) => {
                    let value = amount(*value)?;
                    for item in &mut self.items {
                        *item = item.checked_add(value)?;
                    }
                }
                Op::RefItem(at, value) => {
                    let len = i64::try_from(self.items.len()).ok()?;
                    let at = if *at < 0 { at + len } else { *at };
                    let item = self.items.get_mut(usize::try_from(at).ok()?)?;
                    *item = item.checked_add(amount(*value)?)?.checked_mul(2)?;
                }
                Op::CountsCopied(key, value) => {
                    let value = amount(*value)?;
                    match self.counts.iter_mut().find(|(held, _)| held == key) {
                        Some((_, n)) => *n = value,
                        None => self.counts.push(((*key).to_owned(), value)),
                    }
                }
                Op::SelfAssigned => {}
                Op::PopCount(key) => {
                    let at = self.counts.iter().position(|(held, _)| held == key)?;
                    self.counts.remove(at);
                }
            }
        }
        Some(self)
    }
}

type Ran<T> = Result<T, Stop>;

fn int(value: &Value) -> Ran<i64> {
    if let Value::Int(n) = value { Ok(*n) } else { Err(Stop::Unsure) }
}

fn checked(found: Option<i64>) -> Ran<i64> {
    found.ok_or(Stop::Failed)
}

/// `left // right`, rounding toward minus infinity.
fn floor_div(left: i64, right: i64) -> Ran<i64> {
    let quotient = checked(left.checked_div(right))?;
    let rounded_down = (left % right != 0) && ((left < 0) != (right < 0));
    Ok(if rounded_down { quotient - 1 } else { quotient })
}

/// `left % right`, with the sign of `right`.
fn floor_rem(left: i64, right: i64) -> Ran<i64> {
    if right == 0 {
        return Err(Stop::Failed);
    }
    // `i64::MIN % -1` is 0 by the rule, but whether it stops the program isn't said.
    if left == i64::MIN && right == -1 {
        return Err(Stop::Unsure);
    }
    let rem = left % right;
    Ok(if rem != 0 && ((rem < 0) != (right < 0)) { rem + right } else { rem })
}

/// Where `at` is in a list `len` long, counting from the end when it's negative.
fn place_in(at: i64, len: usize) -> Ran<usize> {
    let len = i64::try_from(len).map_err(|_| Stop::Unsure)?;
    let at = if at < 0 { at + len } else { at };
    if (0..len).contains(&at) { usize::try_from(at).map_err(|_| Stop::Unsure) } else { Err(Stop::Failed) }
}

/// A float as Fennec writes it, which is as Python's `repr` does: the fewest digits that read
/// back as the same float, written out in full from 1e-4 up to 1e16, and with an exponent of at
/// least two digits past that.
pub(crate) fn float_text(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_owned();
    }
    if x.is_infinite() {
        return (if x < 0.0 { "-inf" } else { "inf" }).to_owned();
    }
    // Rust's shortest digits, as `d.ddde<exponent>`.
    let scientific = format!("{:e}", x.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if (-4..16).contains(&exponent) {
        let point = exponent + 1;
        let written = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(usize::try_from(-point).unwrap_or(0)))
        } else {
            let point = usize::try_from(point).unwrap_or(0);
            if digits.len() <= point {
                format!("{digits}{}.0", "0".repeat(point - digits.len()))
            } else {
                format!("{}.{}", digits.get(..point).unwrap_or(""), digits.get(point..).unwrap_or(""))
            }
        };
        return format!("{sign}{written}");
    }
    let (first, rest) = digits.split_at(1.min(digits.len()));
    let fraction = if rest.is_empty() { String::new() } else { format!(".{rest}") };
    let exponent = if exponent < 0 { format!("-{:02}", -exponent) } else { format!("+{exponent:02}") };
    format!("{sign}{first}{fraction}e{exponent}")
}

/// `n` if `size` holds it; a value it doesn't hold stops the program.
fn fitting(size: Size, n: i128) -> Ran<i128> {
    let (least, most) = size.range();
    if (least..=most).contains(&n) { Ok(n) } else { Err(Stop::Failed) }
}

/// `n` wrapped round into what `size` holds, as `wrapping_add` does.
fn wrapped(size: Size, n: i128) -> i128 {
    let (least, most) = size.range();
    (n - least).rem_euclid(most - least + 1) + least
}

/// Floats in Fennec's order: NaN equals itself and comes after everything, and `-0.0` is `0.0`.
fn float_order(left: f64, right: f64) -> std::cmp::Ordering {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => left.partial_cmp(&right).unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// The most positions a leaf of a `parallel` sum's tree holds, as the runtime's `REDUCE_LEAF`.
const REDUCE_LEAF: usize = 1024;

/// `terms` added as the runtime adds a `parallel` sum's parts: a range of at most [`REDUCE_LEAF`]
/// positions from its first term on, in order, and a longer one as its two halves, split at the
/// middle, the earlier first. `None` for no terms.
fn tree_sum<T: Copy>(terms: &[T], add: &dyn Fn(T, T) -> Ran<T>) -> Ran<Option<T>> {
    let Some((first, rest)) = terms.split_first() else { return Ok(None) };
    if terms.len() <= REDUCE_LEAF {
        let mut total = *first;
        for term in rest {
            total = add(total, *term)?;
        }
        return Ok(Some(total));
    }
    let (left, right) = terms.split_at(terms.len() / 2);
    match (tree_sum(left, add)?, tree_sum(right, add)?) {
        (Some(left), Some(right)) => add(left, right).map(Some),
        (one, other) => Ok(one.or(other)),
    }
}

/// Floats added as a plain `sum` adds them: term `at` into total `at % 8`, the totals added
/// pairwise, and then to the start.
fn lanes_sum(terms: &[f64], start: f64) -> f64 {
    let mut lanes = [0.0_f64; 8];
    for (at, term) in terms.iter().enumerate() {
        if let Some(lane) = lanes.get_mut(at % 8) {
            *lane += term;
        }
    }
    for (into, from) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (4, 6), (0, 4)] {
        let added = lanes.get(from).copied().unwrap_or(0.0);
        if let Some(lane) = lanes.get_mut(into) {
            *lane += added;
        }
    }
    start + lanes.first().copied().unwrap_or(0.0)
}

/// `sum(items)`. Overflowing partway but not at the end could go either way.
fn sum_of(items: &[i64]) -> Ran<i64> {
    let mut total: i64 = 0;
    let mut overflowed = false;
    for item in items {
        if let Some(next) = total.checked_add(*item) {
            total = next;
        } else {
            overflowed = true;
            total = total.wrapping_add(*item);
        }
    }
    let exact: i128 = items.iter().map(|item| i128::from(*item)).sum();
    if i64::try_from(exact).is_err() {
        return Err(Stop::Failed);
    }
    if overflowed {
        return Err(Stop::Unsure);
    }
    Ok(total)
}

/// A value as `stdout.print` writes it.
/// A `FieldCalls` struct, as the evaluator keeps it.
struct Boxed {
    text: String,
    count: i64,
    items: Vec<i64>,
}

/// What a `FieldCalls` call gives, or a field holds.
enum Piece {
    Text(String),
    Count(i64),
    Items(Vec<i64>),
}

impl Piece {
    /// As `stdout.print` writes it on its own.
    fn shown(&self) -> String {
        match self {
            Piece::Text(text) => text.clone(),
            Piece::Count(count) => count.to_string(),
            Piece::Items(items) => shown(&Value::Ints(items.clone())),
        }
    }
}

impl Boxed {
    fn read(&self, field: Field) -> Piece {
        match field {
            Field::Text => Piece::Text(self.text.clone()),
            Field::Count => Piece::Count(self.count),
            Field::Items => Piece::Items(self.items.clone()),
        }
    }

    /// `field += given`, or `field += a + given` when `plus`, with `a` made from `n`.
    fn added(&mut self, field: Field, plus: bool, n: i64, given: Piece) -> Option<()> {
        match (field, given) {
            (Field::Text, Piece::Text(given)) => {
                if plus {
                    let _ = write!(self.text, "a{n}");
                }
                self.text.push_str(&given);
            }
            (Field::Count, Piece::Count(given)) => {
                let added = if plus { n.checked_add(given)? } else { given };
                self.count = self.count.checked_add(added)?;
            }
            (Field::Items, Piece::Items(given)) => {
                if plus {
                    self.items.push(n);
                }
                self.items.extend(given);
            }
            _ => return None,
        }
        Some(())
    }

    /// As `stdout.print` writes the `Box{id}` it is.
    fn shown(&self, id: usize) -> String {
        format!(
            "Box{id}(text=\"{}\", count={}, items={})",
            self.text,
            self.count,
            shown(&Value::Ints(self.items.clone()))
        )
    }
}

/// The value at `KEYS[at]` among a dict's entries.
fn keyed<T>(pairs: &mut [(String, T)], at: usize) -> Option<&mut T> {
    let key = KEYS.get(at)?;
    pairs.iter_mut().find(|(name, _)| name == key).map(|(_, value)| value)
}

/// `value` changed by an `Aliased` step, or `None` where the step can't change it. A `Removed`
/// step prints, so `Machine::aliased` makes it.
fn changed(value: &mut Kept, how: Writing, at: usize, n: i64) -> Option<()> {
    match (value, how) {
        (Kept::Numbers(items), Writing::Append) => items.push(n),
        (Kept::Numbers(items), Writing::Set) => *items.get_mut(at)? = n,
        (Kept::Numbers(items), Writing::ForRef) => {
            for item in items {
                *item = item.checked_add(n)?;
            }
        }
        (Kept::Numbers(items), _) => {
            items.push(n);
            let first = items.first_mut()?;
            *first = first.checked_add(n)?;
        }
        (Kept::Holders(holders), Writing::ForRef) => {
            for (items, tag) in holders {
                *tag = tag.checked_add(n)?;
                items.push(n);
            }
        }
        (Kept::Holders(holders), Writing::Field) => holders.get_mut(at)?.1 = n,
        (Kept::Holders(holders), Writing::Append) => holders.get_mut(at)?.0.push(n),
        (Kept::Holders(holders), Writing::PutBack) => {
            let (items, tag) = holders.get_mut(at)?;
            *tag = tag.checked_add(n)?;
            items.push(n);
        }
        (Kept::Holders(holders), _) => holders.get_mut(at)?.0.extend([n, n]),
        (Kept::Keepers(holders), Writing::Field) => keyed(holders, at)?.1 = n,
        (Kept::Keepers(holders), Writing::Set) => *keyed(holders, at)?.0.first_mut()? = n,
        (Kept::Keepers(holders), Writing::PutBack) => {
            let (items, tag) = keyed(holders, at)?;
            *tag = tag.checked_add(n)?;
            items.push(n);
        }
        (Kept::Keepers(holders), _) => keyed(holders, at)?.0.push(n),
        (Kept::Holder(_, tag), Writing::Field) => *tag = n,
        (Kept::Holder(items, _), _) => items.push(n),
        (Kept::Lists(lists), Writing::Set) => *keyed(lists, at)?.first_mut()? = n,
        (Kept::Lists(lists), _) => keyed(lists, at)?.push(n),
        (Kept::Spots(points), Writing::PutBack) => keyed(points, at)?.1 = n,
        (Kept::Spots(points), _) => {
            let (x, _) = keyed(points, at)?;
            *x = x.checked_add(n)?;
        }
        (Kept::Counts(counts), Writing::Set) => *keyed(counts, at)? = n,
        (Kept::Counts(counts), _) => {
            let count = keyed(counts, at)?;
            *count = count.checked_add(n)?;
        }
        (Kept::Rooms(rooms), Writing::Set) => *keyed(&mut rooms.get_mut(at)?.0, 1)? = n,
        (Kept::Rooms(rooms), Writing::Field) => {
            let _ = write!(rooms.get_mut(at)?.1, "q{n}");
        }
        (Kept::Rooms(rooms), Writing::Method) => {
            let _ = write!(rooms.get_mut(at)?.1, "a{n}é{n}");
        }
        (Kept::Rooms(rooms), Writing::PutBack) => {
            let count = keyed(&mut rooms.get_mut(at)?.0, 0)?;
            *count = count.checked_add(n)?;
        }
        (Kept::Rooms(rooms), _) => {
            for (counts, label) in rooms {
                let count = keyed(counts, 0)?;
                *count = count.checked_add(n)?;
                let _ = write!(label, "p{n}");
            }
        }
        (Kept::Ledger(_, count), Writing::Field) => *count = count.checked_add(n)?,
        (Kept::Table(points) | Kept::Ledger(points, _) | Kept::Entries(points), Writing::Set) => {
            points.get_mut(at)?.0 = n;
        }
        (Kept::Table(points) | Kept::Ledger(points, _) | Kept::Entries(points), Writing::PutBack) => {
            let (_, y) = points.get_mut(at)?;
            *y = y.checked_add(n)?;
        }
        (Kept::Table(points) | Kept::Ledger(points, _) | Kept::Entries(points), Writing::Method) => {
            let (x, _) = points.get_mut(at)?;
            *x = x.checked_add(n)?;
        }
        (Kept::Table(points) | Kept::Ledger(points, _) | Kept::Entries(points), _) => {
            for (x, _) in points {
                *x = x.checked_add(n)?;
            }
        }
        (Kept::Point(x, _), Writing::Field) => *x = n,
        (Kept::Point(x, _), _) => *x = x.checked_add(n)?,
        (Kept::Text(text), Writing::Added) => {
            let _ = write!(text, "q{n}");
        }
        (Kept::Text(text), Writing::Formatted) => {
            let _ = write!(text, "r{n}");
        }
        (Kept::Text(text), Writing::Chained) => {
            let before = text.clone();
            let _ = write!(text, "a{n}{before}");
        }
        (Kept::Text(text), Writing::Summed) => {
            let before = text.clone();
            let _ = write!(text, "b{n}{before}ç");
        }
        (Kept::Text(text), Writing::Long) => {
            let _ = write!(text, "{LONG_PIECE}{n}");
        }
        (Kept::Text(text), _) => {
            let _ = write!(text, "p{n}");
        }
    }
    Some(())
}

/// `key` taken out of `pairs`, what `walk` makes of what's left, then `key` put back holding
/// `fresh`, last.
fn taken_out<T>(
    pairs: &mut Vec<(String, T)>,
    key: String,
    fresh: T,
    walk: impl Fn(&[(String, T)]) -> String,
) -> Option<String> {
    let place = pairs.iter().position(|(name, _)| *name == key)?;
    pairs.remove(place);
    let walked = walk(pairs);
    pairs.push((key, fresh));
    Some(walked)
}

/// `_ = value.remove(KEYS[at])` with what a walk of what's left prints, then the key put back,
/// last, with a value made from `n`, as `removed_written` writes it.
fn removed(value: &mut Kept, at: usize, n: i64) -> Option<String> {
    let key = (*KEYS.get(at)?).to_owned();
    match value {
        Kept::Lists(lists) => {
            taken_out(lists, key, vec![n, n], |pairs| walk_shown(&Kept::Lists(pairs.to_vec())))
        }
        Kept::Spots(points) => {
            taken_out(points, key, (n, n), |pairs| walk_shown(&Kept::Spots(pairs.to_vec())))
        }
        Kept::Keepers(holders) => {
            taken_out(holders, key, (vec![n, n], n), |pairs| walk_shown(&Kept::Keepers(pairs.to_vec())))
        }
        Kept::Counts(counts) => taken_out(counts, key, n, |pairs| walk_shown(&Kept::Counts(pairs.to_vec()))),
        _ => None,
    }
}

/// `value` with its key `at` set to a fresh value made from `n`, as `fresh_written` writes it,
/// then its values as `print(value.values())` writes them.
fn drained(value: &mut Kept, at: usize, n: i64) -> Option<String> {
    let listed = |shown: Vec<String>| format!("[{}]", shown.join(", "));
    match value {
        Kept::Lists(lists) => {
            *keyed(lists, at)? = vec![n, n];
            Some(listed(lists.iter().map(|(_, items)| shown(&Value::Ints(items.clone()))).collect()))
        }
        Kept::Spots(points) => {
            *keyed(points, at)? = (n, n);
            Some(listed(points.iter().map(|(_, (x, y))| format!("Point(x={x}, y={y})")).collect()))
        }
        Kept::Keepers(holders) => {
            *keyed(holders, at)? = (vec![n, n], n);
            Some(listed(
                holders.iter().map(|(_, (items, tag))| shown(&Value::Holder(items.clone(), *tag))).collect(),
            ))
        }
        Kept::Counts(counts) => {
            *keyed(counts, at)? = n;
            Some(shown(&Value::Ints(counts.iter().map(|(_, count)| *count).collect())))
        }
        _ => None,
    }
}

/// A `Part{id}` of an `Owned` statement as the evaluator keeps it.
#[derive(Clone)]
struct OwnPart {
    name: &'static str,
    x: i64,
    marks: Vec<i64>,
}

impl OwnPart {
    fn new(name: &'static str, x: i64) -> OwnPart {
        OwnPart { name, x, marks: Vec::new() }
    }

    /// `moved(by)`: adds `by` twice, noting it between.
    fn moved(&mut self, by: i64) -> Ran<()> {
        self.x = checked(self.x.checked_add(by))?;
        self.marks.push(by);
        self.x = checked(self.x.checked_add(by))?;
        Ok(())
    }

    /// `shift(by)`: adds `by` and notes it, then fails past 5, and otherwise adds 100. Whether it
    /// failed.
    fn shift(&mut self, by: i64) -> Ran<bool> {
        self.x = checked(self.x.checked_add(by))?;
        self.marks.push(by);
        if by > 5 {
            return Ok(true);
        }
        self.x = checked(self.x.checked_add(100))?;
        Ok(false)
    }

    /// How it prints, as `Part{id}(name="a", x=1, marks=[2])`.
    fn shown(&self, id: usize) -> String {
        format!(
            "Part{id}(name=\"{}\", x={}, marks={})",
            self.name,
            self.x,
            shown(&Value::Ints(self.marks.clone()))
        )
    }
}

/// A `Crate{id}` or a `Heap{id}` of an `Interfaced` statement as the evaluator keeps it.
#[derive(Clone)]
enum Grower {
    Crate(Vec<i64>),
    Heap(i64),
}

impl Grower {
    fn length(items: &[i64]) -> Ran<i64> {
        i64::try_from(items.len()).map_err(|_| Stop::Unsure)
    }

    /// `grow(by)`: a crate appends its length plus `by`, a heap adds `by` and 3.
    fn grow(&mut self, by: i64) -> Ran<i64> {
        match self {
            Grower::Crate(items) => {
                items.push(checked(Grower::length(items)?.checked_add(by))?);
                Grower::length(items)
            }
            Grower::Heap(held) => {
                *held = checked(held.checked_add(by).and_then(|held| held.checked_add(3)))?;
                Ok(*held)
            }
        }
    }

    /// `tried(by)`: a crate appends `by`, a heap adds it, then each fails past 5. What it gives
    /// back, or `None` for the failure.
    fn tried(&mut self, by: i64) -> Ran<Option<i64>> {
        let given = match self {
            Grower::Crate(items) => {
                items.push(by);
                Grower::length(items)?
            }
            Grower::Heap(held) => {
                *held = checked(held.checked_add(by))?;
                *held
            }
        };
        Ok((by <= 5).then_some(given))
    }

    fn size(&self) -> Ran<i64> {
        match self {
            Grower::Crate(items) => Grower::length(items),
            Grower::Heap(held) => Ok(*held),
        }
    }

    /// The sum of what `counts()` yields.
    fn counted(&self) -> Ran<i64> {
        match self {
            Grower::Crate(items) => {
                checked(items.iter().try_fold(0_i64, |total, item| total.checked_add(*item)))
            }
            Grower::Heap(held) => Ok((0..held.rem_euclid(4)).sum()),
        }
    }

    fn bumped(&mut self, by: i64) -> Ran<()> {
        self.grow(by)?;
        self.grow(checked(by.checked_add(1))?)?;
        Ok(())
    }

    fn shown(&self, id: usize) -> String {
        match self {
            Grower::Crate(items) => format!("Crate{id}(items={})", shown(&Value::Ints(items.clone()))),
            Grower::Heap(held) => format!("Heap{id}(held={held})"),
        }
    }
}

/// A `Tally{id}` of an `Ordered` statement as the evaluator keeps it: its items, its count, and
/// what its dict holds at `"a"`, its only key.
#[derive(Clone)]
struct Tallied {
    items: Vec<i64>,
    count: i64,
    named: i64,
}

impl Tallied {
    /// `grow(by)`: appends `count + by`, then gives back the count, 1 more.
    fn grow(&mut self, by: i64) -> Ran<i64> {
        self.items.push(self.count.checked_add(by).ok_or(Stop::Unsure)?);
        self.count = self.count.checked_add(1).ok_or(Stop::Unsure)?;
        Ok(self.count)
    }
}

/// An item a `Looped` step takes out, changes and puts back: a point, a room's counts and label,
/// a holder's items and tag, or a list.
#[derive(Clone)]
enum Taken {
    Point(i64, i64),
    Room(Vec<(String, i64)>, String),
    Holder(Vec<i64>, i64),
    Ints(Vec<i64>),
}

/// The value at `key` among a dict's entries, without changing it.
fn found<'a, T>(pairs: &'a [(String, T)], key: &str) -> Option<&'a T> {
    pairs.iter().find(|(name, _)| name == key).map(|(_, value)| value)
}

/// `value` put at `key`: where the key is, or last where it isn't, as a dict's store does.
fn stored<T>(pairs: &mut Vec<(String, T)>, key: &str, value: T) {
    match pairs.iter_mut().find(|(name, _)| name == key) {
        Some((_, held)) => *held = value,
        None => pairs.push((key.to_owned(), value)),
    }
}

/// The item a `Looped` step takes out of `value`, at `at`, or its `items` when `items`.
fn taken_out_of(value: &Kept, at: usize, items: bool) -> Option<Taken> {
    let key = KEYS.get(at)?;
    let holder = |(held, tag): &(Vec<i64>, i64)| {
        if items { Taken::Ints(held.clone()) } else { Taken::Holder(held.clone(), *tag) }
    };
    Some(match value {
        Kept::Table(points) | Kept::Entries(points) | Kept::Ledger(points, _) => {
            let (x, y) = points.get(at)?;
            Taken::Point(*x, *y)
        }
        Kept::Rooms(rooms) => {
            let (counts, label) = rooms.get(at)?;
            Taken::Room(counts.clone(), label.clone())
        }
        Kept::Holders(holders) => holder(holders.get(at)?),
        Kept::Keepers(holders) => holder(found(holders, key)?),
        Kept::Lists(lists) => Taken::Ints(found(lists, key)?.clone()),
        Kept::Spots(points) => {
            let (x, y) = found(points, key)?;
            Taken::Point(*x, *y)
        }
        _ => return None,
    })
}

/// `taken` put back into `value` at `at`, or as its item's `items` when `items`.
fn put_back(value: &mut Kept, at: usize, items: bool, taken: Taken) -> Option<()> {
    let key = KEYS.get(at)?;
    match (value, taken) {
        (Kept::Table(points) | Kept::Entries(points) | Kept::Ledger(points, _), Taken::Point(x, y)) => {
            *points.get_mut(at)? = (x, y);
        }
        (Kept::Rooms(rooms), Taken::Room(counts, label)) => *rooms.get_mut(at)? = (counts, label),
        (Kept::Holders(holders), Taken::Ints(held)) if items => holders.get_mut(at)?.0 = held,
        (Kept::Holders(holders), Taken::Holder(held, tag)) => *holders.get_mut(at)? = (held, tag),
        (Kept::Keepers(holders), Taken::Ints(held)) if items => keyed(holders, at)?.0 = held,
        (Kept::Keepers(holders), Taken::Holder(held, tag)) => stored(holders, key, (held, tag)),
        (Kept::Lists(lists), Taken::Ints(held)) => stored(lists, key, held),
        (Kept::Spots(points), Taken::Point(x, y)) => stored(points, key, (x, y)),
        _ => return None,
    }
    Some(())
}

/// The copy a `Looped` step takes changed, as `looped_written` changes it.
fn copy_changed(taken: &mut Taken, n: i64) -> Option<()> {
    match taken {
        Taken::Point(_, y) => *y = y.checked_add(n)?,
        Taken::Room(counts, _) => {
            let count = keyed(counts, 0)?;
            *count = count.checked_add(n)?;
        }
        Taken::Holder(items, tag) => {
            *tag = tag.checked_add(n)?;
            items.push(n);
        }
        Taken::Ints(items) => items.push(n),
    }
    Some(())
}

/// What a `Looped` step does between turns to `value`, after its `turn`th: whether the program
/// goes on, which it doesn't once a table's item is taken out.
fn between_turns(value: &mut Kept, looping: Looping, turn: i64) -> Option<bool> {
    let Looping { at, n, between, inside, items, .. } = looping;
    let key = *KEYS.get(at)?;
    match (value, between) {
        (Kept::Table(_) | Kept::Entries(_) | Kept::Ledger(..) | Kept::Rooms(_), Between::Removed) => {
            return Some(false);
        }
        (Kept::Table(points) | Kept::Entries(points) | Kept::Ledger(points, _), Between::Changed) => {
            let (x, _) = points.get_mut(at)?;
            *x = x.checked_add(n)?;
        }
        (Kept::Table(points) | Kept::Entries(points) | Kept::Ledger(points, _), Between::Grown(count)) => {
            points.extend((0..count).map(|more| (more, n)));
        }
        (Kept::Rooms(rooms), Between::Changed) => stored(&mut rooms.get_mut(at)?.0, "b", n),
        (Kept::Rooms(rooms), Between::Grown(count)) => {
            rooms.extend((0..count).map(|more| (vec![("a".to_owned(), more)], "g".to_owned())));
        }
        (Kept::Holders(holders), Between::Changed) => {
            let (held, tag) = holders.get_mut(at)?;
            if !items {
                *tag = n;
            }
            held.push(n);
        }
        (Kept::Holders(holders), Between::Grown(count)) => {
            holders.extend((0..count).map(|more| (vec![more], n)));
        }
        (Kept::Holders(holders), Between::Removed) => {
            if at >= holders.len() {
                return None;
            }
            holders.remove(at);
            holders.push((vec![n], n));
        }
        (Kept::Keepers(holders), Between::Changed) => {
            let (held, tag) = keyed(holders, at)?;
            if !items {
                *tag = n;
            }
            held.push(n);
        }
        (Kept::Lists(lists), Between::Changed) => keyed(lists, at)?.push(n),
        (Kept::Spots(points), Between::Changed) => {
            let (x, _) = keyed(points, at)?;
            *x = x.checked_add(n)?;
        }
        // Two steps can add the same key, the second storing where the first put it.
        (Kept::Keepers(holders), Between::Grown(count)) => {
            for more in 0..count {
                stored(holders, &format!("g{turn}x{more}"), (vec![more], n));
            }
        }
        (Kept::Lists(lists), Between::Grown(count)) => {
            for more in 0..count {
                stored(lists, &format!("g{turn}x{more}"), vec![more]);
            }
        }
        (Kept::Spots(points), Between::Grown(count)) => {
            for more in 0..count {
                stored(points, &format!("g{turn}x{more}"), (more, n));
            }
        }
        (Kept::Keepers(holders), Between::Removed) => {
            holders.retain(|(name, _)| name != key);
            if inside || items {
                holders.push((key.to_owned(), (vec![n], n)));
            }
        }
        (Kept::Lists(lists), Between::Removed) => {
            lists.retain(|(name, _)| name != key);
            if inside {
                lists.push((key.to_owned(), vec![n]));
            }
        }
        (Kept::Spots(points), Between::Removed) => {
            points.retain(|(name, _)| name != key);
            if inside {
                points.push((key.to_owned(), (n, n)));
            }
        }
        _ => return None,
    }
    Some(true)
}

/// What a `Looped` step does to `value`, as `looped_written` writes it: whether the program goes
/// on, which it doesn't once a table's item is taken out.
fn looped(value: &mut Kept, looping: Looping) -> Option<bool> {
    let Looping { at, n, turns, inside, items, .. } = looping;
    let mut taken = taken_out_of(value, at, items)?;
    copy_changed(&mut taken, n)?;
    for turn in 1..=turns {
        if inside {
            taken = taken_out_of(value, at, items)?;
            copy_changed(&mut taken, n)?;
        }
        put_back(value, at, items, taken.clone())?;
        if turn == turns {
            break;
        }
        if !between_turns(value, looping, turn)? {
            return Some(false);
        }
    }
    Some(true)
}

/// What a `Read` step does: prints, or keeps an item by a name of its own.
enum Read {
    Printed(String),
    Kept(i64, i64),
}

/// What a `Read` step's loop over `value`, as `read_written` writes it, does.
fn read_shown(value: &Kept, how: Reading, n: i64) -> Option<Read> {
    let points = match value {
        Kept::Table(points) | Kept::Entries(points) | Kept::Ledger(points, _) => points,
        Kept::Rooms(rooms) => {
            let summed = |counts: &Vec<(String, i64)>| {
                counts.iter().try_fold(0_i64, |total, (_, count)| total.checked_add(*count))
            };
            let length = |label: &String| i64::try_from(label.chars().count()).ok();
            let mut total = 0_i64;
            if how == Reading::Broken {
                for (counts, label) in rooms {
                    if summed(counts)? > n {
                        break;
                    }
                    total = total.checked_add(length(label)?)?;
                }
                return Some(Read::Printed(format!("broke {total}\n")));
            }
            for (counts, label) in rooms {
                total = total.checked_add(summed(counts)?)?.checked_add(length(label)?)?;
            }
            return Some(Read::Printed(format!("read {total}\n")));
        }
        _ => return None,
    };
    let point = |(x, y): &(i64, i64)| format!("Point(x={x}, y={y})");
    Some(match how {
        Reading::Summed => {
            let mut total = 0_i64;
            for (x, y) in points {
                total = total.checked_add(x.checked_mul(3)?.checked_add(*y)?)?;
            }
            Read::Printed(format!("read {total}\n"))
        }
        Reading::Kept => {
            let last = points.iter().rev().find(|(x, _)| *x >= n).copied().unwrap_or((0, 0));
            Read::Kept(last.0, last.1)
        }
        Reading::Replaced => {
            let mut lines = String::new();
            for (x, y) in points {
                let _ = writeln!(lines, "item {x} {y}");
            }
            Read::Printed(lines)
        }
        Reading::Broken => {
            let mut total = 0_i64;
            for (x, y) in points {
                if *x > n {
                    break;
                }
                total = total.checked_add(y.checked_add(1)?)?;
            }
            Read::Printed(format!("broke {total}\n"))
        }
        Reading::Returned => {
            let found = points.iter().find(|(x, _)| *x > n).copied().unwrap_or((-1, -1));
            Read::Printed(format!("first {}\n", point(&found)))
        }
        Reading::Failed => {
            let got = if points.iter().any(|(x, _)| *x > n) {
                -1
            } else {
                points.iter().try_fold(0_i64, |total, (_, y)| total.checked_add(*y))?
            };
            Read::Printed(format!("refused {got}\n"))
        }
    })
}

/// What `walk_written`'s line prints for `value`.
fn walk_shown(value: &Kept) -> String {
    fn walked<T>(pairs: &[(String, T)], summed: impl Fn(&T) -> i64, shown: impl Fn(&T) -> i64) -> String {
        let total: i64 = pairs.iter().map(|(_, value)| summed(value)).sum();
        let keys: Vec<String> = pairs.iter().map(|(key, _)| key.clone()).collect();
        let items: Vec<String> = pairs.iter().map(|(key, value)| format!("{key}={}", shown(value))).collect();
        format!("walk {total} {} {}", texts_shown(&keys), texts_shown(&items))
    }
    let length = |items: &Vec<i64>| i64::try_from(items.len()).unwrap_or(i64::MAX);
    match value {
        Kept::Lists(lists) => walked(lists, length, length),
        Kept::Spots(points) => walked(points, |point| point.0, |point| point.1),
        Kept::Keepers(holders) => walked(holders, |holder| holder.1, |holder| length(&holder.0)),
        Kept::Counts(counts) => walked(counts, |count| *count, |count| *count),
        Kept::Rooms(rooms) => {
            let sums: Vec<i64> =
                rooms.iter().map(|(counts, _)| counts.iter().map(|(_, count)| count).sum()).collect();
            let labels: Vec<String> = rooms.iter().map(|(_, label)| label.clone()).collect();
            format!("walk {} {}", shown(&Value::Ints(sums)), texts_shown(&labels))
        }
        _ => String::new(),
    }
}

/// What `handed{id}_{kind}(given, n)` gives back: what it works out from its own copy of
/// `given` after changing it, as the task's body in `source.rs` does.
fn handed(given: &Kept, n: i64) -> Option<i64> {
    let mut mine = given.clone();
    let length = |items: &[i64]| i64::try_from(items.len()).ok();
    match &mut mine {
        Kept::Numbers(items) => {
            items.push(n);
            *items.first_mut()? = n;
            items.iter().try_fold(0_i64, |total, item| total.checked_add(*item))
        }
        Kept::Holders(holders) => {
            holders.first_mut()?.0.push(n);
            holders.get_mut(1)?.1 = n;
            length(&holders.first()?.0)?.checked_add(holders.get(1)?.1)
        }
        Kept::Keepers(holders) => {
            keyed(holders, 0)?.0.push(n);
            keyed(holders, 1)?.1 = n;
            length(&keyed(holders, 0)?.0)?.checked_add(keyed(holders, 1)?.1)
        }
        Kept::Holder(items, tag) => {
            items.push(n);
            *tag = tag.checked_add(n)?;
            length(items)?.checked_add(*tag)
        }
        Kept::Lists(lists) => {
            keyed(lists, 0)?.push(n);
            *keyed(lists, 1)?.first_mut()? = n;
            length(keyed(lists, 0)?)?.checked_add(*keyed(lists, 1)?.first()?)
        }
        Kept::Spots(points) => {
            for (_, (x, _)) in points.iter_mut() {
                *x = x.checked_add(n)?;
            }
            keyed(points, 0)?.0.checked_add(keyed(points, 1)?.0)
        }
        Kept::Counts(counts) => {
            let count = keyed(counts, 0)?;
            *count = count.checked_add(n)?;
            counts.iter().try_fold(0_i64, |total, (_, count)| total.checked_add(*count))
        }
        Kept::Rooms(rooms) => {
            let mut total = 0_i64;
            for (counts, _) in rooms {
                let count = keyed(counts, 0)?;
                *count = count.checked_add(n)?;
                total = counts.iter().try_fold(total, |total, (_, count)| total.checked_add(*count))?;
            }
            Some(total)
        }
        Kept::Ledger(points, count) => {
            *count = count.checked_add(1)?;
            let mut total = *count;
            for (x, _) in points {
                *x = x.checked_add(n)?;
                total = total.checked_add(*x)?;
            }
            Some(total)
        }
        Kept::Table(points) | Kept::Entries(points) => {
            let mut total = 0_i64;
            for (x, _) in points {
                *x = x.checked_add(n)?;
                total = total.checked_add(*x)?;
            }
            Some(total)
        }
        Kept::Point(..) | Kept::Text(_) => None,
    }
}

/// A dict as `stdout.print` writes one, each value as `shown` writes it.
fn dict_shown<T>(pairs: &[(String, T)], shown: impl Fn(&T) -> String) -> String {
    let pairs: Vec<String> =
        pairs.iter().map(|(key, value)| format!("\"{key}\": {}", shown(value))).collect();
    format!("{{{}}}", pairs.join(", "))
}

/// How `stdout.print` writes a value of `aliased{id}`.
fn kept_shown(id: usize, value: &Kept) -> String {
    let point = |(x, y): &(i64, i64)| format!("Point(x={x}, y={y})");
    let table = |items: Vec<String>| format!("Table([{}])", items.join(", "));
    let holder = |(items, tag): &(Vec<i64>, i64)| shown(&Value::Holder(items.clone(), *tag));
    let counts = |counts: &Vec<(String, i64)>| dict_shown(counts, ToString::to_string);
    match value {
        Kept::Numbers(items) => shown(&Value::Ints(items.clone())),
        Kept::Holders(holders) => {
            let holders: Vec<String> = holders.iter().map(holder).collect();
            format!("[{}]", holders.join(", "))
        }
        Kept::Holder(items, tag) => holder(&(items.clone(), *tag)),
        Kept::Lists(lists) => dict_shown(lists, |items| shown(&Value::Ints(items.clone()))),
        Kept::Spots(points) => dict_shown(points, point),
        Kept::Keepers(holders) => dict_shown(holders, holder),
        Kept::Counts(pairs) => counts(pairs),
        Kept::Table(points) | Kept::Entries(points) => table(points.iter().map(point).collect()),
        Kept::Ledger(points, count) => {
            format!("Ledger{id}(points={}, count={count})", table(points.iter().map(point).collect()))
        }
        Kept::Rooms(rooms) => table(
            rooms
                .iter()
                .map(|(pairs, label)| format!("Room{id}(counts={}, label=\"{label}\")", counts(pairs)))
                .collect(),
        ),
        Kept::Point(x, y) => point(&(*x, *y)),
        Kept::Text(text) => text.clone(),
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::Int(n) => n.to_string(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Str(text) => text.clone(),
        Value::Ints(items) => {
            let items: Vec<String> = items.iter().map(ToString::to_string).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Point(x, y) => format!("Point(x={x}, y={y})"),
        Value::Shape(Case::Circle, radius) => format!("Shape.Circle(radius={radius})"),
        Value::Shape(Case::Square, side) => format!("Shape.Square(side={side})"),
        Value::Shape(Case::Empty, _) => "Shape.Empty".to_owned(),
        Value::Sized(_, n) => n.to_string(),
        Value::Tally(count) => format!("Tally(count={count})"),
        // None of these is ever printed whole.
        Value::Shared(_)
        | Value::Bag(_)
        | Value::Count(_)
        | Value::Outer(_)
        | Value::Deep(_)
        | Value::Named(_)
        | Value::Chain(_) => "shared".to_owned(),
        Value::Node(tree) => tree.shown(),
        Value::Nodes(trees) => trees_shown(trees),
        Value::Texts(texts) => texts_shown(texts),
        // A buffer prints as a list does.
        Value::Buf(items) => shown(&Value::Ints(items.clone())),
        Value::Float(bits) => float_text(f64::from_bits(*bits)),
        Value::Counts(pairs) => {
            let pairs: Vec<String> = pairs.iter().map(|(key, n)| format!("\"{key}\": {n}")).collect();
            format!("{{{}}}", pairs.join(", "))
        }
        Value::Holder(items, tag) => {
            let items: Vec<String> = items.iter().map(ToString::to_string).collect();
            format!("Holder(items=[{}], tag={tag})", items.join(", "))
        }
        Value::Lists(pairs) => {
            let pairs: Vec<String> = pairs
                .iter()
                .map(|(key, items)| {
                    let items: Vec<String> = items.iter().map(ToString::to_string).collect();
                    format!("\"{key}\": [{}]", items.join(", "))
                })
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
    }
}

/// The name a local `ref`'s place is in.
fn place_holder(place: &Resolved) -> &str {
    match place {
        Resolved::Item(name, _)
        | Resolved::Field(name, _)
        | Resolved::HeldItem(name, _)
        | Resolved::HeldList(name)
        | Resolved::Tag(name)
        | Resolved::NodeItem(name, _) => name,
    }
}

impl Machine {
    fn buffer_of(&mut self, expr: &Expr) -> Ran<Vec<i64>> {
        match self.expr(expr)? {
            Value::Buf(items) => Ok(items),
            _ => Err(Stop::Unsure),
        }
    }

    /// A change to the buffer `name`, its arguments worked out left to right first.
    fn buffered(&mut self, name: &str, op: &BufOp) -> Ran<()> {
        let place = |at: i64| usize::try_from(at).map_err(|_| Stop::Unsure);
        let other = match op {
            BufOp::Extend(other) => match self.get(other)? {
                Value::Buf(items) => items.clone(),
                _ => return Err(Stop::Unsure),
            },
            _ => Vec::new(),
        };
        let (first, second) = match op {
            BufOp::Push(value) | BufOp::Remove(value) | BufOp::Truncate(value) => {
                (Some(int(&self.expr(value)?)?), None)
            }
            BufOp::Set(at, value) | BufOp::Insert(at, value) => {
                (Some(int(&self.expr(at)?)?), Some(int(&self.expr(value)?)?))
            }
            BufOp::Pop | BufOp::Extend(_) => (None, None),
        };
        let Value::Buf(items) = self.slot(name)? else { return Err(Stop::Unsure) };
        match (op, first, second) {
            (BufOp::Push(_), Some(value), _) => items.push(value),
            (BufOp::Pop, ..) => {
                items.pop().ok_or(Stop::Failed)?;
            }
            (BufOp::Set(..), Some(at), Some(value)) => {
                *items.get_mut(place(at)?).ok_or(Stop::Failed)? = value;
            }
            (BufOp::Insert(..), Some(at), Some(value)) => {
                let at = place(at)?;
                if at > items.len() {
                    return Err(Stop::Failed);
                }
                items.insert(at, value);
            }
            (BufOp::Remove(_), Some(at), _) => {
                let at = place(at)?;
                if at >= items.len() {
                    return Err(Stop::Failed);
                }
                items.remove(at);
            }
            (BufOp::Truncate(_), Some(count), _) => items.truncate(place(count)?),
            (BufOp::Extend(_), ..) => items.extend(other),
            _ => return Err(Stop::Unsure),
        }
        Ok(())
    }

    fn tree_of(&mut self, expr: &Expr) -> Ran<Tree> {
        match self.expr(expr)? {
            Value::Node(tree) => Ok(tree),
            _ => Err(Stop::Unsure),
        }
    }

    /// Where a local `ref` points, its index worked out now and kept: outside the list fails.
    fn resolved(&mut self, place: &RefPlace) -> Ran<Resolved> {
        Ok(match place {
            RefPlace::Item(list, at) => {
                let at = int(&self.expr(at)?)?;
                let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                Resolved::Item(list.clone(), place_in(at, items.len())?)
            }
            RefPlace::Field(point, x) => Resolved::Field(point.clone(), *x),
            RefPlace::HeldItem(holder, at) => {
                let at = int(&self.expr(at)?)?;
                let Value::Holder(items, _) = self.get(holder)? else { return Err(Stop::Unsure) };
                Resolved::HeldItem(holder.clone(), place_in(at, items.len())?)
            }
            RefPlace::HeldList(holder) => Resolved::HeldList(holder.clone()),
            RefPlace::Tag(holder) => Resolved::Tag(holder.clone()),
            RefPlace::NodeItem(nodes, at) => {
                let at = int(&self.expr(at)?)?;
                let Value::Nodes(trees) = self.get(nodes)? else { return Err(Stop::Unsure) };
                Resolved::NodeItem(nodes.clone(), place_in(at, trees.len())?)
            }
        })
    }

    /// What a local `ref`'s place holds now.
    fn at_place(&self, place: &Resolved) -> Ran<Value> {
        Ok(match (place, self.get(place_holder(place))?) {
            (Resolved::Item(_, at), Value::Ints(items))
            | (Resolved::HeldItem(_, at), Value::Holder(items, _)) => {
                Value::Int(*items.get(*at).ok_or(Stop::Unsure)?)
            }
            (Resolved::Field(_, x), Value::Point(px, py)) => Value::Int(if *x { *px } else { *py }),
            (Resolved::HeldList(_), Value::Holder(items, _)) => Value::Ints(items.clone()),
            (Resolved::Tag(_), Value::Holder(_, tag)) => Value::Int(*tag),
            (Resolved::NodeItem(_, at), Value::Nodes(trees)) => {
                Value::Node(trees.get(*at).cloned().ok_or(Stop::Unsure)?)
            }
            _ => return Err(Stop::Unsure),
        })
    }

    /// Puts a value into a local `ref`'s place.
    fn put_in_place(&mut self, place: &Resolved, value: Value) -> Ran<()> {
        match (place, self.slot(place_holder(place))?, value) {
            (Resolved::Item(_, at), Value::Ints(items), Value::Int(n))
            | (Resolved::HeldItem(_, at), Value::Holder(items, _), Value::Int(n)) => {
                *items.get_mut(*at).ok_or(Stop::Unsure)? = n;
            }
            (Resolved::Field(_, true), Value::Point(px, _), Value::Int(n)) => *px = n,
            (Resolved::Field(_, false), Value::Point(_, py), Value::Int(n)) => *py = n,
            (Resolved::HeldList(_), Value::Holder(items, _), Value::Ints(list)) => *items = list,
            (Resolved::Tag(_), Value::Holder(_, tag), Value::Int(n)) => *tag = n,
            (Resolved::NodeItem(_, at), Value::Nodes(trees), Value::Node(tree)) => {
                *trees.get_mut(*at).ok_or(Stop::Unsure)? = tree;
            }
            _ => return Err(Stop::Unsure),
        }
        Ok(())
    }

    /// `ref name = place` and the statements using it. Each runs with the name holding what the
    /// place does, which goes back into the place after it, so reading the container between
    /// them sees each change, as the program does. No statement here both changes the name and
    /// reads the container.
    fn local_ref(&mut self, name: &str, place: &RefPlace, body: &[Stmt]) -> Ran<()> {
        let place = self.resolved(place)?;
        for stmt in body {
            let value = self.at_place(&place)?;
            self.scopes.push(vec![(name.to_owned(), value)]);
            let ran = self.stmt(stmt);
            let after = self.scopes.pop().and_then(|mut scope| scope.pop()).map(|(_, value)| value);
            // A failure stops the program, so what the place holds after it can't be seen.
            ran?;
            self.put_in_place(&place, after.ok_or(Stop::Unsure)?)?;
        }
        Ok(())
    }

    fn named_of(&mut self, expr: &Expr) -> Ran<Named> {
        match self.expr(expr)? {
            Value::Named(named) => Ok(named),
            _ => Err(Stop::Unsure),
        }
    }

    /// `match name:` changing a pair's label, tags and left side, or a leaf's name, through
    /// `ref`, the new left side worked out only for a pair.
    fn rename(&mut self, name: &str, text: &Expr, left: &Expr) -> Ran<()> {
        let Value::Named(named) = self.get(name)? else { return Err(Stop::Unsure) };
        let named = match named.clone() {
            Named::Leaf(mut held) => {
                held.push_str(&self.text_of(text)?);
                Named::Leaf(held)
            }
            Named::Pair(_, right, mut label, mut tags) => {
                label.push_str(&self.text_of(text)?);
                tags.push(label.clone());
                Named::Pair(Box::new(self.named_of(left)?), right, label, tags)
            }
        };
        *self.slot(name)? = Value::Named(kept_named(named)?);
        Ok(())
    }

    fn trees_of(&mut self, expr: &Expr) -> Ran<Vec<Tree>> {
        match self.expr(expr)? {
            Value::Nodes(trees) => Ok(trees),
            _ => Err(Stop::Unsure),
        }
    }

    fn text_of(&mut self, expr: &Expr) -> Ran<String> {
        match self.expr(expr)? {
            Value::Str(text) => Ok(text),
            _ => Err(Stop::Unsure),
        }
    }

    fn trees_in(&mut self, name: &str) -> Ran<&mut Vec<Tree>> {
        match self.slot(name)? {
            Value::Nodes(trees) => Ok(trees),
            _ => Err(Stop::Unsure),
        }
    }

    /// `match name:` changing whichever case it holds through `ref`, the text and the child
    /// worked out only in the cases that use them, as the program does.
    fn reshape(&mut self, name: &str, text: &Expr, child: &Expr) -> Ran<()> {
        let Value::Node(tree) = self.get(name)? else { return Err(Stop::Unsure) };
        let tree = match tree.clone() {
            Tree::Number(value) => Tree::Number(checked(value.checked_add(1))?),
            Tree::Leaf(mut held) => {
                held.push_str(&self.text_of(text)?);
                Tree::Leaf(held)
            }
            Tree::Branch(mut label, mut children) => {
                label.push_str(&self.text_of(text)?);
                children.push(self.tree_of(child)?);
                Tree::Branch(label, children)
            }
            Tree::Tagged(mut tags, mut inner) => {
                tags.push(self.text_of(text)?);
                inner.push(self.tree_of(child)?);
                Tree::Tagged(tags, inner)
            }
        };
        *self.slot(name)? = Value::Node(kept(tree)?);
        Ok(())
    }

    fn float_of(&mut self, expr: &Expr) -> Ran<f64> {
        match self.expr(expr)? {
            Value::Float(bits) => Ok(f64::from_bits(bits)),
            _ => Err(Stop::Unsure),
        }
    }

    fn sized_of(&mut self, expr: &Expr) -> Ran<i128> {
        match self.expr(expr)? {
            Value::Sized(_, n) => Ok(n),
            _ => Err(Stop::Unsure),
        }
    }

    fn get(&self, name: &str) -> Ran<&Value> {
        self.scopes
            .iter()
            .rev()
            .flatten()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value)
            .ok_or(Stop::Unsure)
    }

    fn slot(&mut self, name: &str) -> Ran<&mut Value> {
        self.scopes
            .iter_mut()
            .rev()
            .flatten()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value)
            .ok_or(Stop::Unsure)
    }

    fn bind(&mut self, name: &str, value: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.push((name.to_owned(), value));
        }
    }

    /// Runs a block in a scope of its own.
    fn block(&mut self, body: &[Stmt]) -> Ran<()> {
        self.scopes.push(Vec::new());
        let ran = body.iter().try_for_each(|stmt| self.stmt(stmt));
        self.scopes.pop();
        ran
    }

    /// Runs a loop's body: what a `break` stops, and what a `continue` goes on from.
    fn round(&mut self, body: &[Stmt]) -> Ran<bool> {
        match self.block(body) {
            Ok(()) | Err(Stop::Continue) => Ok(true),
            Err(Stop::Break) => Ok(false),
            Err(stop) => Err(stop),
        }
    }

    fn stmt(&mut self, stmt: &Stmt) -> Ran<()> {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let value = self.expr(value)?;
                self.bind(name, value);
            }
            Stmt::Assign { place, value, add } => self.assign(place, value, *add)?,
            Stmt::Append(name, value) => {
                let value = int(&self.expr(value)?)?;
                if let Value::Ints(items) = self.slot(name)? {
                    items.push(value);
                }
            }
            Stmt::NodesAppend(name, node) => {
                let node = self.tree_of(node)?;
                let trees = self.trees_in(name)?;
                trees.push(node);
                if trees.len() > MOST_TREE_NODES {
                    return Err(Stop::Unsure);
                }
            }
            Stmt::NodesPop(name) => {
                self.trees_in(name)?.pop();
            }
            Stmt::NodesSet(name, at, node) => {
                if !self.trees_in(name)?.is_empty() {
                    let at = int(&self.expr(at)?)?;
                    let len = i64::try_from(self.trees_in(name)?.len()).map_err(|_| Stop::Unsure)?;
                    let at = usize::try_from(floor_rem(at, len)?).map_err(|_| Stop::Unsure)?;
                    let node = self.tree_of(node)?;
                    let trees = self.trees_in(name)?;
                    *trees.get_mut(at).ok_or(Stop::Unsure)? = node;
                }
            }
            Stmt::Reshape(name, text, child) => self.reshape(name, text, child)?,
            Stmt::Rename(name, text, left) => self.rename(name, text, left)?,
            Stmt::LocalRef(name, place, body) => self.local_ref(name, place, body)?,
            Stmt::CopiedNested(shape, _, _, value) => {
                let value = int(&self.expr(value)?)?;
                // What `given` holds, which the change to the copy mustn't reach, and the copy.
                let (given, copy) = match shape {
                    Nested::DictOfUnits => {
                        let rest = "\"b\": Unit(tags=[], hp=2)}";
                        (
                            format!("{{\"a\": Unit(tags=[1], hp=1), {rest}"),
                            format!("{{\"a\": Unit(tags=[1, {value}], hp=1), {rest}"),
                        )
                    }
                    Nested::ArmyOfUnits => (
                        "Army(units={\"a\": Unit(tags=[1], hp=1)})".to_owned(),
                        format!("Army(units={{\"a\": Unit(tags=[1, {value}], hp=1)}})"),
                    ),
                    Nested::DictOfDicts => (
                        "{\"a\": {\"x\": 1}, \"b\": {}}".to_owned(),
                        format!("{{\"a\": {{\"x\": {}}}, \"b\": {{}}}}", checked(value.checked_add(1))?),
                    ),
                    Nested::ListOfDicts => (
                        "[{\"x\": 1}, {\"y\": 2}]".to_owned(),
                        format!("[{{\"x\": {}}}, {{\"y\": 2}}]", checked(value.checked_add(1))?),
                    ),
                    Nested::DictOfUnitLists => (
                        "{\"a\": [Unit(tags=[1], hp=1)]}".to_owned(),
                        format!("{{\"a\": [Unit(tags=[1, {value}], hp=1)]}}"),
                    ),
                    Nested::DictOfUnitsHp => (
                        "{\"a\": Unit(tags=[1], hp=1)}".to_owned(),
                        format!("{{\"a\": Unit(tags=[1], hp={})}}", checked(value.checked_add(1))?),
                    ),
                    Nested::DictOfIntLists => (
                        "{\"a\": [1, 2], \"b\": []}".to_owned(),
                        format!("{{\"a\": [1, 2, {value}], \"b\": []}}"),
                    ),
                    Nested::ReplacedInCopy => {
                        let rest = "\"b\": Unit(tags=[], hp=2)}";
                        (
                            format!("{{\"a\": Unit(tags=[1], hp=1), {rest}"),
                            format!("{{\"a\": Unit(tags=[{value}], hp={value}), {rest}"),
                        )
                    }
                };
                let _ = writeln!(self.out, "{given} {copy}");
            }
            Stmt::UnionCall(_, _, member, text) => {
                let text = self.text_of(text)?;
                let log = ["lite", "pg", "mem"].get(*member).ok_or(Stop::Unsure)?;
                let length = i64::try_from(text.chars().count()).map_err(|_| Stop::Unsure)?;
                let times = i64::try_from(*member + 1).map_err(|_| Stop::Unsure)?;
                let _ = writeln!(self.out, "{log} {}", checked(length.checked_mul(times))?);
            }
            Stmt::PlanApply(_, spots, xs, _) => {
                let mut world = Vec::with_capacity(spots.len());
                for (label, x) in spots {
                    world.push(((*label).to_owned(), int(&self.expr(x)?)?));
                }
                let Value::Ints(items) = self.expr(xs)? else { return Err(Stop::Unsure) };
                let len = i64::try_from(world.len()).map_err(|_| Stop::Unsure)?;
                // Each change is planned from the world before any is applied.
                let mut changes = Vec::with_capacity(items.len());
                for x in &items {
                    let at = usize::try_from(floor_rem(*x, len)?).map_err(|_| Stop::Unsure)?;
                    changes.push((at, *x, floor_rem(*x, 2)? == 0));
                }
                for (at, x, moved) in changes {
                    let spot = world.get_mut(at).ok_or(Stop::Unsure)?;
                    if moved {
                        spot.1 = checked(spot.1.checked_add(x))?;
                    } else {
                        let _ = write!(spot.0, "<{x}>");
                    }
                }
                let shown: Vec<String> =
                    world.iter().map(|(label, x)| format!("Spot(label=\"{label}\", x={x})")).collect();
                let _ = writeln!(self.out, "[{}] {}", shown.join(", "), items.len());
            }
            Stmt::Shadowed(_, by) => {
                let by = int(&self.expr(by)?)?;
                let change = checked(by.checked_mul(2))?;
                let bumped = checked(change.checked_add(1))?;
                let tally = checked(bumped.checked_sub(by))?;
                let _ = writeln!(self.out, "{change} {bumped} {tally}");
            }
            Stmt::LentRef(list, _) => {
                if let Value::Ints(items) = self.slot(list)?
                    && items.len() < 64
                {
                    let own = items.clone();
                    items.extend_from_slice(&own);
                    let line = format!("{} {}", shown(&Value::Ints(items.clone())), shown(&Value::Ints(own)));
                    let _ = writeln!(self.out, "{line}");
                }
            }
            Stmt::LentItem(_, _, items) => {
                let Value::Ints(mut first) = self.expr(items)? else { return Err(Stop::Unsure) };
                if first.len() < 64 {
                    let own = first.clone();
                    first.extend_from_slice(&own);
                    let rows = format!("[{}, [7]]", shown(&Value::Ints(first)));
                    let _ = writeln!(self.out, "{rows} {}", shown(&Value::Ints(own)));
                }
            }
            Stmt::SelfField(_) => {}
            Stmt::GrownBy(holder, _) => {
                if let Value::Holder(items, tag) = self.slot(holder)?
                    && items.len() < 64
                {
                    items.push(*tag);
                    let own = items.clone();
                    items.extend_from_slice(&own);
                    let shown_holder = shown(self.get(holder)?);
                    let _ = writeln!(self.out, "{shown_holder} {}", shown(&Value::Ints(own)));
                }
            }
            Stmt::Pop(name) => {
                if let Value::Ints(items) = self.slot(name)? {
                    items.pop();
                }
            }
            Stmt::If(condition, then, otherwise) => {
                if self.expr(condition)? == Value::Bool(true) {
                    self.block(then)?;
                } else {
                    self.block(otherwise)?;
                }
            }
            Stmt::Range(name, count, body) => {
                let count = int(&self.expr(count)?)?;
                for at in 0..count.max(0) {
                    self.scopes.push(vec![(name.clone(), Value::Int(at))]);
                    let more = self.round(body);
                    self.scopes.pop();
                    if !more? {
                        break;
                    }
                }
            }
            Stmt::Each(name, list, body) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                for item in items {
                    self.scopes.push(vec![(name.clone(), Value::Int(item))]);
                    let more = self.round(body);
                    self.scopes.pop();
                    if !more? {
                        break;
                    }
                }
            }
            Stmt::Indexed(name, list, body) => {
                let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                // The range is worked out once, from the length the list had then.
                let rounds = items.len();
                for at in 0..rounds {
                    let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                    let item = items.get(at).copied().ok_or(Stop::Failed)?;
                    let _ = writeln!(self.out, "{item}");
                    let at = i64::try_from(at).map_err(|_| Stop::Unsure)?;
                    self.scopes.push(vec![(name.clone(), Value::Int(at))]);
                    let more = self.round(body);
                    self.scopes.pop();
                    if !more? {
                        break;
                    }
                }
            }
            Stmt::SetKey(name, key, value, add) => {
                let Value::Counts(pairs) = self.get(name)? else { return Err(Stop::Unsure) };
                let old = pairs.iter().find(|(held, _)| held == key).map(|(_, n)| *n);
                // The place first: `+=` on a key that isn't there fails before the value's worked out.
                if *add && old.is_none() {
                    return Err(Stop::Failed);
                }
                let value = int(&self.expr(value)?)?;
                let new = if *add { checked(old.unwrap_or(0).checked_add(value))? } else { value };
                if let Value::Counts(pairs) = self.slot(name)? {
                    match pairs.iter_mut().find(|(held, _)| held == key) {
                        Some((_, held)) => *held = new,
                        None => pairs.push(((*key).to_owned(), new)),
                    }
                }
            }
            Stmt::RemoveKey(name, key) => {
                if let Value::Counts(pairs) = self.slot(name)? {
                    pairs.retain(|(held, _)| held != key);
                }
            }
            Stmt::Buffered(name, op) => self.buffered(name, op)?,
            Stmt::HolderAppend(name, value) => {
                let value = int(&self.expr(value)?)?;
                if let Value::Holder(items, _) = self.slot(name)? {
                    items.push(value);
                }
            }
            Stmt::HolderTag(name, value) => {
                let Value::Holder(_, old) = self.get(name)? else { return Err(Stop::Unsure) };
                let old = *old;
                let value = int(&self.expr(value)?)?;
                let new = checked(old.checked_add(value))?;
                if let Value::Holder(_, tag) = self.slot(name)? {
                    *tag = new;
                }
            }
            Stmt::PutList(name, key, list) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                if let Value::Lists(pairs) = self.slot(name)? {
                    match pairs.iter_mut().find(|(held, _)| held == key) {
                        Some((_, held)) => *held = items,
                        None => pairs.push(((*key).to_owned(), items)),
                    }
                }
            }
            Stmt::ListAppend(name, key, value) => {
                let Value::Lists(pairs) = self.get(name)? else { return Err(Stop::Unsure) };
                if pairs.iter().any(|(held, _)| held == key) {
                    let value = int(&self.expr(value)?)?;
                    if let Value::Lists(pairs) = self.slot(name)?
                        && let Some((_, items)) = pairs.iter_mut().find(|(held, _)| held == key)
                    {
                        items.push(value);
                    }
                }
            }
            Stmt::GrowAll(list, by) => {
                let by = int(&self.expr(by)?)?;
                if let Value::Ints(items) = self.slot(list)? {
                    for item in items.iter_mut() {
                        *item = checked(item.checked_add(by))?;
                    }
                }
            }
            Stmt::EachRefRead(_, list, at) => {
                let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                // Each read sees the items changed so far.
                for place in 0..items.len() {
                    let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                    let item = items.get(place).copied().ok_or(Stop::Unsure)?;
                    let len = i64::try_from(items.len()).map_err(|_| Stop::Unsure)?;
                    let read = at.unwrap_or(item);
                    let read =
                        items.get(usize::try_from(floor_rem(read, len)?).map_err(|_| Stop::Unsure)?).copied();
                    let changed = checked(item.checked_add(read.ok_or(Stop::Unsure)?))?;
                    if let Value::Ints(items) = self.slot(list)?
                        && let Some(held) = items.get_mut(place)
                    {
                        *held = changed;
                    }
                }
            }
            Stmt::EachRefThenRead(_, list, at) => {
                let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                for place in 0..items.len() {
                    let changed = match self.slot(list)? {
                        Value::Ints(items) => {
                            let held = items.get_mut(place).ok_or(Stop::Unsure)?;
                            *held = checked(held.checked_add(1))?;
                            items.clone()
                        }
                        _ => return Err(Stop::Unsure),
                    };
                    let len = i64::try_from(changed.len()).map_err(|_| Stop::Unsure)?;
                    let read = usize::try_from(floor_rem(*at, len)?).map_err(|_| Stop::Unsure)?;
                    let _ = writeln!(self.out, "{}", changed.get(read).copied().ok_or(Stop::Unsure)?);
                }
            }
            Stmt::TableRound { xs, removed, read, by, .. } => {
                let mut slots: Vec<Option<i64>> = Vec::with_capacity(xs.len());
                for x in xs {
                    slots.push(Some(int(&self.expr(x)?)?));
                }
                if let Some(slot) = removed.and_then(|at| slots.get_mut(at)) {
                    *slot = None;
                }
                for place in 0..slots.len() {
                    let Some(Some(x)) = slots.get(place).copied() else { continue };
                    let by = int(&self.expr(by)?)?;
                    let changed = checked(x.checked_add(by))?;
                    if let Some(slot) = slots.get_mut(place) {
                        *slot = Some(changed);
                    }
                    // Read live, by `Id`, after the change through the `ref`.
                    let read = slots.get(*read).copied().flatten().ok_or(Stop::Unsure)?;
                    let _ = writeln!(self.out, "{read}");
                }
                let points: Vec<String> =
                    slots.iter().flatten().map(|x| format!("Point(x={x}, y=0)")).collect();
                let _ = writeln!(self.out, "Table([{}])", points.join(", "));
            }
            Stmt::ShareOuter(name, _) => {
                self.outers.push(0);
                self.bind(name, Value::Outer(self.outers.len() - 1));
            }
            Stmt::OuterBump(name, by, refuse) => {
                let by = int(&self.expr(by)?)?;
                let refuse = self.expr(refuse)? == Value::Bool(true);
                let Value::Outer(at) = self.get(name)? else { return Err(Stop::Unsure) };
                let at = *at;
                let n = self.outers.get(at).copied().ok_or(Stop::Unsure)?;
                let bumped = checked(n.checked_add(by))?;
                if refuse {
                    self.out.push_str("refused\n");
                } else if let Some(held) = self.outers.get_mut(at) {
                    *held = bumped;
                }
            }
            Stmt::PrintOuter(name) => {
                let Value::Outer(at) = self.get(name)? else { return Err(Stop::Unsure) };
                let n = self.outers.get(*at).copied().ok_or(Stop::Unsure)?;
                let _ = writeln!(self.out, "{n}");
            }
            Stmt::Summed(limit, stop) => self.summed(limit, stop)?,
            Stmt::ZipSummed(zipped) => self.zip_summed(zipped)?,
            Stmt::Arrowed(how, n, limit) => self.arrowed(how, n, *limit)?,
            Stmt::Guarded { code, refuse, crash, noted } => self.guarded(*code, *refuse, *crash, *noted)?,
            Stmt::GuardAll(items) => self.guard_all(items),
            Stmt::Left { depth, rounds, at, how, .. } => self.left(*depth, *rounds, *at, *how)?,
            Stmt::Turned(turning) => self.turned(turning)?,
            Stmt::Filed(filing) => self.filed(filing),
            Stmt::Generic(generic) => self.generic(generic),
            Stmt::Stuck(sticking) => self.stuck(*sticking)?,
            Stmt::Interfaced(interfacing) => self.interfaced(interfacing)?,
            Stmt::LockedTogether(locking) => self.locked_together(locking)?,
            Stmt::Arrayed(arraying) => self.arrayed(arraying)?,
            Stmt::Popped(popping) => self.popped(popping),
            Stmt::Sized(sizing) => self.sized(sizing)?,
            Stmt::Generated(generating) => self.generated(generating),
            Stmt::Ruled(ruling) => self.ruled(ruling)?,
            Stmt::Saved(saving) => {
                for line in super::saving::printed(saving) {
                    let _ = writeln!(self.out, "{line}");
                }
            }
            Stmt::Stored(storing) => {
                for line in super::storing::printed(storing) {
                    let _ = writeln!(self.out, "{line}");
                }
            }
            Stmt::Crewed(crew) => {
                for line in super::crews::printed(crew) {
                    let _ = writeln!(self.out, "{line}");
                }
            }
            Stmt::Pooled(pooling) => {
                for line in super::pools::printed(pooling) {
                    let _ = writeln!(self.out, "{line}");
                }
            }
            Stmt::Messaged(messaging) => {
                for line in super::messaging::printed(messaging) {
                    let _ = writeln!(self.out, "{line}");
                }
            }
            Stmt::Opened(opening) => {
                let (lines, stops) = super::opening::printed(opening);
                for line in lines {
                    let _ = writeln!(self.out, "{line}");
                }
                if stops {
                    return Err(Stop::Failed);
                }
            }
            Stmt::Ordered(in_order) => self.ordered(in_order)?,
            Stmt::Owned(owning) => self.owned(owning)?,
            Stmt::Streamed(streaming) => self.streamed(streaming)?,
            Stmt::Aliased(aliasing) => self.aliased(aliasing)?,
            Stmt::Constants(constants) => self.constants(constants)?,
            Stmt::FieldCalls(calls) => self.fielded(calls)?,
            Stmt::Yielded(yielding) => self.yielded(yielding),
            Stmt::Woken(waking) => self.woken(waking),
            Stmt::Swapped(swapping) => self.swapped(swapping),
            Stmt::Shelved(shelving) => self.shelved(shelving),
            Stmt::Listened(listening) => self.listened(listening),
            Stmt::RefCased(casing) => self.ref_cased(*casing),
            Stmt::Captured(capturing) => self.captured(*capturing),
            Stmt::Narrowed(narrowing) => self.narrowed(*narrowing),
            Stmt::FloatKeys { held, values, probe, asked, pair, .. } => {
                let line = float_keys(*held, values, *probe, *asked, *pair);
                self.out.push_str(&line);
            }
            Stmt::EachRef(_, list, by) => {
                let Value::Ints(items) = self.get(list)? else { return Err(Stop::Unsure) };
                for at in 0..items.len() {
                    let by = int(&self.expr(by)?)?;
                    if let Value::Ints(items) = self.slot(list)?
                        && let Some(item) = items.get_mut(at)
                    {
                        *item = checked(item.checked_add(by))?;
                    }
                }
            }
            Stmt::While(counter, limit, body) => {
                self.bind(counter, Value::Int(0));
                while int(self.get(counter)?)? < *limit {
                    let next = int(self.get(counter)?)? + 1;
                    *self.slot(counter)? = Value::Int(next);
                    if !self.round(body)? {
                        break;
                    }
                }
            }
            Stmt::Break(condition) => {
                if self.expr(condition)? == Value::Bool(true) {
                    return Err(Stop::Break);
                }
            }
            Stmt::Continue(condition) => {
                if self.expr(condition)? == Value::Bool(true) {
                    return Err(Stop::Continue);
                }
            }
            Stmt::Moved(name, by) => {
                let by = int(&self.expr(by)?)?;
                if let Value::Point(x, _) = self.slot(name)? {
                    *x = checked(x.checked_add(by))?;
                }
            }
            Stmt::Grow(name, by) => {
                if let Value::Shape(Case::Circle | Case::Square, number) = self.get(name)? {
                    let number = *number;
                    let by = int(&self.expr(by)?)?;
                    let grown = checked(number.checked_add(by))?;
                    if let Value::Shape(_, held) = self.slot(name)? {
                        *held = grown;
                    }
                }
            }
            Stmt::Share(name, x, y) => {
                let (x, y) = (int(&self.expr(x)?)?, int(&self.expr(y)?)?);
                self.heap.push((x, y));
                self.bind(name, Value::Shared(self.heap.len() - 1));
            }
            Stmt::Change(name, by, refuse) => self.change(name, by, refuse)?,
            Stmt::Tally(name, items) => self.tally(name, items)?,
            Stmt::PrintShared(name) => match self.get(name)? {
                Value::Shared(at) => {
                    let (x, y) = self.heap.get(*at).copied().ok_or(Stop::Unsure)?;
                    let _ = writeln!(self.out, "Point(x={x}, y={y})");
                }
                Value::Bag(at) => {
                    let shown = self.bags.get(*at).ok_or(Stop::Unsure)?.shown();
                    let _ = writeln!(self.out, "{shown}");
                }
                _ => return Err(Stop::Unsure),
            },
            Stmt::ShareBag(name, items, counts) => {
                let Value::Ints(items) = self.expr(items)? else { return Err(Stop::Unsure) };
                let counts = counts.iter().map(|(key, n)| ((*key).to_owned(), *n)).collect();
                self.bags.push(Bag { items, counts });
                self.bind(name, Value::Bag(self.bags.len() - 1));
            }
            Stmt::Locked { name, ops, refuse, .. } => {
                let refuse = self.expr(refuse)? == Value::Bool(true);
                let Value::Bag(at) = self.get(name)? else { return Err(Stop::Unsure) };
                let at = *at;
                let bag = self.bags.get(at).cloned().ok_or(Stop::Unsure)?;
                // A crash inside the lock stops the program; a refusal undoes every change.
                let changed = bag.changed(ops, 0).ok_or(Stop::Failed)?;
                if refuse {
                    self.out.push_str("refused\n");
                } else if let Some(held) = self.bags.get_mut(at) {
                    *held = changed;
                }
            }
            Stmt::Sent { name, ops, items, .. } => {
                let Value::Ints(items) = self.expr(items)? else { return Err(Stop::Unsure) };
                let Value::Bag(at) = self.get(name)? else { return Err(Stop::Unsure) };
                let at = *at;
                for number in items {
                    let bag = self.bags.get(at).cloned().ok_or(Stop::Unsure)?;
                    // A round that crashes undoes its own changes and ends the task.
                    let Some(changed) = bag.changed(ops, number) else {
                        self.out.push_str("crashed\n");
                        break;
                    };
                    if let Some(held) = self.bags.get_mut(at) {
                        *held = changed;
                    }
                }
            }
            Stmt::EachCounted(name, limit, body) => {
                let limit = int(&self.expr(limit)?)?;
                for at in 0..limit.max(0) {
                    let item = checked(at.checked_mul(3))?;
                    self.scopes.push(vec![(name.clone(), Value::Int(item))]);
                    let more = self.round(body);
                    self.scopes.pop();
                    if !more? {
                        break;
                    }
                }
            }
            Stmt::With(name, start, body) => {
                let start = int(&self.expr(start)?)?;
                let _ = writeln!(self.out, "open {start}");
                self.scopes.push(vec![(name.clone(), Value::Tally(start))]);
                let ran = self.block(body);
                self.scopes.pop();
                // The block's end, however it comes, is the source's: a crash runs its `else:`.
                let ending = if ran == Err(Stop::Failed) { "undone" } else { "closed" };
                if ran != Err(Stop::Unsure) {
                    let _ = writeln!(self.out, "{ending} {start}");
                }
                ran?;
            }
            Stmt::Print(values) => {
                let mut line = Vec::new();
                for (value, _) in values {
                    line.push(shown(&self.expr(value)?));
                }
                let _ = writeln!(self.out, "{}", line.join(" "));
                if self.out.len() > 1 << 22 {
                    return Err(Stop::Unsure);
                }
            }
        }
        Ok(())
    }

    /// `change(name, by, refuse)`: under the lock, `x` gains `by` and `y` doubles, and a refusal
    /// undoes both. Overflowing stops the program, which ends there either way.
    fn change(&mut self, name: &str, by: &Expr, refuse: &Expr) -> Ran<()> {
        let by = int(&self.expr(by)?)?;
        let refuse = self.expr(refuse)? == Value::Bool(true);
        let Value::Shared(at) = self.get(name)? else { return Err(Stop::Unsure) };
        let at = *at;
        let (x, y) = self.heap.get(at).copied().ok_or(Stop::Unsure)?;
        let changed = (checked(x.checked_add(by))?, checked(y.checked_mul(2))?);
        if refuse {
            self.out.push_str("refused\n");
        } else if let Some(held) = self.heap.get_mut(at) {
            *held = changed;
        }
        Ok(())
    }

    /// `tally(name, items=list)`: each item in turn, under its own lock, counted in `y` and added
    /// to `x`. One that overflows crashes the task, undoing that lock and leaving the rest.
    fn tally(&mut self, name: &str, items: &Expr) -> Ran<()> {
        let Value::Ints(items) = self.expr(items)? else { return Err(Stop::Unsure) };
        let Value::Shared(at) = self.get(name)? else { return Err(Stop::Unsure) };
        let at = *at;
        for item in items {
            let (x, y) = self.heap.get(at).copied().ok_or(Stop::Unsure)?;
            let (Some(y), Some(x)) = (y.checked_add(1), x.checked_add(item)) else {
                self.out.push_str("crashed\n");
                return Ok(());
            };
            if let Some(held) = self.heap.get_mut(at) {
                *held = (x, y);
            }
        }
        Ok(())
    }

    /// `guarded`'s `lend x except:` cleanup, after a block that failed or crashed: the note it
    /// leaves when it fails, with the rest of it skipped.
    fn cleanup(&mut self, code: i64, noted: bool) -> Ran<Option<i64>> {
        let _ = writeln!(self.out, "unguard {code}");
        if floor_rem(code, 3)? == 0 {
            return Ok(Some(checked(code.checked_add(1000))?));
        }
        if noted && code > 20 {
            return Ok(Some(code));
        }
        let _ = writeln!(self.out, "unguarded {code}");
        Ok(None)
    }

    /// `guarded_shown(code, refuse, crash, noted)`: the block refuses, crashes indexing `[1, 2]`, or
    /// gives back `code`.
    fn guarded(&mut self, code: i64, refuse: bool, crash: bool, noted: bool) -> Ran<()> {
        let _ = writeln!(self.out, "guard {code}\ninside {code}");
        if refuse {
            let left = match self.cleanup(code, noted)? {
                Some(note) => format!("[\"Refused(code={note})\"]"),
                None => "[]".to_owned(),
            };
            let _ = writeln!(self.out, "refused {code} {left}");
        } else if crash && !(-2..2).contains(&code) {
            self.cleanup(code, noted)?;
            return Err(Stop::Failed);
        } else {
            let _ = writeln!(self.out, "released {code}\n{code}");
        }
        Ok(())
    }

    /// `guard_all(items)`: the task's `with guarded(item)` for each item until one crashes, on 5,
    /// or fails, on 7, either running the cleanup and printing `stopped`, or, with
    /// `DISTINCT_STOPS`, `crashed` or `refused`: the task's own failure, which its block ends with
    /// whatever it was still sending.
    fn guard_all(&mut self, items: &[i64]) {
        for item in items {
            let _ = writeln!(self.out, "guard {item}\ntask {item}");
            if *item == 5 || *item == 7 {
                // The task's crash or failure is the block's, whatever the cleanup does.
                let _ = self.cleanup(*item, false);
                let word = match (super::generate::DISTINCT_STOPS, *item) {
                    (false, _) => "stopped",
                    (true, 5) => "crashed",
                    (true, _) => "refused",
                };
                let _ = writeln!(self.out, "{word}");
                return;
            }
            let _ = writeln!(self.out, "released {item}");
        }
    }

    /// `left{id}(stdout)`: each round opens the wards outermost first and closes them innermost
    /// first, the code after `lend` for a block that finished or was left by `return`, `break` or
    /// `continue`, and `lend x except:` for one that failed or crashed.
    fn left(&mut self, depth: usize, rounds: i64, at: i64, how: Leave) -> Ran<()> {
        let names = super::source::WARDS.get(..depth).unwrap_or(&super::source::WARDS);
        let mut total = 0_i64;
        for i in 0..rounds {
            for name in names {
                let _ = writeln!(self.out, "open {name}");
            }
            let _ = writeln!(self.out, "in {} {i}", names.join(" "));
            let ending = if how.fails() || how == Leave::Crash { "undo" } else { "close" };
            // An `except` that carries on handles every round from `at`, where the call fails.
            let caught = match how {
                Leave::ExceptReturn | Leave::ExceptArms => i == at,
                Leave::ExceptContinue => i >= at,
                _ => false,
            };
            if caught {
                let _ = writeln!(self.out, "caught {i}");
                for name in names.iter().rev() {
                    let _ = writeln!(self.out, "close {name}");
                }
                match how {
                    Leave::ExceptReturn => return Ok(()),
                    Leave::ExceptArms => break,
                    _ => continue,
                }
            }
            if i == at && !matches!(how, Leave::ExceptValue | Leave::ExceptContinue) {
                for name in names.iter().rev() {
                    let _ = writeln!(self.out, "{ending} {name}");
                }
                match how {
                    Leave::Crash => return Err(Stop::Failed),
                    _ if how.fails() => {
                        let _ = writeln!(self.out, "refused {i}");
                        return Ok(());
                    }
                    Leave::ElseReturn | Leave::NestedReturn | Leave::Return => return Ok(()),
                    Leave::ElseBreak | Leave::Break => break,
                    _ => continue,
                }
            }
            let doubled = matches!(
                how,
                Leave::QuestionInside | Leave::QuestionNestedInner | Leave::QuestionNestedOuter
            );
            let kept = if how == Leave::ExceptValue && i >= at {
                -1
            } else if doubled {
                checked(i.checked_mul(2))?
            } else {
                i
            };
            total = checked(total.checked_add(kept))?;
            let _ = writeln!(self.out, "kept {i}");
            for name in names.iter().rev() {
                let _ = writeln!(self.out, "close {name}");
            }
            let _ = writeln!(self.out, "round {i}");
        }
        let _ = writeln!(self.out, "total {total}");
        Ok(())
    }

    /// What `streamed{id}` prints. Its items are read in order, as one thread would work them out,
    /// and the first item that fails, among those read, stops the program, saying why: an item a
    /// `break` never reaches doesn't. A `sum` or a list reads every item before printing.
    fn streamed(&mut self, streaming: &Streaming) -> Ran<()> {
        let worked = |item: i64| -> Result<i64, String> {
            match streaming.map {
                StreamMap::Scaled(by, plus) => {
                    item.checked_mul(by).and_then(|scaled| scaled.checked_add(plus)).ok_or_else(String::new)
                }
                StreamMap::Divided(zero) => {
                    let by = item - zero;
                    if by == 0 {
                        Err("division by zero".to_owned())
                    } else {
                        floor_div(100, by).map_err(|_| String::new())
                    }
                }
                StreamMap::Indexed => usize::try_from(item)
                    .ok()
                    .and_then(|at| YS.get(at).copied())
                    .ok_or_else(|| format!("index {item} is out of range for length {}", YS.len())),
            }
        };
        let mut values = Vec::new();
        let limit = match streaming.read {
            StreamRead::Loop(Some(stop)) => usize::try_from(stop).unwrap_or(usize::MAX),
            _ => usize::MAX,
        };
        for &item in streaming.items.iter().take(limit) {
            match worked(item) {
                Ok(value) => {
                    if matches!(streaming.read, StreamRead::Loop(_)) {
                        let _ = writeln!(self.out, "streamed {value}");
                    }
                    values.push(value);
                }
                Err(said) if said.is_empty() => return Err(Stop::Unsure),
                Err(said) => {
                    self.said = Some(said);
                    return Err(Stop::Failed);
                }
            }
        }
        match streaming.read {
            StreamRead::Loop(_) => {
                let _ = writeln!(self.out, "read {}", values.len());
            }
            StreamRead::Sum => {
                let total = values
                    .iter()
                    .try_fold(0_i64, |total, value| total.checked_add(*value))
                    .ok_or(Stop::Unsure)?;
                let _ = writeln!(self.out, "sum {total}");
            }
            StreamRead::Listed => {
                let plus: Option<Vec<i64>> = values.iter().map(|value| value.checked_add(1)).collect();
                let _ = writeln!(self.out, "{}", shown(&Value::Ints(plus.ok_or(Stop::Unsure)?)));
            }
        }
        Ok(())
    }

    /// What `woken{id}` prints: each answer before the task's crash or failure, then how its
    /// block ended, if it did.
    fn woken(&mut self, waking: &Waking) {
        let Waking { count, at, failing, rounds, .. } = *waking;
        for _ in 0..rounds {
            for number in 0..count.min(at.max(0)) {
                let _ = writeln!(self.out, "got {}", number * 2);
            }
            if (0..count).contains(&at) {
                if failing {
                    let _ = writeln!(self.out, "refused {at}");
                } else {
                    self.out.push_str("crashed\n");
                }
            }
        }
    }

    /// What `swaps{id}` prints: the struct given as it was, and the other one with the store,
    /// whatever the lookup searched.
    fn swapped(&mut self, swapping: &Swapping) {
        let Swapping { id, base, other_base, field, key, fresh, called, kept, owned, rounds } = *swapping;
        let key = SWAP_KEYS.get(key).copied().unwrap_or("a");
        let desk = |at: i64| -> Desk {
            (
                vec![("a".to_owned(), at), ("b".to_owned(), at + 1)],
                vec![("a".to_owned(), vec![at, 1]), ("b".to_owned(), vec![at + 2])],
            )
        };
        for turn in 0..rounds {
            let given = desk(base + turn);
            let mut after = if called {
                desk(other_base)
            } else {
                (vec![("c".to_owned(), other_base)], vec![("c".to_owned(), vec![other_base])])
            };
            if field == SwapField::Lists {
                let known = given.1.iter().find(|(held, _)| held == key).map(|(_, items)| items.clone());
                put(&mut after.1, key, vec![fresh]);
                match (kept, known) {
                    (true, Some(known)) => put(&mut after.1, "known", known),
                    (true, None) => {}
                    (false, known) => put(&mut after.1, "found", vec![i64::from(known.is_some())]),
                }
            } else {
                let known = given.0.iter().find(|(held, _)| held == key).map(|(_, n)| *n);
                put(&mut after.0, key, fresh);
                match (kept, known) {
                    (true, Some(known)) => put(&mut after.0, "known", known),
                    (true, None) => {}
                    (false, known) => put(&mut after.0, "found", i64::from(known.is_some())),
                }
            }
            if owned {
                let _ = writeln!(self.out, "{} {}", desk_shown(id, &given), desk_shown(id, &after));
            } else {
                let _ = writeln!(self.out, "{}", desk_shown(id, &after));
            }
        }
    }

    /// What `shelved{id}` prints: for each step, `crashed` for a value past 1, and the pouches.
    /// Under a lock, the shelf as it is after each step, with a crash's lock undone whole. Held
    /// by the task alone, what the task made of them, printed by the task when it doesn't crash.
    fn shelved(&mut self, shelving: &Shelving) {
        let Shelving { id, in_list, seen, place, rounds, ref steps } = *shelving;
        let first = (vec![format!("first, {LONG_PIECE}")], seen);
        let mut kept = first.clone();
        let limits = [1, 2];
        for (value, how) in steps {
            let Some(limit) = usize::try_from(*value).ok().and_then(|at| limits.get(at)) else {
                self.out.push_str("crashed\n");
                if place == ShelfPlace::Locked {
                    self.pouches_shown(id, in_list, &kept);
                }
                continue;
            };
            let mut pouch = if place == ShelfPlace::Locked { kept.clone() } else { first.clone() };
            for turn in 0..rounds {
                if how.by_hand() {
                    pouch.0.push(format!("by hand {value} {turn}, {LONG_PIECE}"));
                } else {
                    pouch.0.push(format!("added {value} {turn}, {LONG_PIECE}"));
                    pouch.0.push(format!("again {value} {turn}, {LONG_PIECE}"));
                }
            }
            pouch.1 += limit;
            self.pouches_shown(id, in_list, &pouch);
            if place == ShelfPlace::Locked {
                kept = pouch;
            }
        }
    }

    /// What `listened{id}` prints: what the loop dropping the oldest heard, the last `size` sent,
    /// then each inbox's messages, or `behind` for one that fell behind.
    /// A `match` with `ref` put back where it matched, however it's written, as the language
    /// says: each `ref` name's change goes into the field it names, three `match`es deep
    /// included, and a whole new value too.
    fn ref_cased(&mut self, casing: RefCasing) {
        let RefCasing { way, start, text, .. } = casing;
        let text = REF_TEXTS.get(text).copied().unwrap_or_default();
        let leaf = |name: &str| Named::Leaf(name.to_owned());
        let mut named = match start {
            0 => leaf("a"),
            1 => Named::Pair(Box::new(leaf("a")), Box::new(leaf("b")), "x".to_owned(), Vec::new()),
            _ => Named::Pair(
                Box::new(Named::Pair(Box::new(leaf("a")), Box::new(leaf("b")), "in".to_owned(), Vec::new())),
                Box::new(leaf("c")),
                "out".to_owned(),
                vec!["t".to_owned()],
            ),
        };
        let digest = |named: &Named| {
            let mut out = String::new();
            named.digest(&mut out);
            out
        };
        match way {
            RefWay::Deep => {
                match &mut named {
                    Named::Leaf(name) => name.push_str(text),
                    Named::Pair(left, ..) => match left.as_mut() {
                        Named::Leaf(name) => name.push_str(text),
                        Named::Pair(inner, ..) => {
                            if let Named::Leaf(name) = inner.as_mut() {
                                name.push_str(text);
                            }
                        }
                    },
                }
                let _ = writeln!(self.out, "{}", digest(&named));
            }
            RefWay::NewValue => {
                match &mut named {
                    Named::Leaf(name) => text.clone_into(name),
                    Named::Pair(left, ..) => **left = leaf(text),
                }
                let _ = writeln!(self.out, "{}", digest(&named));
            }
            RefWay::Shadowed | RefWay::IndexMoved => {
                match &mut named {
                    Named::Leaf(name) | Named::Pair(_, _, name, _) => name.push_str(text),
                }
                let moved = if way == RefWay::IndexMoved { " 1" } else { "" };
                let held = |named: &Named| {
                    let mut out = String::new();
                    named.held_digest(&mut out);
                    out
                };
                let _ = writeln!(self.out, "{} {}{moved}", held(&named), held(&leaf("z")));
            }
            RefWay::ForRefWhile | RefWay::ForRefElif | RefWay::ForRefAnd => {
                // The first tag grows, and the `?` after it fails with its length, which the
                // case puts back with the change.
                match &mut named {
                    Named::Leaf(name) => name.push_str(text),
                    Named::Pair(_, _, _, tags) => {
                        if let Some(tag) = tags.first_mut() {
                            tag.push_str(text);
                            let _ = writeln!(self.out, "refused {}", tag.chars().count());
                        }
                    }
                }
                let _ = writeln!(self.out, "{}", digest(&named));
            }
            RefWay::Guarded => {
                match &mut named {
                    Named::Pair(_, _, label, tags) if label.chars().count() < 3 => {
                        label.push_str(text);
                        tags.push(label.clone());
                    }
                    Named::Leaf(name) if !name.is_empty() => name.push_str(text),
                    _ => {}
                }
                let _ = writeln!(self.out, "{}", digest(&named));
            }
        }
    }

    /// What `narrowed{id}` prints: each `grow` changes the member the union holds, whether it's
    /// called on a narrowed name, a `match` case, a `for ref` item, a field or a `ref` parameter, and
    /// a `ref` call taking the union leaves it the other member.
    fn narrowed(&mut self, narrowing: Narrowing) {
        let Narrowing { way, crate_first, n, .. } = narrowing;
        let start = if crate_first { Member::Crate(vec![n]) } else { Member::Heap(n) };
        let other = if crate_first { Member::Heap(n + 1) } else { Member::Crate(vec![n + 1]) };
        let mut shape = start.clone();
        let mut lines: Vec<String> = Vec::new();
        match way {
            NarrowWay::Given => {
                lines.push(shape.grow().to_string());
                lines.push(shape.grow().to_string());
                lines.push(shape.shown());
                shape = other;
                lines.push(shape.grow().to_string());
                lines.push(shape.shown());
            }
            NarrowWay::Is => {
                let grown = shape.grow();
                let read = match &shape {
                    Member::Crate(_) => shape.shown().replacen("crate ", "", 1),
                    Member::Heap(held) => held.to_string(),
                };
                lines.push(format!("{grown} {read}"));
                lines.push(shape.shown());
            }
            NarrowWay::Left => {
                let grown = shape.grow();
                let added = if crate_first { 100 } else { 0 };
                lines.push((grown + added).to_string());
                lines.push(shape.shown());
            }
            NarrowWay::Whole | NarrowWay::Field => {
                lines.push(shape.grow().to_string());
                lines.push(shape.grow().to_string());
                lines.push(shape.shown());
            }
            NarrowWay::Matched => {
                lines.push(shape.grow().to_string());
                lines.push(shape.shown());
            }
            NarrowWay::ForRef => {
                let mut shapes = [start, other];
                for each in &mut shapes {
                    lines.push(each.grow().to_string());
                    if matches!(each, Member::Crate(_)) {
                        lines.push(each.grow().to_string());
                    }
                }
                lines.extend(shapes.iter().map(Member::shown));
            }
            NarrowWay::RefMember => {
                lines.push(shape.grow().to_string());
                lines.push(shape.shown());
                let mut given = start;
                lines.push(given.grow().to_string());
                lines.push(given.shown());
            }
            NarrowWay::RefUnion | NarrowWay::RefUnionGrown => {
                let grown = shape.grow();
                shape = shape.swapped(grown);
                lines.push(grown.to_string());
                if way == NarrowWay::RefUnion {
                    lines.push(shape.shown());
                }
                lines.push(shape.grow().to_string());
                lines.push(shape.shown());
            }
        }
        for line in lines {
            let _ = writeln!(self.out, "{line}");
        }
    }

    /// Each name meaning what it does where it's written: the nested function's, the loop's, the
    /// case's or the arrow's own parameter or name, and the function the source calls, not the
    /// caller's parameter of that name.
    fn captured(&mut self, capturing: Capturing) {
        let Capturing { way, n, .. } = capturing;
        let inside = match way {
            CaptureWay::WithDef => {
                let _ = writeln!(self.out, "200 {n}");
                return;
            }
            CaptureWay::SourceCall => {
                let _ = writeln!(self.out, "{n} {}", n + 2);
                return;
            }
            CaptureWay::LockLoop | CaptureWay::LockArrow => 3,
            CaptureWay::LockCase => 4,
            CaptureWay::LockDef => 5,
        };
        let _ = writeln!(self.out, "{inside}\n[1, 2, {n}]");
    }

    fn listened(&mut self, listening: &Listening) {
        let Listening { size, sent, dropping, ref inboxes, .. } = *listening;
        let shown = |from: i64| -> String {
            let heard: Vec<String> = (from..=sent).map(|hit| hit.to_string()).collect();
            format!("[{}]", heard.join(", "))
        };
        if dropping {
            let _ = writeln!(self.out, "listened {}", shown((sent - size + 1).max(1)));
        }
        for looping in inboxes {
            if sent > size {
                self.out.push_str("behind\n");
            } else {
                let _ = writeln!(self.out, "{} {}", if *looping { "looped" } else { "took" }, shown(1));
            }
        }
    }

    /// A `Shelved` statement's list or dict holding its one pouch, as Fennec prints it.
    fn pouches_shown(&mut self, id: usize, in_list: bool, (items, seen): &(Vec<String>, i64)) {
        let texts: Vec<String> = items.iter().map(|text| format!("\"{text}\"")).collect();
        let pouch = format!("Pouch{id}(items=[{}], seen={seen})", texts.join(", "));
        if in_list {
            let _ = writeln!(self.out, "[{pouch}]");
        } else {
            let _ = writeln!(self.out, "{{\"one\": {pouch}}}");
        }
    }

    /// What `yielded{id}` prints: every value as it was yielded, however much was yielded after
    /// it, and the notes again as they come back from the task.
    fn yielded(&mut self, yielding: &Yielding) {
        let Yielding { id, count, widths: [notes, pairs, words] } = *yielding;
        let repeat = |letter: &str, width: i64| letter.repeat(usize::try_from(width).unwrap_or(0));
        let mut got = Vec::new();
        let mut kept_items = vec![0_i64];
        let mut kept_count = 0_i64;
        for at in 0..count {
            got.push(format!(
                "Note{id}(text=\"{at}{}\", items=[{at}, {}], counts={{\"a\": {at}}})",
                repeat("n", notes),
                at + 1
            ));
            kept_items.push(at);
            kept_count += at;
            got.push(format!(
                "Note{id}(text=\"{}\", items={}, counts={{\"k\": {kept_count}}})",
                repeat("k", notes),
                shown(&Value::Ints(kept_items.clone()))
            ));
        }
        for note in &got {
            let _ = writeln!(self.out, "note {note}");
        }
        let firsts: Vec<String> =
            (0..count).map(|at| format!("(\"{at}{}\", [{at}])", repeat("p", pairs))).collect();
        let _ = writeln!(self.out, "pairs [{}]", firsts.join(", "));
        for at in 0..count {
            if at % 2 == 0 {
                let _ = writeln!(self.out, "plain {at}{}", repeat("w", words));
            } else {
                let _ = writeln!(self.out, "many [{at}, {at}]");
            }
        }
        for note in &got {
            let _ = writeln!(self.out, "back {note}");
        }
    }

    /// What `fielded{id}` prints: each call's own print, then the field it added to; then the
    /// struct and the `shared` one.
    fn fielded(&mut self, calls: &FieldCalls) -> Ran<()> {
        let base = calls.base;
        let mut boxed = Boxed { text: format!("t{base}"), count: base, items: vec![base] };
        let mut kept = Boxed { text: "k".to_owned(), count: 0, items: Vec::new() };
        for step in &calls.steps {
            let FieldCall { field, call, plus, n } = *step;
            let target = if call == Calling::Shared { &mut kept } else { &mut boxed };
            let given = match call {
                Calling::Printed | Calling::Shared => {
                    let maker = match field {
                        Field::Count => "told",
                        Field::Text => "said",
                        Field::Items => "given",
                    };
                    let _ = writeln!(self.out, "{maker} {n}");
                    Some(match field {
                        Field::Count => Piece::Count(n),
                        Field::Text => Piece::Text(format!("s{n}")),
                        Field::Items => Piece::Items(vec![n, n]),
                    })
                }
                Calling::Failed if n > 5 => {
                    self.out.push_str("refused\n");
                    None
                }
                Calling::Failed => Some(match field {
                    Field::Count => Piece::Count(n),
                    Field::Text => Piece::Text(format!("c{n}")),
                    Field::Items => Piece::Items(vec![n]),
                }),
                Calling::Copied | Calling::Listed => {
                    let read = target.read(field);
                    let _ = writeln!(self.out, "peek {}", read.shown());
                    Some(read)
                }
                Calling::Closure => Some(target.read(field)),
            };
            if let Some(given) = given {
                target.added(field, plus, n, given).ok_or(Stop::Unsure)?;
            }
            if call != Calling::Shared {
                let _ = writeln!(self.out, "now {}", boxed.read(field).shown());
            }
        }
        let _ = writeln!(self.out, "box {}", boxed.shown(calls.id));
        let _ = writeln!(self.out, "kept {}", kept.shown(calls.id));
        Ok(())
    }

    /// What `constants{id}` prints: each use of the constants sees them as they started, and its
    /// own changes to its copies, whichever thread it runs on.
    fn constants(&mut self, constants: &Constants) -> Ran<()> {
        let Constants { base, reads, main_first, launch_first, .. } = *constants;
        let next = checked(base.checked_add(1))?;
        let used = |n: i64| -> Ran<String> {
            let x = checked(base.checked_add(n))?;
            Ok(format!(
                "{n} 0 {{\"a\": {base}, \"n\": {n}}} [{base}, {next}, {n}] [{n}, {next}, {n}] Point(x={x}, y=1) \
                 {{\"a\": {base}, \"m\": {n}}} 0 [{base}, {next}] {{\"a\": {base}}} Point(x={base}, y=1)"
            ))
        };
        for _ in 0..reads {
            let _ = writeln!(self.out, "2 1 0 {base}");
        }
        if main_first {
            let _ = writeln!(self.out, "main {}", used(1)?);
        }
        let launched = format!("task {}\n", used(2)?);
        let mut parallel = String::new();
        for i in 0..3 {
            let _ = writeln!(parallel, "parallel {}", used(i)?);
        }
        if launch_first {
            self.out.push_str(&launched);
            self.out.push_str(&parallel);
        } else {
            self.out.push_str(&parallel);
            self.out.push_str(&launched);
        }
        let _ = writeln!(self.out, "after {} [{base}, {next}] {{\"a\": {base}}} 0", used(3)?);
        Ok(())
    }

    /// What `aliased{id}` prints: every copy holding only what was written to it, as values do.
    fn aliased(&mut self, aliasing: &Aliasing) -> Ran<()> {
        let mut kept = starting(aliasing.base);
        for step in &aliasing.steps {
            match *step {
                Alias::Copied { from } => {
                    let value = kept.get(from).cloned().ok_or(Stop::Unsure)?;
                    kept.push(value);
                }
                Alias::Part { from, at } => {
                    let part = match kept.get(from).ok_or(Stop::Unsure)? {
                        Kept::Holders(holders) => {
                            holders.get(at).map(|(items, tag)| Kept::Holder(items.clone(), *tag))
                        }
                        Kept::Keepers(holders) => KEYS
                            .get(at)
                            .and_then(|key| holders.iter().find(|(name, _)| name == key))
                            .map(|(_, (items, tag))| Kept::Holder(items.clone(), *tag)),
                        Kept::Holder(items, _) => Some(Kept::Numbers(items.clone())),
                        Kept::Lists(lists) => KEYS
                            .get(at)
                            .and_then(|key| lists.iter().find(|(name, _)| name == key))
                            .map(|(_, items)| Kept::Numbers(items.clone())),
                        Kept::Spots(points) => KEYS
                            .get(at)
                            .and_then(|key| points.iter().find(|(name, _)| name == key))
                            .map(|(_, (x, y))| Kept::Point(*x, *y)),
                        Kept::Table(points) | Kept::Entries(points) => {
                            points.get(at).map(|&(x, y)| Kept::Point(x, y))
                        }
                        Kept::Ledger(points, _) => Some(Kept::Entries(points.clone())),
                        Kept::Rooms(rooms) => rooms.get(at).map(|(counts, _)| Kept::Counts(counts.clone())),
                        Kept::Numbers(_) | Kept::Point(..) | Kept::Text(_) | Kept::Counts(_) => None,
                    };
                    kept.push(part.ok_or(Stop::Unsure)?);
                }
                Alias::Built { from, n } => {
                    let Some(Kept::Numbers(items)) = kept.get(from) else {
                        return Err(Stop::Unsure);
                    };
                    kept.push(Kept::Holder(items.clone(), n));
                }
                Alias::Written { to, how: Writing::Removed, at, n } => {
                    let walked = removed(kept.get_mut(to).ok_or(Stop::Unsure)?, at, n).ok_or(Stop::Unsure)?;
                    let _ = writeln!(self.out, "{walked}");
                }
                Alias::Written { to, how, at, n } => {
                    changed(kept.get_mut(to).ok_or(Stop::Unsure)?, how, at, n).ok_or(Stop::Unsure)?;
                }
                Alias::Looped(looping) => {
                    let value = kept.get_mut(looping.to).ok_or(Stop::Unsure)?;
                    if !looped(value, looping).ok_or(Stop::Unsure)? {
                        self.said = Some("this Id finds nothing in this Table: its item was taken out, or it's from another Table".to_owned());
                        return Err(Stop::Failed);
                    }
                }
                Alias::Tasked { from } => {
                    let given = kept.get(from).ok_or(Stop::Unsure)?;
                    let sums: Option<Vec<i64>> = (0..3).map(|i| handed(given, i)).collect();
                    let _ = writeln!(self.out, "sums {}", shown(&Value::Ints(sums.ok_or(Stop::Unsure)?)));
                }
                Alias::Read { from, how, n } => {
                    let read = read_shown(kept.get(from).ok_or(Stop::Unsure)?, how, n).ok_or(Stop::Unsure)?;
                    match read {
                        Read::Printed(line) => self.out.push_str(&line),
                        Read::Kept(x, y) => kept.push(Kept::Point(x, y)),
                    }
                }
                Alias::Drained { from, at, n } => {
                    let mut last = kept.get(from).cloned().ok_or(Stop::Unsure)?;
                    let values = drained(&mut last, at, n).ok_or(Stop::Unsure)?;
                    let _ = writeln!(self.out, "values {values}");
                }
                Alias::Walked { from } => {
                    let walked = walk_shown(kept.get(from).ok_or(Stop::Unsure)?);
                    if walked.is_empty() {
                        return Err(Stop::Unsure);
                    }
                    let _ = writeln!(self.out, "{walked}");
                }
            }
        }
        let points = |at: usize| match kept.get(at) {
            // Only the two items the program made at first are printed, by their `Id`s.
            Some(Kept::Table(points) | Kept::Ledger(points, _)) => {
                let points: Vec<String> =
                    points.iter().take(2).map(|(x, y)| format!("Point(x={x}, y={y})")).collect();
                Ok(points.join(" "))
            }
            _ => Err(Stop::Unsure),
        };
        let _ = writeln!(self.out, "ids {}", points(TABLE)?);
        let _ = writeln!(self.out, "entries {}", points(LEDGER)?);
        for (n, value) in kept.iter().enumerate() {
            let _ = writeln!(self.out, "{} {}", alias_name(n), kept_shown(aliasing.id, value));
        }
        Ok(())
    }

    /// What `filed{id}` prints: a write through `with fs.create(...)` that fails, passes a failure
    /// up or crashes before its end leaves the file as it was, one that ends replaces it whole, and
    /// a narrowed `Fs` refuses to write.
    fn filed(&mut self, filing: &Filing) {
        let id = filing.id;
        // Both files are cleared at the start.
        let mut files: [Option<String>; 2] = [None, None];
        let shown = |held: &Option<String>| {
            held.as_ref().map_or_else(|| "nothing".to_owned(), |text| format!("[{text}]"))
        };
        for step in &filing.steps {
            let at = usize::from(step.second);
            let whole: String =
                step.pieces.iter().map(|at| FILE_PIECES.get(*at).copied().unwrap_or("")).collect();
            let finishes = step.stop_at >= step.pieces.len();
            let mut lines = Vec::new();
            match step.way {
                FileWay::Saved | FileWay::Questioned | FileWay::Tasked => {
                    if finishes {
                        if let Some(file) = files.get_mut(at) {
                            *file = Some(whole);
                        }
                    } else if step.way == FileWay::Tasked {
                        lines.push("crashed".to_owned());
                    } else {
                        lines.push(format!("failed Halt{id}(at={})", step.stop_at));
                    }
                }
                FileWay::Both => {
                    if finishes {
                        files = [Some(whole.clone()), Some(whole)];
                    } else {
                        lines.push(format!("failed Halt{id}(at={})", step.stop_at));
                    }
                }
                FileWay::Plain => {
                    if let Some(file) = files.get_mut(at) {
                        *file = Some(whole);
                    }
                }
                FileWay::ReadOnly => {
                    let name = if step.second { "g" } else { "f" };
                    // Printed, an `IoError` gives its words: its path, then its reason.
                    lines.push(format!(
                        "refused `d{id}/{name}.txt`: this Fs only reads, so it can't change it"
                    ));
                }
                FileWay::Outside => lines.push("refused outside".to_owned()),
                FileWay::Removed => match files.get_mut(at) {
                    Some(file @ Some(_)) => *file = None,
                    _ => lines.push("not there".to_owned()),
                },
            }
            match step.way {
                FileWay::Both => lines.push(format!("{} {}", shown(&files[0]), shown(&files[1]))),
                FileWay::Outside => {}
                _ => lines.push(shown(files.get(at).unwrap_or(&None))),
            }
            for line in lines {
                let _ = writeln!(self.out, "{line}");
            }
        }
    }

    /// What `owned{id}` prints: each change through a type's own `def []` written back once
    /// through its `def []=`, as far as it got when it fails, and each `ref` form's place changed
    /// where it is, then what each value holds and how often its `def []=` ran.
    fn owned(&mut self, owning: &OwnIndexing) -> Ran<()> {
        let OwnIndexing { id, base, ref steps } = *owning;
        let mut shelved = [OwnPart::new("a", base), OwnPart::new("b", base + 1)];
        let mut keyed = OwnPart::new("k", base);
        let mut room = [OwnPart::new("c", base), OwnPart::new("d", base + 1)];
        let mut rows = [vec![base + 2, base, base + 2], vec![base + 4, base + 3, base + 5]];
        let mut slots = [base, base + 1, base + 2];
        let (mut shelf_writes, mut keyed_writes, mut room_shelf_writes, mut room_writes, mut rows_writes) =
            (0, 0, 0, 0, 0);
        let add = |held: &mut i64, by: i64| -> Ran<()> {
            *held = checked(held.checked_add(by))?;
            Ok(())
        };
        for &(step, at, n) in steps {
            let pair = at % 2;
            let other = 1 - pair;
            let slot = slots.get_mut(at).ok_or(Stop::Unsure)?;
            let part = shelved.get_mut(pair).ok_or(Stop::Unsure)?;
            match step {
                OwnStep::Plus | OwnStep::RefField => add(&mut part.x, n)?,
                OwnStep::Set => part.x = n,
                OwnStep::Moved => part.moved(n)?,
                OwnStep::Appended => part.marks.push(n),
                OwnStep::Shifted | OwnStep::Passed => {
                    if part.shift(n)? {
                        let _ = writeln!(self.out, "failed {n}");
                    } else if step == OwnStep::Passed {
                        add(&mut part.x, 1000)?;
                        shelf_writes += 1;
                    }
                }
                OwnStep::PlusOther => {
                    let read = shelved.get(other).ok_or(Stop::Unsure)?.x;
                    add(&mut shelved.get_mut(pair).ok_or(Stop::Unsure)?.x, read)?;
                }
                OwnStep::Keyed => add(&mut keyed.x, n)?,
                OwnStep::KeyedMoved => keyed.moved(n)?,
                OwnStep::Nested | OwnStep::NestedRef => {
                    add(&mut room.get_mut(pair).ok_or(Stop::Unsure)?.x, n)?;
                }
                OwnStep::NestedMoved => room.get_mut(pair).ok_or(Stop::Unsure)?.moved(n)?,
                OwnStep::Counted => {
                    let row = rows.get(pair).ok_or(Stop::Unsure)?;
                    let count = row.iter().filter(|item| **item == n).count();
                    let got = usize::try_from(n).ok().and_then(|at| row.get(at));
                    let got = got.map_or_else(|| "None".to_owned(), ToString::to_string);
                    let _ = writeln!(self.out, "counted {count} {got} {rows_writes}");
                }
                OwnStep::Sorted => rows.get_mut(pair).ok_or(Stop::Unsure)?.sort_unstable(),
                OwnStep::SortedKey => rows.get_mut(pair).ok_or(Stop::Unsure)?.sort_by(|a, b| b.cmp(a)),
                OwnStep::Removed => rows.get_mut(pair).ok_or(Stop::Unsure)?.retain(|item| *item <= n),
                OwnStep::SlotSet => *slot = n,
                OwnStep::SlotPlus | OwnStep::SlotRef => add(slot, n)?,
                OwnStep::ForRef | OwnStep::ParallelRef => {
                    // The middle position is empty, so `for ref` passes it over.
                    for (place, held) in slots.iter_mut().enumerate() {
                        if place == 1 {
                            continue;
                        }
                        *held = if step == OwnStep::ForRef {
                            checked(held.checked_mul(2).and_then(|twice| twice.checked_add(n)))?
                        } else {
                            checked(held.checked_add(n))?
                        };
                    }
                }
            }
            match step {
                OwnStep::Plus
                | OwnStep::Set
                | OwnStep::Moved
                | OwnStep::Appended
                | OwnStep::Shifted
                | OwnStep::Passed
                | OwnStep::RefField
                | OwnStep::PlusOther => shelf_writes += 1,
                OwnStep::Keyed | OwnStep::KeyedMoved => keyed_writes += 1,
                OwnStep::Nested | OwnStep::NestedMoved | OwnStep::NestedRef => {
                    room_shelf_writes += 1;
                    room_writes += 1;
                }
                OwnStep::Sorted | OwnStep::SortedKey | OwnStep::Removed => rows_writes += 1,
                OwnStep::Counted
                | OwnStep::SlotSet
                | OwnStep::SlotPlus
                | OwnStep::SlotRef
                | OwnStep::ForRef
                | OwnStep::ParallelRef => {}
            }
        }
        let parts = |parts: &[OwnPart]| {
            let parts: Vec<String> = parts.iter().map(|part| part.shown(id)).collect();
            format!("[{}]", parts.join(", "))
        };
        let lists: Vec<String> = rows.iter().map(|row| shown(&Value::Ints(row.clone()))).collect();
        let _ = writeln!(self.out, "shelf {} {shelf_writes}", parts(&shelved));
        let _ = writeln!(self.out, "keyed {{\"k\": {}}} {keyed_writes}", keyed.shown(id));
        let _ = writeln!(self.out, "room {} {room_shelf_writes} {room_writes}", parts(&room));
        let _ = writeln!(self.out, "rows [{}] {rows_writes}", lists.join(", "));
        let _ = writeln!(self.out, "slots {}", shown(&Value::Ints(slots.to_vec())));
        Ok(())
    }

    /// What `generic{id}` prints: generic code given values of one type, each value its text alone,
    /// since generic code can't look inside it. Copies change apart, and a `ref` changes the place
    /// it names.
    fn generic(&mut self, generic: &Genericking) {
        #[derive(Clone)]
        struct Held {
            item: String,
            items: Vec<String>,
        }
        impl Held {
            fn put(&mut self, value: &str) {
                self.items.push(std::mem::replace(&mut self.item, value.to_owned()));
            }
        }
        let id = generic.id;
        let shown =
            |held: &Held| format!("Holder{id}(item={}, items=[{}])", held.item, held.items.join(", "));
        let values = generic.ty.values(id);
        let value = |at: usize| values.get(at).map_or(String::new(), |(_, printed)| printed.clone());
        let mut holder = Held { item: value(0), items: Vec::new() };
        let mut other = Held { item: value(1), items: vec![value(2)] };
        let mut outer = holder.clone();
        let mut pair = [holder.clone(), other.clone()];
        let mut table = other.clone();
        for &(step, which, count) in &generic.steps {
            let v = value(which);
            match step {
                GenericStep::Put => holder.put(&v),
                GenericStep::Grown => holder.items.extend(std::iter::repeat_n(v, count)),
                GenericStep::Swapped => std::mem::swap(&mut holder.item, &mut other.item),
                GenericStep::SwappedItems => std::mem::swap(&mut holder.items, &mut other.items),
                GenericStep::Copied => {
                    let mut copy = holder.clone();
                    copy.put(&v);
                    let _ = writeln!(self.out, "{}", shown(&copy));
                }
                GenericStep::Doubled => {
                    let _ = writeln!(self.out, "[{0}, {0}]", holder.item);
                }
                GenericStep::Oldest => {
                    let _ = writeln!(self.out, "[{}]", holder.items.first().unwrap_or(&holder.item));
                }
                GenericStep::Nested => outer.put(&v),
                GenericStep::ForRef => pair.iter_mut().for_each(|each| each.put(&v)),
                GenericStep::Popped => {
                    let taken = holder.items.pop().unwrap_or_else(|| holder.item.clone());
                    let _ = writeln!(self.out, "[{taken}]");
                }
                GenericStep::DictPut => table.put(&v),
            }
        }
        let _ = writeln!(self.out, "{}", shown(&holder));
        let _ = writeln!(self.out, "{}", shown(&other));
        let _ = writeln!(self.out, "Holder{id}(item={}, items=[])", shown(&outer));
        let _ = writeln!(self.out, "[{}, {}]", shown(&pair[0]), shown(&pair[1]));
        let _ = writeln!(self.out, "{{\"k\": {}}}", shown(&table));
    }

    /// What `stuck{id}` prints: a wait with a time limit times out and the program goes on, and
    /// one without is a deadlock, which stops the program after what it printed.
    fn stuck(&mut self, sticking: Sticking) -> Ran<()> {
        let Sticking { id, way, n } = sticking;
        let _ = writeln!(self.out, "stuck{id} {n}");
        match way {
            StuckWay::TimedReceive => {
                let _ = writeln!(self.out, "received -1");
                return Ok(());
            }
            StuckWay::TimedSend => {
                let _ = writeln!(self.out, "send timed out");
                return Ok(());
            }
            StuckWay::BlockEnd => {
                let _ = writeln!(self.out, "drained{id} waits");
            }
            StuckWay::Received | StuckWay::SentFull | StuckWay::Rounds => {}
        }
        self.said =
            Some("a deadlock: every task is waiting for another, and nothing can wake them".to_owned());
        Err(Stop::Failed)
    }

    /// What `interfaced{id}` prints: each generic call runs the method of the type it's given,
    /// changes the place a `ref` names and leaves a copy apart.
    fn interfaced(&mut self, interfacing: &Interfacing) -> Ran<()> {
        let id = interfacing.id;
        let mut crates = vec![Grower::Crate(vec![1]), Grower::Crate(Vec::new())];
        let mut heaps = vec![Grower::Heap(2), Grower::Heap(5)];
        let (mut crate_one, mut heap) = (Grower::Crate(vec![5]), Grower::Heap(1));
        for &(step, crate_first, n) in &interfacing.steps {
            let (items, item) =
                if crate_first { (&mut crates, &mut crate_one) } else { (&mut heaps, &mut heap) };
            let line = match step {
                IfaceStep::GrownAll => {
                    let mut total = 0_i64;
                    for each in items.iter_mut() {
                        total = checked(total.checked_add(each.grow(n)?))?;
                    }
                    Some(total.to_string())
                }
                IfaceStep::Bumped => {
                    item.bumped(n)?;
                    None
                }
                IfaceStep::Copied => {
                    let mut copy = item.clone();
                    copy.grow(n)?;
                    Some(format!("{} {}", copy.shown(id), item.shown(id)))
                }
                IfaceStep::Summed => Some(item.counted()?.to_string()),
                IfaceStep::Tried => {
                    let second_by = checked(n.checked_add(4))?;
                    let first = item.tried(n)?;
                    let second = if first.is_some() { item.tried(second_by)? } else { None };
                    if let (Some(first), Some(second)) = (first, second) {
                        Some(checked(first.checked_add(second))?.to_string())
                    } else {
                        // The first try fails past 5, and the second, given 4 more, past 1.
                        let code = if first.is_none() { n } else { second_by };
                        let _ = writeln!(self.out, "failed Nope{id}(code={code})");
                        Some("-1".to_owned())
                    }
                }
                IfaceStep::EachBumped => {
                    for each in items.iter_mut() {
                        each.bumped(n)?;
                    }
                    None
                }
                IfaceStep::Sized => {
                    let mut total = 0_i64;
                    for each in items.iter() {
                        total = checked(total.checked_add(each.size()?))?;
                    }
                    Some(total.to_string())
                }
            };
            if let Some(line) = line {
                let _ = writeln!(self.out, "{line}");
            }
        }
        let list = |items: &[Grower]| {
            let shown: Vec<String> = items.iter().map(|each| each.shown(id)).collect();
            format!("[{}]", shown.join(", "))
        };
        let _ = writeln!(self.out, "{}", list(&crates));
        let _ = writeln!(self.out, "{}", list(&heaps));
        let _ = writeln!(self.out, "{}", crate_one.shown(id));
        let _ = writeln!(self.out, "{}", heap.shown(id));
        Ok(())
    }

    /// What `locked_together{id}` prints: a change to two accounts under one `lock` that fails or
    /// crashes leaves both as they were, and one that ends keeps both changes.
    fn locked_together(&mut self, locking: &LockingTogether) -> Ran<()> {
        let id = locking.id;
        let base = locking.base;
        let mut accounts: [(i64, Vec<i64>); 3] = [
            (base, Vec::new()),
            (checked(base.checked_add(1))?, vec![1]),
            (checked(base.checked_add(2))?, vec![2, 3]),
        ];
        let length = |moves: &Vec<i64>| i64::try_from(moves.len()).map_err(|_| Stop::Unsure);
        for step in &locking.steps {
            let (one_at, two_at) = (step.pair.0 % 3, step.pair.1 % 3);
            let (one, two) = (
                accounts.get(one_at).cloned().ok_or(Stop::Unsure)?,
                accounts.get(two_at).cloned().ok_or(Stop::Unsure)?,
            );
            let (mut one, mut two) = (one, two);
            let amount = step.amount;
            match step.way {
                LockWay::Moved => {
                    let (from, to) = if step.backward { (&mut two, &mut one) } else { (&mut one, &mut two) };
                    from.0 = checked(from.0.checked_sub(amount))?;
                    from.1.push(-amount);
                    to.0 = checked(to.0.checked_add(amount))?;
                    to.1.push(amount);
                    if from.0 < 0 {
                        let _ = writeln!(self.out, "failed Short{id}(left={})", from.0);
                    } else {
                        for (at, kept) in [(one_at, one), (two_at, two)] {
                            if let Some(held) = accounts.get_mut(at) {
                                *held = kept;
                            }
                        }
                    }
                }
                LockWay::Crashed => {
                    let _ = writeln!(self.out, "crashed");
                }
                LockWay::Summed => {
                    let total = [one.0, two.0, length(&one.1)?, length(&two.1)?]
                        .iter()
                        .try_fold(0_i64, |total, each| total.checked_add(*each));
                    let _ = writeln!(self.out, "{}", checked(total)?);
                }
                LockWay::HalfRef => {
                    let added = checked(two.0.rem_euclid(7).checked_add(amount))?;
                    one.0 = checked(one.0.checked_add(added))?;
                    one.1.push(length(&two.1)?);
                    if let Some(held) = accounts.get_mut(one_at) {
                        *held = one;
                    }
                }
            }
            let shown: Vec<String> = accounts
                .iter()
                .map(|(total, moves)| format!("{total}{}", shown(&Value::Ints(moves.clone()))))
                .collect();
            let _ = writeln!(self.out, "{}", shown.join(" "));
        }
        Ok(())
    }

    /// What `ruled{id}` prints: each step's write, which stops the program where it breaks a
    /// `where` rule, as the field comes back for a field lent alone by `ref`, so a dip put right
    /// before then is fine, and a value made below 0 stops it where it's built (#2434).
    fn ruled(&mut self, ruling: &Ruling) -> Ran<()> {
        #[derive(Clone)]
        struct Gauge {
            level: i64,
            tags: Vec<String>,
            name: String,
            counts: [i64; 2],
        }
        let id = ruling.id;
        let made = |level: i64| Gauge { level, tags: Vec::new(), name: "g".to_owned(), counts: [1, 2] };
        let shown = |gauge: &Gauge| {
            let tags: Vec<String> = gauge.tags.iter().map(|tag| format!("\"{tag}\"")).collect();
            let [first, second] = gauge.counts;
            format!(
                "Gauge{id}(level={}, tags=[{}], name=\"{}\", counts=[{first}, {second}])",
                gauge.level,
                tags.join(", "),
                gauge.name
            )
        };
        let (mut gauge, mut gauges, mut by_name) = (made(5), [made(3), made(8)], made(4));
        for step in &ruling.steps {
            let RuleStep { way, value: v, breaks } = *step;
            let broken = |field: &str| format!("this breaks the `where` rule on `Gauge{id}.{field}`");
            // What's taken from a level: no more than it holds, or one more when it breaks.
            let amount = |level: i64| if breaks { level + v + 1 } else { level.min(v) };
            let picked = if breaks { -(v + 1) } else { v };
            let mut stopped = None;
            match way {
                RuleWay::Set => gauge.level = picked,
                RuleWay::Raised => gauge.level += v,
                RuleWay::Taken | RuleWay::Lowered | RuleWay::Drained | RuleWay::Shrunk => {
                    gauge.level -= amount(gauge.level);
                }
                RuleWay::Dipped => {}
                RuleWay::InList => {
                    let held = gauges.get_mut(usize::try_from(v % 2).unwrap_or(0)).ok_or(Stop::Unsure)?;
                    held.level -= amount(held.level);
                }
                RuleWay::ForRef => {
                    for each in &mut gauges {
                        each.level = if breaks { -(v + 1) } else { each.level + v };
                        if each.level < 0 {
                            break;
                        }
                    }
                }
                RuleWay::Counted if breaks => {
                    let [first, _] = gauge.counts;
                    let _ = writeln!(self.out, "counted {}", first + 20);
                    stopped = Some(broken("counts"));
                }
                RuleWay::Counted => {
                    for each in gauge.counts {
                        let _ = writeln!(self.out, "counted {each}");
                    }
                }
                RuleWay::Tagged | RuleWay::TaggedByRef if breaks => {
                    if way == RuleWay::TaggedByRef {
                        let _ = writeln!(self.out, "flooded {}", gauge.tags.len() + 4);
                    }
                    stopped = Some(broken("tags"));
                }
                RuleWay::Tagged | RuleWay::TaggedByRef => {
                    if gauge.tags.len() < 3 {
                        gauge.tags.push(format!("{}{v}", if way == RuleWay::Tagged { "t" } else { "r" }));
                    }
                }
                RuleWay::Named if breaks => stopped = Some(broken("name")),
                RuleWay::Named => gauge.name = format!("n{v}"),
                RuleWay::Copied => {
                    if breaks {
                        stopped = Some(broken("level"));
                    } else {
                        let _ = writeln!(self.out, "{}", shown(&Gauge { level: v, ..gauge.clone() }));
                    }
                }
                RuleWay::Made if breaks => stopped = Some(broken("level")),
                RuleWay::Made => {
                    let _ = writeln!(self.out, "made {}", shown(&made(v)));
                }
                RuleWay::InDict => by_name.level -= amount(by_name.level),
            }
            let below = gauge.level < 0 || gauges.iter().any(|held| held.level < 0) || by_name.level < 0;
            if let Some(said) = stopped.or_else(|| below.then(|| broken("level"))) {
                self.said = Some(said);
                return Err(Stop::Failed);
            }
        }
        let _ = writeln!(self.out, "{}", shown(&gauge));
        let listed: Vec<String> = gauges.iter().map(shown).collect();
        let _ = writeln!(self.out, "[{}]", listed.join(", "));
        let _ = writeln!(self.out, "{{\"k\": {}}}", shown(&by_name));
        Ok(())
    }

    /// What `popped{id}` prints: each value its text, alone and inside a list, and a missing one
    /// what `else` gives in its place, as an item that's `None` gives too.
    fn popped(&mut self, popping: &Popping) {
        let id = popping.id;
        let values = popping.ty.values(id);
        let alone = |at: usize| values.get(at).map_or(String::new(), |(_, shown, _)| shown.clone());
        let inside = |item: &Option<usize>| match item {
            Some(at) => values.get(*at).map_or(String::new(), |(_, _, shown)| shown.clone()),
            None => "None".to_owned(),
        };
        let missing = alone(3);
        // `pop(at)` and `remove` give the item, a `T?`, so `else` takes a `None` held for the default.
        let taken = |item: Option<Option<usize>>| item.flatten().map_or_else(|| missing.clone(), alone);
        // `pop()` gives a `T??`, which keeps a `None` held apart from the list being empty: `else`
        // takes off only the outer one, so a `None` held comes out as itself.
        let popped = |item: Option<Option<usize>>| match item {
            None => missing.clone(),
            Some(None) => "None".to_owned(),
            Some(Some(at)) => alone(at),
        };
        let mut list: Vec<Option<usize>> = vec![Some(0), None, Some(1)];
        let mut lines = Vec::new();
        for &(way, which) in &popping.steps {
            let position = |list: &Vec<Option<usize>>, wanted: Option<usize>| {
                list.iter().position(|item| *item == wanted)
            };
            match way {
                PopWay::Pushed => list.push(Some(which)),
                PopWay::PushedNone => list.push(None),
                PopWay::Kept | PopWay::Inline | PopWay::ByRef => lines.push(popped(list.pop())),
                PopWay::Dropped => {
                    let _ = list.pop();
                }
                PopWay::First | PopWay::Middle | PopWay::Last if !list.is_empty() => {
                    let at = match way {
                        PopWay::First => 0,
                        PopWay::Middle => list.len() / 2,
                        _ => list.len() - 1,
                    };
                    lines.push(taken(Some(list.remove(at))));
                }
                PopWay::First | PopWay::Middle | PopWay::Last => {}
                PopWay::Removed => {
                    let found = position(&list, Some(which)).map(|at| list.remove(at));
                    lines.push(taken(found));
                }
                PopWay::Discarded | PopWay::DiscardedNone => {
                    let wanted = if way == PopWay::Discarded { Some(which) } else { None };
                    if let Some(at) = position(&list, wanted) {
                        list.remove(at);
                    }
                }
                PopWay::Empty => lines.push(missing.clone()),
            }
        }
        for line in lines {
            let _ = writeln!(self.out, "{line}");
        }
        let (held, empty) =
            (format!("[{}]", list.iter().map(inside).collect::<Vec<_>>().join(", ")), "[]".to_owned());
        let (local, field) = if popping.in_field { (empty, held) } else { (held, empty) };
        let _ = writeln!(self.out, "{local}");
        let _ = writeln!(self.out, "Holder{id}(items={field})");
    }

    /// What `sized{id}` prints: each step's number as compiled code works it out, or what stops
    /// the program, in its words. A sum, a widening and a parse print what they give and leave
    /// the number as it was.
    fn sized(&mut self, sizing: &Sizing) -> Ran<()> {
        let Sizing { num, start, .. } = sizing;
        let (least, most) = num.range();
        let mut x = *start;
        for step in &sizing.steps {
            let SizedStep { way, operand: k } = *step;
            let after = match sized_step(*num, x, way, k) {
                Ok(after) => after,
                Err(said) => {
                    self.said = Some(said);
                    return Err(Stop::Failed);
                }
            };
            match way {
                SizedWay::Widened => {
                    // `//` by a number above 0 rounds down, as `div_euclid` does.
                    let _ = writeln!(self.out, "widened {}", x.div_euclid(3));
                }
                SizedWay::Summed => {
                    let _ = writeln!(self.out, "summed {after}");
                }
                SizedWay::Parsed if (least..=most).contains(&k) => {
                    let _ = writeln!(self.out, "parsed {k}");
                }
                SizedWay::Parsed => {
                    let _ = writeln!(
                        self.out,
                        "refused {k} doesn't fit in {}, which holds {least} to {most}",
                        num.name()
                    );
                }
                _ => x = after,
            }
            let _ = writeln!(self.out, "{x}");
        }
        Ok(())
    }

    /// What `arrayed{id}` prints: each array held inline, so a copy made by `=`, given as a value
    /// or held in a list, a Table or a dict changes apart from the rest, and an index past either
    /// end of `counts` stops the program.
    fn arrayed(&mut self, arraying: &Arraying) -> Ran<()> {
        #[derive(Clone, Default, PartialEq)]
        struct Cell {
            health: i64,
            tags: Vec<String>,
        }
        #[derive(Clone, Default, PartialEq)]
        struct Grid {
            counts: [i64; 4],
            cells: [Cell; 3],
            slots: [Option<Cell>; 2],
            names: [String; 2],
            rows: [[i64; 2]; 2],
        }
        let id = arraying.id;
        let ints = |items: &[i64]| shown(&Value::Ints(items.to_vec()));
        let texts = |items: &[String]| {
            format!("[{}]", items.iter().map(|item| format!("\"{item}\"")).collect::<Vec<_>>().join(", "))
        };
        let cell_shown =
            |cell: &Cell| format!("Cell{id}(health={}, tags={})", cell.health, texts(&cell.tags));
        let grid_shown = |grid: &Grid| {
            let cells: Vec<String> = grid.cells.iter().map(cell_shown).collect();
            let slots: Vec<String> = grid
                .slots
                .iter()
                .map(|slot| slot.as_ref().map_or_else(|| "None".to_owned(), cell_shown))
                .collect();
            let rows: Vec<String> = grid.rows.iter().map(|row| ints(row)).collect();
            format!(
                "Grid{id}(counts={}, cells=[{}], slots=[{}], names={}, rows=[{}])",
                ints(&grid.counts),
                cells.join(", "),
                slots.join(", "),
                texts(&grid.names),
                rows.join(", ")
            )
        };
        let add = |place: &mut i64, by: i64| -> Ran<()> {
            *place = checked(place.checked_add(by))?;
            Ok(())
        };
        let out_of_range = |at: i64| format!("index {at} is out of range for length 4");
        let mut grid = Grid::default();
        let other = Grid { counts: [1, 2, 3, -4], ..Grid::default() };
        let mut grids = [grid.clone(), other.clone()];
        let mut table = other.clone();
        let mut by_name = grid.clone();
        let mut loose = [0_i64; 4];
        for step in &arraying.steps {
            let v = step.value;
            let (a4, a3, a2) = (step.at % 4, step.at % 3, step.at % 2);
            match step.way {
                ArrayWay::Set => *grid.counts.get_mut(a4).ok_or(Stop::Unsure)? = v,
                ArrayWay::Added => add(grid.counts.get_mut(a4).ok_or(Stop::Unsure)?, v)?,
                ArrayWay::FromEnd => add(grid.counts.get_mut(3 - a4).ok_or(Stop::Unsure)?, v)?,
                ArrayWay::Copied => {
                    let mut copied = grid.counts;
                    *copied.get_mut(a4).ok_or(Stop::Unsure)? = v;
                    let _ = writeln!(self.out, "{} {}", ints(&copied), ints(&grid.counts));
                }
                ArrayWay::Replaced => {
                    grid.counts = loose;
                    *loose.get_mut(a4).ok_or(Stop::Unsure)? = v;
                }
                ArrayWay::ForRefCells => {
                    for cell in &mut grid.cells {
                        add(&mut cell.health, v)?;
                        cell.tags.push(format!("t{v}"));
                    }
                }
                ArrayWay::CellSet => grid.cells.get_mut(a3).ok_or(Stop::Unsure)?.health = v,
                ArrayWay::CellTagged => {
                    grid.cells.get_mut(a3).ok_or(Stop::Unsure)?.tags.push(format!("c{v}"));
                }
                ArrayWay::SlotFilled => {
                    *grid.slots.get_mut(a2).ok_or(Stop::Unsure)? = Some(Cell { health: v, tags: Vec::new() });
                }
                ArrayWay::SlotCleared => *grid.slots.get_mut(a2).ok_or(Stop::Unsure)? = None,
                ArrayWay::ForRefSlots => {
                    for cell in grid.slots.iter_mut().flatten() {
                        add(&mut cell.health, v)?;
                    }
                }
                ArrayWay::SlotRead => {
                    let slot = grid.slots.get(a2).ok_or(Stop::Unsure)?;
                    let health = slot.as_ref().map_or(v, |cell| cell.health);
                    let _ = writeln!(self.out, "{} {health}", if slot.is_none() { "True" } else { "False" });
                }
                ArrayWay::InList => {
                    add(grids.get_mut(a2).ok_or(Stop::Unsure)?.counts.get_mut(a4).ok_or(Stop::Unsure)?, v)?;
                }
                ArrayWay::ForRefList => {
                    for each in &mut grids {
                        add(each.counts.get_mut(a4).ok_or(Stop::Unsure)?, v)?;
                    }
                }
                ArrayWay::InTable => *table.counts.get_mut(a4).ok_or(Stop::Unsure)? = v,
                ArrayWay::InDict => add(by_name.counts.get_mut(a4).ok_or(Stop::Unsure)?, v)?,
                ArrayWay::ForRef | ArrayWay::ByRef => {
                    for each in &mut grid.counts {
                        add(each, v)?;
                    }
                }
                ArrayWay::ByValue => {
                    let total = grid.counts.iter().try_fold(0_i64, |total, each| total.checked_add(*each));
                    let summed =
                        checked(checked(total)?.checked_mul(1000).and_then(|total| total.checked_add(100)))?;
                    let _ = writeln!(self.out, "{summed}");
                }
                ArrayWay::Read => {
                    let found = if grid.counts.contains(&v) { "True" } else { "False" };
                    let same = if grid.counts == other.counts { "True" } else { "False" };
                    let mut listed = grid.counts.to_vec();
                    listed.push(v);
                    let _ = writeln!(self.out, "{found} 4 3 2 {same} {}", ints(&listed));
                }
                ArrayWay::Rows => {
                    let row = grid.rows.get_mut(a2).ok_or(Stop::Unsure)?;
                    add(row.get_mut((step.at + 1) % 2).ok_or(Stop::Unsure)?, v)?;
                }
                ArrayWay::RowCopied => {
                    let mut row = *grid.rows.get(a2).ok_or(Stop::Unsure)?;
                    *row.get_mut(1).ok_or(Stop::Unsure)? = v;
                    let rows: Vec<String> = grid.rows.iter().map(|each| ints(each)).collect();
                    let _ = writeln!(self.out, "{} [{}]", ints(&row), rows.join(", "));
                }
                ArrayWay::Named => {
                    let _ = write!(grid.names.get_mut(a2).ok_or(Stop::Unsure)?, "n{v}");
                }
                ArrayWay::WholeCopied => {
                    let mut whole = grid.clone();
                    *whole.counts.get_mut(a4).ok_or(Stop::Unsure)? = v;
                    let [whole_first, ..] = &mut whole.cells;
                    whole_first.tags.push("w".to_owned());
                    let [first, ..] = &grid.cells;
                    let _ = writeln!(
                        self.out,
                        "{} {} {} {}",
                        ints(&whole.counts),
                        ints(&grid.counts),
                        cell_shown(whole_first),
                        cell_shown(first)
                    );
                }
                ArrayWay::Looped => {
                    for place in 0..step.bound() {
                        let Some(each) = usize::try_from(place).ok().and_then(|at| grid.counts.get_mut(at))
                        else {
                            self.said = Some(out_of_range(place));
                            return Err(Stop::Failed);
                        };
                        add(each, v)?;
                    }
                }
                ArrayWay::Picked => {
                    let at = step.picked();
                    let from_start = if at < 0 { at + 4 } else { at };
                    let Some(place) = usize::try_from(from_start).ok().and_then(|at| grid.counts.get_mut(at))
                    else {
                        self.said = Some(out_of_range(at));
                        return Err(Stop::Failed);
                    };
                    *place = v;
                }
            }
        }
        let shown_all: Vec<String> = grids.iter().map(grid_shown).collect();
        let _ = writeln!(self.out, "{}", grid_shown(&grid));
        let _ = writeln!(self.out, "{}", grid_shown(&other));
        let _ = writeln!(self.out, "[{}]", shown_all.join(", "));
        let _ = writeln!(self.out, "{}", grid_shown(&table));
        let _ = writeln!(self.out, "{{\"k\": {}}}", grid_shown(&by_name));
        let _ = writeln!(self.out, "{}", ints(&loose));
        Ok(())
    }

    /// What `generated{id}` prints: each step's items as its generators yield them, read lazily,
    /// `zip` asking its first side before its second, then `end` or the failure that ended it,
    /// wrapped as the `? from` of the generator that failed says.
    fn generated(&mut self, generating: &Generating) {
        let id = generating.id;
        // What `failing(limit, fail_at)` yields, and whether it fails after them.
        let failing = |limit: i64, fail_at: i64| -> (Vec<i64>, bool) {
            if fail_at < limit { ((0..fail_at).collect(), true) } else { ((0..limit).collect(), false) }
        };
        for (n, step) in generating.steps.iter().enumerate() {
            let GenStep { way, limit, fail_at, other_at } = *step;
            let (first, first_fails) = failing(limit, fail_at);
            let (second, second_fails) = failing(limit + 1, other_at);
            let shut = |code: i64| format!("Shut{id}(code={code})");
            let lost = format!("Lost{id}(step={n})");
            let mut lines: Vec<String> = Vec::new();
            let each = |name: &str, items: &[i64], lines: &mut Vec<String>| {
                lines.extend(items.iter().map(|item| format!("{name} {item}")));
            };
            let failed = match way {
                GenWay::ForFrom => {
                    each("for", &first, &mut lines);
                    first_fails.then(|| lost.clone())
                }
                GenWay::EnumerateFrom => {
                    lines.extend(
                        first.iter().enumerate().map(|(place, item)| format!("enumerate {place} {item}")),
                    );
                    first_fails.then(|| lost.clone())
                }
                GenWay::ZipBothFrom | GenWay::ZipLeftFrom | GenWay::ZipRightFrom | GenWay::ZipListFrom => {
                    let words = [10, 20, 30];
                    let (left, left_fails, left_why) = match way {
                        GenWay::ZipListFrom => (words.to_vec(), false, String::new()),
                        GenWay::ZipRightFrom => (first.clone(), first_fails, shut(fail_at)),
                        _ => (first.clone(), first_fails, format!("Left{id}(step={n})")),
                    };
                    let (right, right_fails, right_why) = match way {
                        GenWay::ZipListFrom => (first.clone(), first_fails, format!("Right{id}(step={n})")),
                        GenWay::ZipLeftFrom => (second.clone(), second_fails, shut(other_at)),
                        _ => (second.clone(), second_fails, format!("Right{id}(step={n})")),
                    };
                    let mut failed = None;
                    for at in 0.. {
                        let Some(one) = left.get(at) else {
                            failed = left_fails.then_some(left_why);
                            break;
                        };
                        let Some(other) = right.get(at) else {
                            failed = right_fails.then_some(right_why);
                            break;
                        };
                        lines.push(format!("zip {one} {other}"));
                    }
                    failed
                }
                GenWay::Broken => {
                    let mut failed = first_fails.then(|| lost.clone());
                    for item in &first {
                        lines.push(format!("broken {item}"));
                        if *item == other_at {
                            failed = None;
                            break;
                        }
                    }
                    failed
                }
                GenWay::Named => {
                    lines.push("named".to_owned());
                    each("named", &first, &mut lines);
                    first_fails.then(|| shut(fail_at))
                }
                GenWay::KeptOver => {
                    let mut failed = None;
                    for size in 0..limit {
                        let (items, fails) = failing(size + 1, fail_at);
                        each("kept", &items, &mut lines);
                        if fails {
                            failed = Some(shut(fail_at));
                            break;
                        }
                    }
                    failed
                }
                GenWay::InTuple | GenWay::InList => {
                    let (one, two) =
                        if way == GenWay::InTuple { ("first", "second") } else { ("listed", "listed") };
                    each(one, &first, &mut lines);
                    if first_fails {
                        Some(shut(fail_at))
                    } else {
                        each(two, &second, &mut lines);
                        second_fails.then(|| shut(other_at))
                    }
                }
                GenWay::TupleIndexed => {
                    lines.extend(first.iter().map(|item| format!("indexed {item} {n}")));
                    first_fails.then(|| shut(fail_at))
                }
                GenWay::InBox => {
                    each("boxed", &first, &mut lines);
                    first_fails.then(|| shut(fail_at))
                }
            };
            for line in lines {
                let _ = writeln!(self.out, "{line}");
            }
            match failed {
                Some(why) => {
                    let _ = writeln!(self.out, "failed {why}");
                }
                None => {
                    let _ = writeln!(self.out, "end {n}");
                }
            }
        }
    }

    /// What `ordered{id}` prints: each line read left to right, as `ordered_written` writes it,
    /// then what the tally and the pair hold.
    fn ordered(&mut self, in_order: &InOrder) -> Ran<()> {
        let base = in_order.base;
        let mut tally = Tallied { items: vec![base, base + 1, base + 2, base + 3], count: base, named: base };
        let (mut left, right) = (tally.clone(), base);
        let ints = |items: &[i64]| shown(&Value::Ints(items.to_vec()));
        for &(step, n) in &in_order.steps {
            let line = match step {
                OrderStep::Receiver => {
                    let (items, count) = (ints(&tally.items), tally.count);
                    format!("receiver {items} {count} {}", tally.grow(n)?)
                }
                OrderStep::FieldReceiver => {
                    let count = left.count;
                    format!("field receiver {count} {right} {}", left.grow(n)?)
                }
                OrderStep::Counted => {
                    let items = tally.items.clone();
                    let grown = tally.grow(n)?;
                    format!("counted {}", items.iter().filter(|item| **item == grown).count())
                }
                OrderStep::Got | OrderStep::Keyed => {
                    let named = tally.named;
                    tally.named = tally.named.checked_add(n).ok_or(Stop::Unsure)?;
                    format!("{} {named}", if step == OrderStep::Got { "got" } else { "keyed" })
                }
                OrderStep::Indexed => {
                    let items = tally.items.clone();
                    let at = usize::try_from(tally.grow(n)? % 4).map_err(|_| Stop::Unsure)?;
                    format!("indexed {}", items.get(at).ok_or(Stop::Unsure)?)
                }
                OrderStep::Arguments => {
                    let first = tally.count;
                    let second = tally.grow(n)?;
                    format!("arguments {first} {second} {}", tally.count)
                }
                OrderStep::PlusBefore => {
                    let count = tally.count;
                    format!("plus before {}", count.checked_add(tally.grow(n)?).ok_or(Stop::Unsure)?)
                }
                OrderStep::PlusAfter => {
                    let grown = tally.grow(n)?;
                    format!("plus after {}", grown.checked_add(tally.count).ok_or(Stop::Unsure)?)
                }
                OrderStep::Listed | OrderStep::Text => {
                    let first = tally.count;
                    let second = tally.grow(n)?;
                    let third = tally.count;
                    if step == OrderStep::Listed {
                        format!("listed {}", ints(&[first, second, third]))
                    } else {
                        format!("text {first} {second} {third}")
                    }
                }
                OrderStep::AtLeast => {
                    let count = tally.count;
                    let grown = tally.grow(n)?;
                    format!("at least {}", if count >= grown { "True" } else { "False" })
                }
                OrderStep::FieldWrite => {
                    tally.count = tally.grow(n)?.checked_add(n).ok_or(Stop::Unsure)?;
                    format!("field write {}", tally.count)
                }
                OrderStep::IndexWrite => {
                    let at = usize::try_from(tally.grow(n)? % 4).map_err(|_| Stop::Unsure)?;
                    let put = tally.grow(n)?;
                    *tally.items.get_mut(at).ok_or(Stop::Unsure)? = put;
                    format!("index write {}", ints(&tally.items))
                }
                OrderStep::Added => {
                    let count = tally.count;
                    tally.count = count.checked_add(tally.grow(n)?).ok_or(Stop::Unsure)?;
                    format!("added {}", tally.count)
                }
            };
            let _ = writeln!(self.out, "{line}");
        }
        let _ = writeln!(
            self.out,
            "tally {} {} {{\"a\": {}}}\npair {} {} {right}",
            ints(&tally.items),
            tally.count,
            tally.named,
            ints(&left.items),
            left.count
        );
        Ok(())
    }

    /// What `turned{id}` prints: each turn, its int, `XS` at it, it times 3, and it added to each
    /// end of `int`. A place outside `XS` or an add past an end stops the program, as natively.
    fn turned(&mut self, turning: &Turning) -> Ran<()> {
        let Turning { how, rounds, start, step, reset_at, reset_to, .. } = *turning;
        let (mut at, mut sign, mut n) = (start, 1_i64, start);
        for i in 0..rounds {
            if how == Turn::Reset && i == reset_at {
                n = reset_to;
            }
            let value = match how {
                Turn::StepDown => at,
                Turn::Flip => checked(sign.checked_mul(start))?,
                Turn::Reset => n,
            };
            let item = XS.get(place_in(value, XS.len())?).copied().ok_or(Stop::Unsure)?;
            let low = (-9_223_372_036_854_775_804_i64).checked_add(value).ok_or(Stop::Failed)?;
            let high = 9_223_372_036_854_775_804_i64.checked_add(value).ok_or(Stop::Failed)?;
            let _ = writeln!(self.out, "turn {value} {item} {} {low} {high}", checked(value.checked_mul(3))?);
            match how {
                Turn::StepDown => at = checked(at.checked_sub(step))?,
                Turn::Flip => sign = -sign,
                Turn::Reset => n = checked(n.checked_add(1))?,
            }
        }
        Ok(())
    }

    /// What `tried`, `tried_twice`, `tried_each`, `ran_fine` or `either` prints, given an arrow
    /// calling `capped(n, limit)?`, which fails with `n` when it's over `limit`.
    fn arrowed(&mut self, how: &Arrow, n: &Expr, limit: i64) -> Ran<()> {
        let line = if let Arrow::Each(list) = how {
            let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
            match items.iter().position(|item| *item > limit) {
                Some(at) => {
                    let kept = shown(&Value::Ints(items.get(..at).unwrap_or_default().to_vec()));
                    format!("refused {} after {kept}", items.get(at).copied().unwrap_or(0))
                }
                None => shown(&Value::Ints(items)),
            }
        } else {
            let n = int(&self.expr(n)?)?;
            let fine = n <= limit;
            match how {
                Arrow::Tried | Arrow::Fallback if fine => n.to_string(),
                Arrow::Tried => format!("refused {n}"),
                Arrow::Twice if fine => format!("{n} {n}"),
                Arrow::Twice => "refused".to_owned(),
                Arrow::RanFine => shown(&Value::Bool(fine)),
                Arrow::Fallback => checked(n.checked_mul(100))?.to_string(),
                Arrow::Either(_) if !fine => format!("refused {n}"),
                Arrow::Either(other) => {
                    // The second call is reached only once the first gave its value.
                    let other = int(&self.expr(other)?)?;
                    if other < checked(limit.checked_neg())? {
                        format!("shut {other}")
                    } else {
                        format!("{n} {other}")
                    }
                }
                Arrow::Each(_) => return Err(Stop::Unsure),
            }
        };
        let _ = writeln!(self.out, "{line}");
        Ok(())
    }

    /// `summed_until(limit, stop, stdout)`: the source opens, the generator gives `at + limit`
    /// for each `at` below `limit`, the loop sums them until one is `stop`, and the source closes
    /// as the generator's let go or ends, failing for an even `limit`.
    fn summed(&mut self, limit: &Expr, stop: &Expr) -> Ran<()> {
        let limit = int(&self.expr(limit)?)?;
        let stop = int(&self.expr(stop)?)?;
        let _ = writeln!(self.out, "opening {limit}");
        let mut total: i64 = 0;
        for at in 0..limit.max(0) {
            let value = checked(at.checked_add(limit))?;
            if value == stop {
                break;
            }
            total = checked(total.checked_add(value))?;
        }
        let _ = writeln!(self.out, "closing {limit}");
        if floor_rem(limit, 2)? == 0 {
            self.out.push_str("shut\n");
        } else {
            let _ = writeln!(self.out, "{total}");
        }
        Ok(())
    }

    /// What `through_shown` prints for a loop over `enumerate` or `zip` of a generator made right
    /// there, as the compiled program runs it since #2041. `lent_counts` prints as its source opens,
    /// when it's first asked for an item, and as it closes, when it finishes or is let go, and its
    /// cleanup fails for an even limit. `failing_counts` fails as it's asked for item `stop`. Then
    /// `shut` for a failure, or the total.
    fn zip_summed(&mut self, zipped: &ZipSumming) -> Ran<()> {
        let ZipSumming { way, failing, limit, stop, words } = zipped.clone();
        let words: Vec<i64> = (1..=words.max(0)).collect();
        let (mut total, mut started, mut failed): (i64, bool, bool) = (0, false, false);
        for at in 0_i64.. {
            let place = usize::try_from(at).map_err(|_| Stop::Unsure)?;
            // `zip`'s list first, when the generator is its second.
            if way == ZipWay::Second && place >= words.len() {
                break;
            }
            if !failing && !started {
                let _ = writeln!(self.out, "opening {limit}");
                started = true;
            }
            if at >= limit {
                break;
            }
            if failing && at == stop {
                failed = true;
                break;
            }
            let value = if failing { at } else { checked(at.checked_add(limit))? };
            if way == ZipWay::First && place >= words.len() {
                break;
            }
            if !failing && value == stop {
                break;
            }
            let beside = match way {
                ZipWay::Enumerated => at,
                ZipWay::First | ZipWay::Second => words.get(place).copied().ok_or(Stop::Unsure)?,
            };
            total = checked(total.checked_add(checked(value.checked_add(beside))?))?;
        }
        if started {
            let _ = writeln!(self.out, "closing {limit}");
            failed |= floor_rem(limit, 2)? == 0;
        }
        if failed {
            self.out.push_str("shut\n");
        } else {
            let _ = writeln!(self.out, "{total}");
        }
        Ok(())
    }

    fn assign(&mut self, place: &Place, value: &Expr, add: bool) -> Ran<()> {
        // The place first, then what it held, then the new value.
        let at = match place {
            Place::Item(name, at) => {
                let at = int(&self.expr(at)?)?;
                let Value::Ints(items) = self.get(name)? else { return Err(Stop::Unsure) };
                Some(place_in(at, items.len())?)
            }
            _ => None,
        };
        let old = match (place, at) {
            (Place::Name(name), _) => self.get(name)?.clone(),
            (Place::Item(name, _), Some(at)) => match self.get(name)? {
                Value::Ints(items) => Value::Int(items.get(at).copied().ok_or(Stop::Unsure)?),
                _ => return Err(Stop::Unsure),
            },
            (Place::Field(name, x), _) => match self.get(name)? {
                Value::Point(px, py) => Value::Int(if *x { *px } else { *py }),
                _ => return Err(Stop::Unsure),
            },
            (Place::Item(..), None) => return Err(Stop::Unsure),
        };
        let mut new = self.expr(value)?;
        if add {
            new = match (&old, &new) {
                (Value::Float(old), Value::Float(by)) => {
                    Value::Float((f64::from_bits(*old) + f64::from_bits(*by)).to_bits())
                }
                _ => Value::Int(checked(int(&old)?.checked_add(int(&new)?))?),
            };
        }
        match place {
            Place::Name(name) => *self.slot(name)? = new,
            Place::Item(name, _) => {
                let new = int(&new)?;
                if let Value::Ints(items) = self.slot(name)?
                    && let Some(item) = at.and_then(|at| items.get_mut(at))
                {
                    *item = new;
                }
            }
            Place::Field(name, x) => {
                let new = int(&new)?;
                if let Value::Point(px, py) = self.slot(name)? {
                    *(if *x { px } else { py }) = new;
                }
            }
        }
        Ok(())
    }

    fn expr(&mut self, expr: &Expr) -> Ran<Value> {
        Ok(match expr {
            Expr::Int(n) => Value::Int(*n),
            Expr::Bool(value) => Value::Bool(*value),
            Expr::Str(text) => Value::Str(text.clone()),
            Expr::Name(name) => self.get(name)?.clone(),
            Expr::Arith(operator, left, right) => {
                let (left, right) = (int(&self.expr(left)?)?, int(&self.expr(right)?)?);
                Value::Int(match operator {
                    Arith::Add => checked(left.checked_add(right))?,
                    Arith::Sub => checked(left.checked_sub(right))?,
                    Arith::Mul => checked(left.checked_mul(right))?,
                    Arith::Div => floor_div(left, right)?,
                    Arith::Rem => floor_rem(left, right)?,
                })
            }
            Expr::Small(value) => Value::Int(floor_rem(int(&self.expr(value)?)?, 1009)?),
            Expr::Compare(compare, left, right) => {
                let (left, right) = (int(&self.expr(left)?)?, int(&self.expr(right)?)?);
                Value::Bool(match compare {
                    Compare::Less => left < right,
                    Compare::LessEq => left <= right,
                    Compare::Equal => left == right,
                    Compare::NotEqual => left != right,
                    Compare::Greater => left > right,
                })
            }
            Expr::And(left, right) => {
                if self.expr(left)? == Value::Bool(true) {
                    self.expr(right)?
                } else {
                    Value::Bool(false)
                }
            }
            Expr::Not(value) => Value::Bool(self.expr(value)? != Value::Bool(true)),
            Expr::Choose(when, yes, no) => {
                if self.expr(when)? == Value::Bool(true) {
                    self.expr(yes)?
                } else {
                    self.expr(no)?
                }
            }
            Expr::Concat(left, right) => {
                let (Value::Str(left), Value::Str(right)) = (self.expr(left)?, self.expr(right)?) else {
                    return Err(Stop::Unsure);
                };
                // Past this, the program is about memory, not what it computes.
                if left.len() + right.len() > 1 << 20 {
                    return Err(Stop::Unsure);
                }
                Value::Str(left + &right)
            }
            Expr::Format(value) => Value::Str(format!("<{}>", int(&self.expr(value)?)?)),
            Expr::TextLen(text) => {
                let Value::Str(text) = self.expr(text)? else { return Err(Stop::Unsure) };
                Value::Int(i64::try_from(text.chars().count()).map_err(|_| Stop::Unsure)?)
            }
            Expr::Ints(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(int(&self.expr(item)?)?);
                }
                Value::Ints(values)
            }
            Expr::Regrown(list) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                let mut terms = Vec::with_capacity(items.len());
                for seed in items {
                    let next = checked(seed.checked_add(1))?;
                    let grown = [seed, next, seed, seed, next, seed];
                    let mut total: i64 = 0;
                    for item in grown {
                        total = checked(total.checked_add(item))?;
                    }
                    terms.push(checked(total.checked_add(3))?);
                }
                Value::Int(sum_of(&terms)?)
            }
            Expr::ParallelSum(list, by) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                let mut terms = Vec::with_capacity(items.len());
                for item in items {
                    let by = int(&self.expr(by)?)?;
                    terms.push(checked(item.checked_mul(by))?);
                }
                // Which overflow stops it, of the tree's or a term's, doesn't change what it says.
                let added = tree_sum(&terms, &|left: i64, right| checked(left.checked_add(right)))?;
                Value::Int(added.unwrap_or(0))
            }
            Expr::FloatSum(list, scale, filtered) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                let mut terms = Vec::with_capacity(items.len());
                for item in items {
                    if *filtered && item % 2 != 0 {
                        continue;
                    }
                    #[expect(clippy::cast_precision_loss, reason = "float(int) rounds to the nearest float")]
                    let item = item as f64;
                    terms.push(item * self.float_of(scale)?);
                }
                let total = if *filtered {
                    lanes_sum(&terms, 0.0)
                } else {
                    tree_sum(&terms, &|left: f64, right| Ok(left + right))?.map_or(0.0, |tree| 0.0 + tree)
                };
                Value::Float(total.to_bits())
            }
            Expr::Upto(count) => Value::Ints((0..*count).map(|at| i64::try_from(at).unwrap_or(0)).collect()),
            Expr::Mapped(list, by, plus, _) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                let mut mapped = Vec::with_capacity(items.len());
                for item in items {
                    let by = int(&self.expr(by)?)?;
                    let times = checked(item.checked_mul(by))?;
                    let plus = int(&self.expr(plus)?)?;
                    mapped.push(checked(times.checked_add(plus))?);
                }
                Value::Ints(mapped)
            }
            Expr::Len(list) => {
                let len = match self.expr(list)? {
                    Value::Ints(items) => items.len(),
                    Value::Nodes(trees) => trees.len(),
                    _ => return Err(Stop::Unsure),
                };
                Value::Int(i64::try_from(len).map_err(|_| Stop::Unsure)?)
            }
            Expr::Index(list, at) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                let at = place_in(int(&self.expr(at)?)?, items.len())?;
                Value::Int(items.get(at).copied().ok_or(Stop::Unsure)?)
            }
            Expr::AtOrZero(list, at) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                let at = int(&self.expr(at)?)?;
                if items.is_empty() {
                    Value::Int(0)
                } else {
                    let len = i64::try_from(items.len()).map_err(|_| Stop::Unsure)?;
                    let at = usize::try_from(floor_rem(at, len)?).map_err(|_| Stop::Unsure)?;
                    Value::Int(items.get(at).copied().ok_or(Stop::Unsure)?)
                }
            }
            Expr::Sum(list) => {
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                Value::Int(sum_of(&items)?)
            }
            Expr::Holder(items, tag) => {
                let Value::Ints(items) = self.expr(items)? else { return Err(Stop::Unsure) };
                Value::Holder(items, int(&self.expr(tag)?)?)
            }
            Expr::HeldItems(holder) => match self.expr(holder)? {
                Value::Holder(items, _) => Value::Ints(items),
                _ => return Err(Stop::Unsure),
            },
            Expr::Tag(holder) => match self.expr(holder)? {
                Value::Holder(_, tag) => Value::Int(tag),
                _ => return Err(Stop::Unsure),
            },
            Expr::Lists(pairs) => {
                let mut lists = Vec::with_capacity(pairs.len());
                for (key, list) in pairs {
                    let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                    lists.push(((*key).to_owned(), items));
                }
                Value::Lists(lists)
            }
            Expr::ListAt(lists, key) => match self.expr(lists)? {
                Value::Lists(pairs) => {
                    let found = pairs.into_iter().find(|(held, _)| held == key);
                    Value::Ints(found.map(|(_, items)| items).unwrap_or_default())
                }
                _ => return Err(Stop::Unsure),
            },
            Expr::Summing(name) => match self.get(name)? {
                Value::Ints(items) => Value::Count(items.clone()),
                _ => return Err(Stop::Unsure),
            },
            Expr::Counts(pairs) => {
                let mut counts = Vec::with_capacity(pairs.len());
                for (key, n) in pairs {
                    counts.push(((*key).to_owned(), int(&self.expr(n)?)?));
                }
                Value::Counts(counts)
            }
            Expr::CountOf(counts, key) => match self.expr(counts)? {
                Value::Counts(pairs) => {
                    Value::Int(pairs.iter().find(|(held, _)| held == key).map_or(0, |(_, n)| *n))
                }
                _ => return Err(Stop::Unsure),
            },
            Expr::Has(counts, key) => match self.expr(counts)? {
                Value::Counts(pairs) => Value::Bool(pairs.iter().any(|(held, _)| held == key)),
                _ => return Err(Stop::Unsure),
            },
            Expr::Buffer(_, items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(int(&self.expr(item)?)?);
                }
                Value::Buf(values)
            }
            Expr::BufItems(buffer) => Value::Ints(self.buffer_of(buffer)?),
            Expr::BufLen(buffer) => {
                Value::Int(i64::try_from(self.buffer_of(buffer)?.len()).map_err(|_| Stop::Unsure)?)
            }
            Expr::BufAt(buffer, at) => {
                let items = self.buffer_of(buffer)?;
                let at = int(&self.expr(at)?)?;
                // Counting from the end isn't said for a buffer.
                let at = usize::try_from(at).map_err(|_| Stop::Unsure)?;
                Value::Int(items.get(at).copied().ok_or(Stop::Failed)?)
            }
            Expr::Float(x) => Value::Float(x.to_bits()),
            Expr::FloatArith(operator, left, right) => {
                let (left, right) = (self.float_of(left)?, self.float_of(right)?);
                let result = match operator {
                    Arith::Add => left + right,
                    Arith::Sub => left - right,
                    Arith::Mul => left * right,
                    Arith::Div | Arith::Rem => {
                        if right == 0.0 {
                            return Err(Stop::Failed);
                        }
                        left / right
                    }
                };
                Value::Float(result.to_bits())
            }
            Expr::FloatOf(value) => {
                #[expect(clippy::cast_precision_loss, reason = "float(int) rounds to the nearest float")]
                let x = int(&self.expr(value)?)? as f64;
                Value::Float(x.to_bits())
            }
            Expr::Truncated(value) => {
                let x = self.float_of(value)?.trunc();
                // `i64::MAX` itself isn't a float, so the largest float below 2^63 is the edge.
                if x.is_nan() || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&x) {
                    return Err(Stop::Failed);
                }
                #[expect(clippy::cast_possible_truncation, reason = "it's in range and has no fraction")]
                let n = x as i64;
                Value::Int(n)
            }
            Expr::FloatCompare(compare, left, right) => {
                let order = float_order(self.float_of(left)?, self.float_of(right)?);
                Value::Bool(match compare {
                    Compare::Less => order.is_lt(),
                    Compare::LessEq => order.is_le(),
                    Compare::Equal => order.is_eq(),
                    Compare::NotEqual => order.is_ne(),
                    Compare::Greater => order.is_gt(),
                })
            }
            Expr::Called(name) => match self.get(name)? {
                Value::Count(items) => Value::Int(sum_of(items)?),
                _ => return Err(Stop::Unsure),
            },
            Expr::Point(x, y) => {
                let (x, y) = (int(&self.expr(x)?)?, int(&self.expr(y)?)?);
                Value::Point(x, y)
            }
            Expr::Field(point, x) => {
                let Value::Point(px, py) = self.expr(point)? else { return Err(Stop::Unsure) };
                Value::Int(if *x { px } else { py })
            }
            Expr::Total(point) => {
                let Value::Point(px, py) = self.expr(point)? else { return Err(Stop::Unsure) };
                Value::Int(checked(px.checked_add(py))?)
            }
            Expr::Shape(case, number) => Value::Shape(*case, int(&self.expr(number)?)?),
            Expr::Leaf(name) => Value::Node(Tree::Leaf(self.text_of(name)?)),
            Expr::Number(value) => Value::Node(Tree::Number(int(&self.expr(value)?)?)),
            Expr::Branch(label, children) => {
                let label = self.text_of(label)?;
                Value::Node(kept(Tree::Branch(label, self.trees_of(children)?))?)
            }
            Expr::Tagged(tags, inner) => {
                let Value::Texts(tags) = self.expr(tags)? else { return Err(Stop::Unsure) };
                Value::Node(kept(Tree::Tagged(tags, self.trees_of(inner)?))?)
            }
            Expr::Texts(texts) => {
                let mut values = Vec::with_capacity(texts.len());
                for text in texts {
                    values.push(self.text_of(text)?);
                }
                Value::Texts(values)
            }
            Expr::NodeList(nodes) => {
                let mut trees = Vec::with_capacity(nodes.len());
                for node in nodes {
                    trees.push(self.tree_of(node)?);
                }
                Value::Nodes(trees)
            }
            Expr::NodeAt(nodes, at) => {
                let (trees, at) = (self.trees_of(nodes)?, int(&self.expr(at)?)?);
                let len = i64::try_from(trees.len()).map_err(|_| Stop::Unsure)?;
                if len == 0 {
                    Value::Node(Tree::Leaf("none".to_owned()))
                } else {
                    let at = usize::try_from(floor_rem(at, len)?).map_err(|_| Stop::Unsure)?;
                    Value::Node(trees.get(at).cloned().ok_or(Stop::Unsure)?)
                }
            }
            Expr::Digest(node) => {
                let mut text = String::new();
                self.tree_of(node)?.digest(&mut text);
                Value::Str(text)
            }
            Expr::DigestAll(nodes) => {
                let mut text = "[".to_owned();
                for tree in self.trees_of(nodes)? {
                    tree.digest(&mut text);
                    text.push(';');
                }
                text.push(']');
                Value::Str(text)
            }
            Expr::NamedLeaf(name) => Value::Named(Named::Leaf(self.text_of(name)?)),
            Expr::NamedPair(left, right, label, tags) => {
                let (left, right) = (self.named_of(left)?, self.named_of(right)?);
                let label = self.text_of(label)?;
                let Value::Texts(tags) = self.expr(tags)? else { return Err(Stop::Unsure) };
                Value::Named(kept_named(Named::Pair(Box::new(left), Box::new(right), label, tags))?)
            }
            Expr::Chain(depth, label) => {
                let label = self.text_of(label)?;
                let depth = (*depth).max(0);
                if depth > 64 {
                    // Only measured, as the program only ever measures one this deep.
                    Value::Chain(depth)
                } else {
                    let mut named = Named::Leaf(label.clone());
                    for at in 0..depth {
                        let right = Named::Leaf(format!("<{at}>"));
                        named =
                            Named::Pair(Box::new(named), Box::new(right), label.clone(), vec![label.clone()]);
                    }
                    Value::Named(named)
                }
            }
            Expr::NamedDigest(named) => {
                let mut text = String::new();
                self.named_of(named)?.digest(&mut text);
                Value::Str(text)
            }
            Expr::NamedDepth(named) => match self.expr(named)? {
                Value::Chain(depth) => Value::Int(depth),
                Value::Named(mut at) => {
                    let mut depth = 0;
                    while let Named::Pair(left, ..) = at {
                        at = *left;
                        depth += 1;
                    }
                    Value::Int(depth)
                }
                _ => return Err(Stop::Unsure),
            },
            Expr::Deep(depth, label) => {
                // The label is worked out, for anything it does, though only the depth is kept.
                let _ = self.text_of(label)?;
                Value::Deep((*depth).max(0))
            }
            Expr::DepthOf(node) => match self.expr(node)? {
                Value::Deep(depth) => Value::Int(depth),
                Value::Node(mut tree) => {
                    let mut depth = 0;
                    while let Tree::Branch(_, children) = tree {
                        let Some(first) = children.into_iter().next() else { break };
                        tree = first;
                        depth += 1;
                    }
                    Value::Int(depth)
                }
                _ => return Err(Stop::Unsure),
            },
            Expr::Area(shape) => {
                let Value::Shape(case, number) = self.expr(shape)? else { return Err(Stop::Unsure) };
                Value::Int(match case {
                    Case::Circle => checked(checked(number.checked_mul(number))?.checked_mul(3))?,
                    Case::Square => checked(number.checked_mul(number))?,
                    Case::Empty => 0,
                })
            }
            Expr::Convert(size, value) => {
                Value::Sized(*size, fitting(*size, i128::from(int(&self.expr(value)?)?))?)
            }
            Expr::SizedArith(size, operator, left, right) => {
                let (left, right) = (self.sized_of(left)?, self.sized_of(right)?);
                let worked = match operator {
                    Arith::Sub => left.checked_sub(right),
                    Arith::Mul => left.checked_mul(right),
                    _ => left.checked_add(right),
                };
                Value::Sized(*size, fitting(*size, worked.ok_or(Stop::Failed)?)?)
            }
            Expr::SizedWrap(size, left, right) => {
                let (left, right) = (self.sized_of(left)?, self.sized_of(right)?);
                Value::Sized(*size, wrapped(*size, left + right))
            }
            Expr::IntOf(sized) => Value::Int(i64::try_from(self.sized_of(sized)?).map_err(|_| Stop::Failed)?),
            Expr::SumCounted(limit) => {
                let limit = int(&self.expr(limit)?)?;
                let mut total: i64 = 0;
                for at in 0..limit.max(0) {
                    total = checked(total.checked_add(checked(at.checked_mul(3))?))?;
                }
                Value::Int(total)
            }
            Expr::Count(name) => match self.get(name)? {
                Value::Tally(count) => Value::Int(*count),
                _ => return Err(Stop::Unsure),
            },
            Expr::Bump(name, by) => {
                let by = int(&self.expr(by)?)?;
                let held = int(self.get(name)?)?;
                let bumped = checked(held.checked_add(by))?;
                *self.slot(name)? = Value::Int(bumped);
                Value::Int(checked(bumped.checked_mul(10))?)
            }
        })
    }
}

/// What `FloatKeys` prints: how many keys and set items there are, the last place given the key
/// `values[probe]` is, whether `FLOATS[asked]` is in the set, then `==` and `!=` on `pair`.
fn float_keys(held: FloatHeld, values: &[usize], probe: usize, asked: usize, pair: (usize, usize)) -> String {
    let key = |value: usize| FLOATS.get(value).map_or(0, |(_, key)| *key);
    let at = |place: usize| key(values.get(place).copied().unwrap_or(0));
    let mut keys: Vec<u8> = values.iter().map(|value| key(*value)).collect();
    keys.sort_unstable();
    keys.dedup();
    let marked = usize::from(matches!(held, FloatHeld::Marked));
    let found = values.iter().rposition(|value| key(*value) == at(probe)).unwrap_or(0);
    let asked = keys.contains(&key(asked));
    let same = at(pair.0) == at(pair.1);
    format!(
        "{} {found} {} {} {} {}\n",
        keys.len(),
        keys.len() + marked,
        shown(&Value::Bool(asked)),
        shown(&Value::Bool(same)),
        shown(&Value::Bool(!same))
    )
}

#[cfg(test)]
mod tests {
    use super::float_text;

    /// What `stdout.print` wrote for each of these floats, built natively.
    #[test]
    fn floats_print_as_fennec_prints_them() {
        let cases = [
            (1.0 / 3.0, "0.3333333333333333"),
            (100.0, "100.0"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-1.5e-10, "-1.5e-10"),
            (1_152_921_504_606_846_976.0, "1.152921504606847e+18"),
            (123_456_789_012_345_680.0, "1.2345678901234568e+17"),
            (0.1 + 0.7, "0.7999999999999999"),
            (1e22, "1e+22"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (9_007_199_254_740_992.0, "9007199254740992.0"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (123_456_789.125, "123456789.125"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
        ];
        for (x, written) in cases {
            assert_eq!(float_text(x), written, "{x:e}");
        }
    }
}
