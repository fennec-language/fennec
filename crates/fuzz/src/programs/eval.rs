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

use super::ast::{Arith, Case, Compare, Expr, Place, Program, Stmt};

/// A value while the program runs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    Ints(Vec<i64>),
    Point(i64, i64),
    Shape(Case, i64),
    Byte(u8),
    /// A `shared Point`, by where it is on the heap.
    Shared(usize),
    Tally(i64),
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
    let mut machine = Machine { scopes: vec![Vec::new()], out: String::new(), heap: Vec::new() };
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
        Value::Byte(n) => n.to_string(),
        Value::Tally(count) => format!("Tally(count={count})"),
        Value::Shared(_) => "shared".to_owned(),
    }
}

impl Machine {
    fn byte_of(&mut self, expr: &Expr) -> Ran<u8> {
        match self.expr(expr)? {
            Value::Byte(n) => Ok(n),
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
            Stmt::PrintShared(name) => {
                let Value::Shared(at) = self.get(name)? else { return Err(Stop::Unsure) };
                let (x, y) = self.heap.get(*at).copied().ok_or(Stop::Unsure)?;
                let _ = writeln!(self.out, "Point(x={x}, y={y})");
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
            new = Value::Int(checked(int(&old)?.checked_add(int(&new)?))?);
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
            Expr::Mapped(list, by, plus) => {
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
                let Value::Ints(items) = self.expr(list)? else { return Err(Stop::Unsure) };
                Value::Int(i64::try_from(items.len()).map_err(|_| Stop::Unsure)?)
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
                // Overflowing partway but not at the end could go either way.
                let mut total: i64 = 0;
                let mut overflowed = false;
                for item in &items {
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
                Value::Int(total)
            }
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
            Expr::Area(shape) => {
                let Value::Shape(case, number) = self.expr(shape)? else { return Err(Stop::Unsure) };
                Value::Int(match case {
                    Case::Circle => checked(checked(number.checked_mul(number))?.checked_mul(3))?,
                    Case::Square => checked(number.checked_mul(number))?,
                    Case::Empty => 0,
                })
            }
            Expr::Byte(value) => {
                let value = int(&self.expr(value)?)?;
                Value::Byte(u8::try_from(value).map_err(|_| Stop::Failed)?)
            }
            Expr::ByteAdd(left, right) => {
                let (left, right) = (self.byte_of(left)?, self.byte_of(right)?);
                Value::Byte(left.checked_add(right).ok_or(Stop::Failed)?)
            }
            Expr::ByteWrap(left, right) => {
                let (left, right) = (self.byte_of(left)?, self.byte_of(right)?);
                Value::Byte(left.wrapping_add(right))
            }
            Expr::IntOf(byte) => Value::Int(i64::from(self.byte_of(byte)?)),
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
