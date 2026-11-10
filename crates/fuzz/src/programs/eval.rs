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
    Amount, Arith, BufOp, Case, Compare, Expr, Nested, Op, Place, Program, RefPlace, Size, Stmt,
};

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
        match self {
            Named::Leaf(name) => {
                let _ = write!(text, "l({name})");
            }
            Named::Pair(left, right, label, tags) => {
                let _ = write!(text, "p({label}");
                for tag in tags {
                    text.push(',');
                    text.push_str(tag);
                }
                text.push(' ');
                left.digest(text);
                text.push(' ');
                right.digest(text);
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

/// How a program ran: what it printed, and whether it ran to the end, or None where the evaluator
/// can't be sure.
pub(crate) fn run(program: &Program) -> Option<(String, bool)> {
    let mut machine = Machine {
        scopes: vec![Vec::new()],
        out: String::new(),
        heap: Vec::new(),
        bags: Vec::new(),
        outers: Vec::new(),
    };
    match machine.block(&program.body) {
        Ok(()) => {
            machine.out.push_str("done\n");
            Some((machine.out, true))
        }
        Err(Stop::Failed) => Some((machine.out, false)),
        // A `break` or `continue` outside a loop never gets written.
        Err(Stop::Unsure | Stop::Break | Stop::Continue) => None,
    }
}

struct Machine {
    /// Each block's names and values, innermost last.
    scopes: Vec<Vec<(String, Value)>>,
    out: String,
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
                Value::Int(sum_of(&terms)?)
            }
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
