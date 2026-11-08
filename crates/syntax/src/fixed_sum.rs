//! How `sum` adds floats when an engine writes a sum out as a loop, over a generator or a range:
//! in the order `math.fixed_sum` adds a list's, so a sum gives the same bits whatever it's over,
//! on every engine and platform. Item `at` goes into running total `at % 8`, and the eight are
//! added pairwise at the end, then added to the start. Ints are added one by one, as before, since
//! their sum is exact.
//!
//! [`unrolled`] writes a sum with no `if` over a `range` or a list eight items a round, each into
//! its own total, so the eight adds of a round don't wait on each other. [`fixed_sum`] writes any
//! other sum one item at a time: the eight totals turn once per item, so the next item's total is
//! always the first, and after the last item they turn the rest of the way round, so each is back
//! in its place before they're added.
//!
//! What's written is names, literals, `+=`, comparisons, sums of ints and, over a list, its items
//! and length. Each of those has a place of its own, in [`Places`], for an engine to type it by.

use crate::ast::{BinOp, Expr, ExprKind, Stmt, StmtKind};
use crate::span::Span;

/// How many running totals a float sum keeps.
pub const LANES: usize = 8;

/// The same, as an int written in the code.
const LANES_WRITTEN: i64 = 8;

/// How the eight totals are added at the end: (0 + 1) + (2 + 3), (4 + 5) + (6 + 7), then the two.
const PAIRWISE: [(usize, usize); 7] = [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (4, 6), (0, 4)];

/// Where what's written is, so an engine can type what it needs to: `at` holds what needs no
/// type, `compared` each comparison, a `bool`, `counted` each sum of ints, and `item` each item
/// read from a list, as the list's items are typed.
#[derive(Clone, Copy)]
pub struct Places {
    pub at: Span,
    pub compared: Span,
    pub counted: Span,
    pub item: Span,
}

/// Writes the code of a sum into `total`, with hidden names of its own beside it.
struct Writer<'t> {
    total: &'t str,
    places: Places,
}

impl Writer<'_> {
    fn hidden(&self, suffix: &str) -> String {
        format!("{}\0{suffix}", self.total)
    }

    fn lane(&self, k: usize) -> String {
        self.hidden(&format!("lane{k}"))
    }

    fn read(&self, name: &str) -> Expr {
        Expr { kind: ExprKind::Name(name.to_owned()), span: self.places.at }
    }

    fn stmt(&self, kind: StmtKind) -> Stmt {
        Stmt { kind, span: self.places.at }
    }

    /// `target op= value`. A name made here is typed from its value, which an engine finds at the
    /// value's span.
    fn assign(&self, target: &str, op: Option<BinOp>, value: Expr, declares: bool) -> Stmt {
        let target = Expr { kind: ExprKind::Name(target.to_owned()), span: value.span };
        self.stmt(StmtKind::Assign { target, op, value, declare_mut: declares, ty: None })
    }

    fn int(&self, value: i64) -> Expr {
        Expr { kind: ExprKind::Int(value), span: self.places.at }
    }

    fn binary(op: BinOp, lhs: Expr, rhs: Expr, span: Span) -> Expr {
        Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span }
    }

    /// `total` given the start, and each total zero.
    fn started(&self, start: Expr) -> Vec<Stmt> {
        let mut written = vec![self.assign(self.total, None, start, true)];
        for k in 0..LANES {
            let zero = Expr { kind: ExprKind::Float(0.0), span: self.places.at };
            written.push(self.assign(&self.lane(k), None, zero, true));
        }
        written
    }

    /// The totals added pairwise, into `total`.
    fn finished(&self) -> Vec<Stmt> {
        let mut written: Vec<Stmt> = PAIRWISE
            .iter()
            .map(|(into, from)| {
                self.assign(&self.lane(*into), Some(BinOp::Add), self.read(&self.lane(*from)), false)
            })
            .collect();
        written.push(self.assign(self.total, Some(BinOp::Add), self.read(&self.lane(0)), false));
        written
    }
}

/// A float sum written one item at a time: `setup` gives `total` the start and the totals their
/// zeros, a loop runs `each` for each item that counts, and `finish` adds the totals into `total`.
pub struct FixedSum {
    pub setup: Vec<Stmt>,
    pub each: Vec<Stmt>,
    pub finish: Vec<Stmt>,
}

/// The statements of a float sum into `total`, starting at `start`, adding `item` once each.
/// Only `places.at` and `places.compared` are used.
pub fn fixed_sum(total: &str, start: Expr, item: &Expr, places: Places) -> FixedSum {
    let writer = Writer { total, places };
    let (turn, count) = (writer.hidden("turn"), writer.hidden("count"));
    let compare =
        |op: BinOp, value: i64| Writer::binary(op, writer.read(&count), writer.int(value), places.compared);
    // The totals turn once: the first goes to the back, holding `added` more.
    let turned = |added: Option<&Expr>| {
        let mut turning = vec![writer.assign(&turn, None, writer.read(&writer.lane(0)), false)];
        if let Some(added) = added {
            turning.push(writer.assign(&turn, Some(BinOp::Add), added.clone(), false));
        }
        for k in 0..LANES - 1 {
            turning.push(writer.assign(&writer.lane(k), None, writer.read(&writer.lane(k + 1)), false));
        }
        turning.push(writer.assign(&writer.lane(LANES - 1), None, writer.read(&turn), false));
        turning.push(writer.assign(&count, Some(BinOp::Add), writer.int(1), false));
        let wrapped = vec![writer.assign(&count, None, writer.int(0), false)];
        turning.push(writer.stmt(StmtKind::If {
            branches: vec![(compare(BinOp::Eq, LANES_WRITTEN), wrapped)],
            otherwise: None,
        }));
        turning
    };
    let mut setup = writer.started(start);
    let zero = Expr { kind: ExprKind::Float(0.0), span: places.at };
    setup.push(writer.assign(&turn, None, zero, true));
    setup.push(writer.assign(&count, None, writer.int(0), true));
    // The rest of the way round, then pairwise.
    let mut finish =
        vec![writer.stmt(StmtKind::While { cond: compare(BinOp::NotEq, 0), body: turned(None) })];
    finish.extend(writer.finished());
    FixedSum { setup, each: turned(Some(item)), finish }
}

/// What a sum with no `if` goes over, when its places are known: a `range` counting up by one,
/// from `first` or 0 to `limit`, or a list.
pub enum Over {
    Range { first: Option<Expr>, limit: Expr },
    List(Expr),
}

/// The same sum, written out eight items a round, each straight into its own total, then the
/// last few one by one into the first totals: the order of [`fixed_sum`] with nothing to turn
/// or count. `name` is the loop's name, which `item` reads.
pub fn unrolled(
    total: &str,
    start: Expr,
    (name, item): (&str, &Expr),
    over: &Over,
    places: Places,
) -> Vec<Stmt> {
    let writer = Writer { total, places };
    let (place, end, items) = (writer.hidden("at"), writer.hidden("end"), writer.hidden("items"));
    let mut written = writer.started(start);
    // The list, read where it's written, so its type is the list's.
    let the_list = |list: &Expr| Expr { kind: ExprKind::Name(items.clone()), span: list.span };
    // What's `k` past where the round is.
    let value_at = |k: i64| {
        let offset = if k == 0 {
            Expr { kind: ExprKind::Name(place.clone()), span: places.counted }
        } else {
            Writer::binary(BinOp::Add, writer.read(&place), writer.int(k), places.counted)
        };
        match over {
            Over::Range { .. } => offset,
            Over::List(list) => Expr {
                kind: ExprKind::Index { object: Box::new(the_list(list)), index: Box::new(offset) },
                span: places.item,
            },
        }
    };
    match over {
        Over::Range { first, limit } => {
            written.push(writer.assign(&place, None, first.clone().unwrap_or_else(|| writer.int(0)), true));
            written.push(writer.assign(&end, None, limit.clone(), false));
        }
        Over::List(list) => {
            written.push(writer.assign(&items, None, list.clone(), false));
            written.push(writer.assign(&place, None, writer.int(0), true));
            let method =
                ExprKind::Attr { object: Box::new(the_list(list)), name: "len".to_owned(), optional: false };
            let callee = Box::new(Expr { kind: method, span: places.at });
            let length = Expr { kind: ExprKind::Call { callee, args: Vec::new() }, span: places.counted };
            written.push(writer.assign(&end, None, length, false));
        }
    }
    let mut round = Vec::new();
    for (k, offset) in (0..LANES).zip(0_i64..) {
        // The loop's name is made again for each item, as a loop makes it for each round.
        round.push(writer.assign(name, None, value_at(offset), true));
        round.push(writer.assign(&writer.lane(k), Some(BinOp::Add), item.clone(), false));
    }
    round.push(writer.assign(&place, Some(BinOp::Add), writer.int(LANES_WRITTEN), false));
    let left = Writer::binary(BinOp::Sub, writer.read(&end), writer.read(&place), places.counted);
    let full = Writer::binary(BinOp::GtEq, left, writer.int(LANES_WRITTEN), places.compared);
    written.push(writer.stmt(StmtKind::While { cond: full, body: round }));
    // At most seven are left, into the first totals in order, each only if it's there.
    let mut rest: Vec<Stmt> = Vec::new();
    for k in (0..LANES - 1).rev() {
        let mut body = vec![
            writer.assign(name, None, value_at(0), true),
            writer.assign(&writer.lane(k), Some(BinOp::Add), item.clone(), false),
            writer.assign(&place, Some(BinOp::Add), writer.int(1), false),
        ];
        body.append(&mut rest);
        let there = Writer::binary(BinOp::Lt, writer.read(&place), writer.read(&end), places.compared);
        rest = vec![writer.stmt(StmtKind::If { branches: vec![(there, body)], otherwise: None })];
    }
    written.append(&mut rest);
    written.extend(writer.finished());
    written
}

/// The same sum, for an engine that adds floats it holds: `start`, then the items in the order
/// above.
pub fn added(start: f64, items: impl IntoIterator<Item = f64>) -> f64 {
    let mut lanes = [0.0; LANES];
    for (at, item) in items.into_iter().enumerate() {
        if let Some(lane) = lanes.get_mut(at % LANES) {
            *lane += item;
        }
    }
    let [a0, a1, a2, a3, a4, a5, a6, a7] = lanes;
    start + (((a0 + a1) + (a2 + a3)) + ((a4 + a5) + (a6 + a7)))
}

#[cfg(test)]
mod tests {
    use super::added;

    /// Eight totals added pairwise lose far less than one running total on a long sum whose
    /// small items a large one would swallow, and an empty sum is its start.
    #[test]
    fn eight_totals_added_pairwise_keep_what_one_total_loses() {
        assert_eq!(added(2.5, []).to_bits(), 2.5_f64.to_bits());
        let items: Vec<f64> = (0..1_000_000).map(|at| if at % 8 == 0 { 1e8 } else { 0.1 }).collect();
        let exact = 125_000.0 * 1e8 + 875_000.0 * 0.1;
        let one_by_one: f64 = items.iter().sum();
        let fixed = added(0.0, items.iter().copied());
        assert!((fixed - exact).abs() < (one_by_one - exact).abs() / 100.0, "{fixed} against {one_by_one}");
    }
}
