//! What's known about the values a program's ints and floats can hold, for a backend to leave out
//! the checks a value in that range can't fail: the correction a floor `//` or `%` makes for a
//! negative side, `int(x)`'s stop for a float that doesn't fit or is NaN, and the NaN test a
//! float comparison makes for the total order. It works over the whole program and depends on
//! nothing but the IR, as the list lengths (`crate::lengths`) do.
//!
//! An int is known to be within a range, and a float within a range and whether it can be NaN.
//! A struct's or a tuple's value carries what's known of each of its fields or items, and a
//! list's what's known of every item in it. They come from constants; `+ - * / // %`, `min`,
//! `max`, `abs`, `int(x)`, `float(n)` and `math.sqrt` on what's known; what a branch says where it
//! holds; what every call gives a parameter; and what every `return` gives back.
//!
//! - **Floats' total order.** NaN sorts last, so it's on the false side of `x < y` and the true
//!   side of `x > y`. A branch on `at > high` that came out false says `at` isn't NaN, where
//!   neither side of `at < low` does.
//! - **Lists.** What a list holds is followed through each function from where it's made: what's
//!   appended or set joins it, and a copy holds what it held. A call lent the list with `ref`
//!   leaves in it what was there and anything the callee puts in. A callee that sets every item,
//!   with a count from 0 to the list's length in a loop it always finishes, leaves only what it
//!   put in: `integrate` setting every `positions[ball]` from `kept`.
//! - **Loops.** What a value written round a loop can reach is followed for a few rounds, then
//!   taken to have no bound on the side it was still moving. A `for` loop's count stays between
//!   its first value and its limit, as `crate::facts` has it.
//!
//! What it can't follow, it doesn't guess: a builtin it doesn't know gives anything, and so does a
//! function something other than a call by name may call, like `main`, a closure, a task or a
//! generator, for its parameters.
//!
//! What a backend can drop, by the place of its instruction in its function, is in
//! [`FunctionRanges`]. A wrong one turns a negative side's floor into the wrong number, lets
//! `int(x)` give a wrong int, or puts a NaN in the wrong place in the order, so each is checked
//! as a program runs in the IR's evaluator (`crate::eval::check_ranges`), and a build that checks
//! proofs checks them too.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::{BinOp, UnaryOp};

use crate::lengths::{called_other_ways, every};
use crate::{Block, Function, InstKind, LocalId, Operand, Program, Terminator};

/// How many times a fact grows before what's still moving in it is widened.
const GROWTHS_BEFORE_WIDENING: usize = 4;

/// How many rounds there are at most, widening included: past them, what's still moving is
/// taken as unknown.
const MOST_ROUNDS: usize = 24;

/// The changes to a list that keep its items.
const KEEPS_ITEMS: [&str; 2] = ["list.sort", "list.reverse"];

/// The largest float that `int(x)` can take without overflowing: 2^63 is just past `i64::MAX`.
const INT_CEILING: f64 = 9_223_372_036_854_775_808.0;

/// What's known about a value.
#[derive(Clone, Debug, PartialEq)]
pub enum Fact {
    /// Nothing reaches here yet.
    Unset,
    /// An int from the first to the second, both included.
    Int(i64, i64),
    Float(Float),
    /// A struct's fields, in the order they're declared.
    Struct(Vec<Fact>),
    /// A tuple's items.
    Tuple(Vec<Fact>),
    /// A list's items, each of which is this.
    List(Box<Fact>),
    /// Nothing is known.
    Any,
}

/// What's known about a float: it's from `low` to `high`, both included, which may be the
/// infinities, and it may be NaN only if `nan` says so.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Float {
    pub low: f64,
    pub high: f64,
    pub nan: bool,
}

impl Float {
    /// Any float at all.
    pub const ANY: Float = Float { low: f64::NEG_INFINITY, high: f64::INFINITY, nan: true };

    fn exactly(value: f64) -> Float {
        if value.is_nan() {
            Float { low: f64::INFINITY, high: f64::NEG_INFINITY, nan: true }
        } else {
            Float { low: value, high: value, nan: false }
        }
    }

    /// Whether it holds no number, only NaN or nothing.
    fn empty(self) -> bool {
        self.low > self.high
    }

    fn join(self, other: Float) -> Float {
        if self.empty() {
            return Float { nan: self.nan || other.nan, ..other };
        }
        if other.empty() {
            return Float { nan: self.nan || other.nan, ..self };
        }
        Float { low: self.low.min(other.low), high: self.high.max(other.high), nan: self.nan || other.nan }
    }

    /// Whether it can be the infinity of this sign.
    fn reaches(self, infinity: f64) -> bool {
        !self.empty() && (self.low.total_cmp(&infinity).is_eq() || self.high.total_cmp(&infinity).is_eq())
    }

    /// Whether it can be zero.
    fn holds_zero(self) -> bool {
        !self.empty() && self.low <= 0.0 && self.high >= 0.0
    }
}

/// Every product of one bound of each side, rounded as the float operation rounds, so the least
/// and the most of them bound what the operation gives on any values between.
fn bounds_of(products: [f64; 4]) -> (f64, f64) {
    let numbers: Vec<f64> = products.into_iter().filter(|value| !value.is_nan()).collect();
    let low = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let high = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (low, high)
}

impl Fact {
    /// What's known about either of two values.
    #[must_use]
    pub fn join(&self, other: &Fact) -> Fact {
        match (self, other) {
            (Fact::Unset, known) | (known, Fact::Unset) => known.clone(),
            (Fact::Int(low, high), Fact::Int(other_low, other_high)) => {
                Fact::Int(*low.min(other_low), *high.max(other_high))
            }
            (Fact::Float(first), Fact::Float(second)) => Fact::Float(first.join(*second)),
            (Fact::Struct(first), Fact::Struct(second)) if first.len() == second.len() => {
                Fact::Struct(first.iter().zip(second).map(|(one, other)| one.join(other)).collect())
            }
            (Fact::Tuple(first), Fact::Tuple(second)) if first.len() == second.len() => {
                Fact::Tuple(first.iter().zip(second).map(|(one, other)| one.join(other)).collect())
            }
            (Fact::List(first), Fact::List(second)) => Fact::List(Box::new(first.join(second))),
            _ => Fact::Any,
        }
    }

    /// What's known once a value that was `self` has also been `newer`, with every bound that
    /// moved taken as far as it can go, so a value growing round a loop is settled in one step.
    #[must_use]
    fn widen(&self, newer: &Fact) -> Fact {
        match (self, newer) {
            (Fact::Unset, known) | (known, Fact::Unset) => known.clone(),
            (Fact::Int(low, high), Fact::Int(new_low, new_high)) => Fact::Int(
                if new_low < low { i64::MIN } else { *low },
                if new_high > high { i64::MAX } else { *high },
            ),
            (Fact::Float(old), Fact::Float(new)) => {
                if old.empty() {
                    return Fact::Float(*new);
                }
                Fact::Float(Float {
                    low: if new.low < old.low { f64::NEG_INFINITY } else { old.low },
                    high: if new.high > old.high { f64::INFINITY } else { old.high },
                    nan: old.nan || new.nan,
                })
            }
            (Fact::Struct(old), Fact::Struct(new)) if old.len() == new.len() => {
                Fact::Struct(old.iter().zip(new).map(|(one, other)| one.widen(other)).collect())
            }
            (Fact::Tuple(old), Fact::Tuple(new)) if old.len() == new.len() => {
                Fact::Tuple(old.iter().zip(new).map(|(one, other)| one.widen(other)).collect())
            }
            (Fact::List(old), Fact::List(new)) => Fact::List(Box::new(old.widen(new))),
            _ => Fact::Any,
        }
    }

    /// What's known about an int, as a range, if it's an int.
    #[must_use]
    pub fn int(&self) -> Option<(i64, i64)> {
        match self {
            Fact::Int(low, high) => Some((*low, *high)),
            _ => None,
        }
    }

    /// What's known about a float, if it's a float.
    #[must_use]
    pub fn float(&self) -> Option<Float> {
        match self {
            Fact::Float(known) => Some(*known),
            _ => None,
        }
    }

    /// What's known of a struct's field or a tuple's item, by its place.
    fn part(&self, at: Option<usize>) -> Fact {
        match (self, at) {
            (Fact::Struct(parts) | Fact::Tuple(parts), Some(at)) => {
                parts.get(at).cloned().unwrap_or(Fact::Any)
            }
            (Fact::Unset, _) => Fact::Unset,
            _ => Fact::Any,
        }
    }

    /// What's known about a list's items, if it's a list.
    fn items(&self) -> Fact {
        match self {
            Fact::List(items) => (**items).clone(),
            Fact::Unset => Fact::Unset,
            _ => Fact::Any,
        }
    }

    /// The fact as a float's: a float, or any float for what isn't known.
    fn as_float(&self) -> Option<Float> {
        match self {
            Fact::Float(known) => Some(*known),
            Fact::Any => Some(Float::ANY),
            _ => None,
        }
    }

    /// The fact as an int's: an int, or any int for what isn't known.
    fn as_int(&self) -> Option<(i64, i64)> {
        match self {
            Fact::Int(low, high) => Some((*low, *high)),
            Fact::Any => Some((i64::MIN, i64::MAX)),
            _ => None,
        }
    }
}

/// An int's range from an `i128` one, clamped to what an int holds: an operation that would go
/// past stops the program, so what it gives when it doesn't is an int.
fn clamped(low: i128, high: i128) -> Fact {
    let clamp = |value: i128| i64::try_from(value).unwrap_or(if value < 0 { i64::MIN } else { i64::MAX });
    Fact::Int(clamp(low), clamp(high))
}

/// An int converted to a float, which rounds past 2^53.
#[expect(clippy::cast_precision_loss, reason = "a bound past 2^53 is widened by the caller")]
fn int_as_float(value: i64) -> f64 {
    value as f64
}

/// A float in an int's range, truncated as `int(x)` truncates.
#[expect(clippy::cast_possible_truncation, reason = "the caller checks the float fits")]
fn truncated(value: f64) -> i64 {
    value.trunc() as i64
}

/// Where an instruction is in its function: its block, and its place in the block.
pub type Place = (usize, usize);

/// What's known about one function's values, and what its checks can leave out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FunctionRanges {
    /// What's known about each local that isn't a list, whatever writes it.
    locals: Vec<Fact>,
    /// Each floor `//` and `%` whose sides are both known not negative.
    unsigned: BTreeSet<Place>,
    /// Each `int(x)` whose float is known to fit an int and not be NaN.
    fits: BTreeSet<Place>,
    /// Each float comparison whose sides are both known not to be NaN.
    not_nan: BTreeSet<Place>,
}

impl FunctionRanges {
    /// What's known about a local that isn't a list, wherever it's read.
    #[must_use]
    pub fn local(&self, local: LocalId) -> Option<&Fact> {
        self.locals.get(local.0).filter(|fact| !matches!(fact, Fact::Any | Fact::Unset))
    }

    /// What's known about a local that isn't a list as it's written, `Fact::Unset` included: a
    /// value written to a local that's unset says something's wrong.
    #[must_use]
    pub fn written(&self, local: LocalId) -> Option<&Fact> {
        self.locals.get(local.0)
    }

    /// Whether the floor `//` or `%` at this place has both sides known not negative, so it's
    /// the same as the unsigned operation.
    #[must_use]
    pub fn unsigned(&self, place: Place) -> bool {
        self.unsigned.contains(&place)
    }

    /// Whether the `int(x)` at this place takes a float known to fit an int and not be NaN.
    #[must_use]
    pub fn fits(&self, place: Place) -> bool {
        self.fits.contains(&place)
    }

    /// Whether the float comparison at this place has both sides known not to be NaN.
    #[must_use]
    pub fn not_nan(&self, place: Place) -> bool {
        self.not_nan.contains(&place)
    }

    /// The `//` and `%` known unsigned, the `int(x)` known to fit and the comparisons known not
    /// NaN, by place.
    pub fn places(&self) -> (&BTreeSet<Place>, &BTreeSet<Place>, &BTreeSet<Place>) {
        (&self.unsigned, &self.fits, &self.not_nan)
    }
}

/// What a function leaves in a list it's lent with `ref`.
#[derive(Clone, Debug, PartialEq)]
struct Leaves {
    /// What it puts in.
    puts: Fact,
    /// Whether it sets every item that was there, so only what it puts in is left.
    every: bool,
}

/// What's known about the values of a program's ints and floats.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ranges {
    functions: BTreeMap<String, FunctionRanges>,
}

impl Ranges {
    /// What's known about one function's values.
    #[must_use]
    pub fn function(&self, name: &str) -> Option<&FunctionRanges> {
        self.functions.get(name)
    }

    /// Every function's, by name.
    pub fn functions(&self) -> impl Iterator<Item = (&str, &FunctionRanges)> {
        self.functions.iter().map(|(name, ranges)| (name.as_str(), ranges))
    }

    /// Works out what's known about a program's values.
    #[must_use]
    pub fn of(program: &Program) -> Self {
        let functions: HashMap<&str, &Function> =
            every(program).map(|function| (function.name.as_str(), function)).collect();
        let other_ways = called_other_ways(program);
        let sets_every: HashMap<&str, Vec<bool>> =
            functions.iter().map(|(name, function)| (*name, sets_every_item(function))).collect();
        let mut world = World {
            program,
            functions: &functions,
            params: BTreeMap::new(),
            returns: BTreeMap::new(),
            leaves: BTreeMap::new(),
            sets_every: &sets_every,
            other_ways: &other_ways,
            grew: BTreeMap::new(),
        };
        let mut found = BTreeMap::new();
        for _ in 0..MOST_ROUNDS {
            let mut next = Next::default();
            found.clear();
            for function in every(program) {
                let ranges = world.function(function, &mut next);
                found.insert(function.name.clone(), ranges);
            }
            if !world.take(next) {
                return Ranges { functions: found };
            }
        }
        // Still moving after every round: nothing worked out can be trusted.
        Ranges::default()
    }
}

/// What's known across functions, round by round.
struct World<'p> {
    program: &'p Program,
    functions: &'p HashMap<&'p str, &'p Function>,
    /// What every call gives each parameter, by function.
    params: BTreeMap<String, Vec<Fact>>,
    /// What every `return` gives back, by function.
    returns: BTreeMap<String, Fact>,
    /// What each function leaves in each list it's lent with `ref`, by function and parameter.
    leaves: BTreeMap<String, Vec<Option<Leaves>>>,
    sets_every: &'p HashMap<&'p str, Vec<bool>>,
    other_ways: &'p HashSet<&'p str>,
    /// How many times each fact across functions has grown, by what it's of, for widening.
    grew: BTreeMap<(String, usize, u8), usize>,
}

/// What one round found, for the next.
#[derive(Default)]
struct Next {
    params: BTreeMap<String, Vec<Fact>>,
    returns: BTreeMap<String, Fact>,
    puts: BTreeMap<String, Vec<Fact>>,
}

impl World<'_> {
    /// Takes what a round found, joined with what was known, widening each fact once it's grown
    /// a few times. Says whether anything changed.
    fn take(&mut self, next: Next) -> bool {
        let mut changed = false;
        let grew = &mut self.grew;
        let mut merge = |key: (String, usize, u8), old: &mut Fact, new: &Fact| {
            let joined = old.join(new);
            if joined == *old {
                return;
            }
            let times = grew.entry(key).or_insert(0);
            *times += 1;
            *old = if *times > GROWTHS_BEFORE_WIDENING { old.widen(&joined) } else { joined };
            changed = true;
        };
        for (name, facts) in next.params {
            let known = self.params.entry(name.clone()).or_default();
            if known.len() < facts.len() {
                known.resize(facts.len(), Fact::Unset);
            }
            for (at, (old, new)) in known.iter_mut().zip(&facts).enumerate() {
                merge((name.clone(), at, 0), old, new);
            }
        }
        for (name, fact) in next.returns {
            let old = self.returns.entry(name.clone()).or_insert(Fact::Unset);
            merge((name, 0, 1), old, &fact);
        }
        for (name, puts) in next.puts {
            let every = self.sets_every.get(name.as_str()).cloned().unwrap_or_default();
            let known = self.leaves.entry(name.clone()).or_default();
            if known.len() < puts.len() {
                known.resize(puts.len(), None);
            }
            for (at, (old, new)) in known.iter_mut().zip(&puts).enumerate() {
                let leaves = old.get_or_insert_with(|| Leaves {
                    puts: Fact::Unset,
                    every: every.get(at).copied().unwrap_or(false),
                });
                merge((name.clone(), at, 2), &mut leaves.puts, new);
            }
        }
        changed
    }

    /// What's known of a parameter of a function, from every call that gives it one.
    fn param(&self, function: &Function, at: usize) -> Fact {
        if self.other_ways.contains(function.name.as_str()) {
            return Fact::Any;
        }
        self.params.get(&function.name).and_then(|params| params.get(at)).cloned().unwrap_or(Fact::Unset)
    }

    /// Works out what's known in one function, given what's known across functions, and puts
    /// what its calls give their callees, what it gives back, and what it puts in the lists it's
    /// lent, in `next`.
    fn function(&self, function: &Function, next: &mut Next) -> FunctionRanges {
        let mut walk = Walk::new(self, function);
        for _ in 0..MOST_ROUNDS {
            if !walk.round(false) {
                break;
            }
        }
        // The last round, with nothing moving, writes what's found.
        walk.round(true);
        walk.hand_over(next);
        walk.found
    }
}

/// What's known about each list local where a block starts or ends.
type Lists = BTreeMap<usize, Fact>;

/// One function's walk, round after round until nothing moves.
struct Walk<'w, 'p> {
    world: &'w World<'p>,
    function: &'w Function,
    /// What each list local holds where each block ends.
    ends: Vec<Option<Lists>>,
    /// How many times each local's fact grew, for widening.
    grew: Vec<usize>,
    /// How many instructions write each local, counting what changes one in place.
    writes: Vec<usize>,
    /// What branches say in each block: the comparisons that came out each way on the only way in.
    narrowed: Vec<Vec<(BinOp, Operand, Operand, bool)>>,
    /// Each loop count's local, with its limit's and whether it counts down.
    counts: BTreeMap<usize, (LocalId, bool)>,
    /// What's known about each count's first value: every write to it but its step.
    starts: BTreeMap<usize, Fact>,
    found: FunctionRanges,
    /// What each call gives each callee's parameters, what's given back, and what's put in each
    /// list parameter lent with `ref`.
    calls: BTreeMap<String, Vec<Fact>>,
    returned: Fact,
    puts: Vec<Fact>,
    /// Whether this is the last round, which notes what checks can be left out, what calls
    /// give, and what's given back.
    noting: bool,
}

impl<'w, 'p> Walk<'w, 'p> {
    fn new(world: &'w World<'p>, function: &'w Function) -> Self {
        let changes = |callee: &str| {
            world.functions.get(callee).map(|callee| callee.mut_params.clone()).unwrap_or_default()
        };
        let facts = crate::facts::Facts::of(function, &changes);
        let writes = (0..function.locals.len())
            .map(|at| if facts.holds_one_value(LocalId(at)) { 1 } else { 2 })
            .collect();
        let mut found =
            FunctionRanges { locals: vec![Fact::Unset; function.locals.len()], ..FunctionRanges::default() };
        for at in 0..function.params {
            if let Some(slot) = found.locals.get_mut(at) {
                *slot = world.param(function, at);
            }
        }
        Walk {
            world,
            function,
            ends: vec![None; function.blocks.len()],
            grew: vec![0; function.locals.len()],
            writes,
            narrowed: narrowings(function),
            counts: function.counts.iter().map(|count| (count.count.0, (count.stop, count.down))).collect(),
            starts: BTreeMap::new(),
            found,
            calls: BTreeMap::new(),
            returned: Fact::Unset,
            puts: vec![Fact::Unset; function.params],
            noting: false,
        }
    }

    /// Whether a local is a list, whose items are followed through the function.
    fn is_list(&self, local: LocalId) -> bool {
        self.function.locals.get(local.0).is_some_and(|known| matches!(known.ty, Type::List(_)))
    }

    /// One walk over every block. With `last`, it also notes what checks can be left out, what
    /// calls give, and what's given back. Says whether anything moved.
    fn round(&mut self, last: bool) -> bool {
        let mut moved = false;
        self.noting = last;
        if last {
            self.calls.clear();
            self.returned = Fact::Unset;
            self.puts = vec![Fact::Unset; self.function.params];
        }
        for at in 0..self.function.blocks.len() {
            let Some(block) = self.function.blocks.get(at) else { continue };
            let mut lists = self.entering(at);
            for (place, inst) in block.insts.iter().enumerate() {
                moved |= self.step(&inst.kind, &mut lists, (at, place));
            }
            if last && let Terminator::Return(Some(value)) = &block.end {
                let given = self.read(value, &lists, at);
                self.returned = self.returned.join(&given);
            }
            let ended = Some(lists);
            if self.ends.get(at) != Some(&ended) {
                moved = true;
                if let Some(end) = self.ends.get_mut(at) {
                    *end = ended;
                }
            }
        }
        moved
    }

    /// What each list local holds where a block starts: what the blocks leading to it left, or
    /// what the parameters are given where the function starts.
    fn entering(&self, block: usize) -> Lists {
        let mut lists = Lists::new();
        if block == 0 {
            for at in 0..self.function.params {
                if self.is_list(LocalId(at)) {
                    lists.insert(at, self.world.param(self.function, at).items());
                }
            }
        }
        for (from, ended) in self.ends.iter().enumerate() {
            let Some(ended) = ended else { continue };
            let leads_here = self
                .function
                .blocks
                .get(from)
                .is_some_and(|candidate| candidate.end.targets().iter().any(|target| target.0 == block));
            if !leads_here {
                continue;
            }
            for (local, items) in ended {
                let joined = lists.get(local).map_or_else(|| items.clone(), |known| known.join(items));
                lists.insert(*local, joined);
            }
        }
        lists
    }

    /// What's known about an operand read in a block.
    fn read(&self, value: &Operand, lists: &Lists, block: usize) -> Fact {
        match value {
            Operand::Int(number) => Fact::Int(*number, *number),
            Operand::Float(number) => Fact::Float(Float::exactly(*number)),
            Operand::Local(local) if self.is_list(*local) => {
                Fact::List(Box::new(lists.get(&local.0).cloned().unwrap_or(Fact::Any)))
            }
            Operand::Local(local) => self.local_in(*local, block),
            _ => Fact::Any,
        }
    }

    /// What's known about a local that isn't a list where a block reads it: what writes it,
    /// narrowed by what the branches on the only way in say, or a count's range.
    fn local_in(&self, local: LocalId, block: usize) -> Fact {
        let known = self.found.locals.get(local.0).cloned().unwrap_or(Fact::Any);
        if let Some((stop, down)) = self.counts.get(&local.0) {
            return self.count_range(local, *stop, *down, block);
        }
        if self.writes.get(local.0).copied().unwrap_or(2) > 1 {
            return known;
        }
        let mut narrowed = known;
        for (op, lhs, rhs, holds) in self.narrowed.get(block).into_iter().flatten() {
            if *lhs == Operand::Local(local) {
                let other = self.plain(rhs);
                narrowed = narrow(&narrowed, *op, &other, *holds);
            } else if *rhs == Operand::Local(local)
                && let Some(op) = crate::facts::flipped(*op)
            {
                let other = self.plain(lhs);
                narrowed = narrow(&narrowed, op, &other, *holds);
            }
        }
        narrowed
    }

    /// What's known about an operand without what branches say, as the other side of one.
    fn plain(&self, value: &Operand) -> Fact {
        match value {
            Operand::Int(number) => Fact::Int(*number, *number),
            Operand::Float(number) => Fact::Float(Float::exactly(*number)),
            Operand::Local(local) => self.found.locals.get(local.0).cloned().unwrap_or(Fact::Any),
            _ => Fact::Any,
        }
    }

    /// A count's range where the loop's body reads it: from its first value to its limit, as
    /// `crate::facts` has it.
    fn count_range(&self, count: LocalId, stop: LocalId, down: bool, block: usize) -> Fact {
        let Some(first) = self.starts.get(&count.0).and_then(Fact::int) else { return Fact::Any };
        let Some((stop_low, stop_high)) = self.local_in(stop, block).int() else { return Fact::Any };
        if down {
            Fact::Int(stop_low.saturating_add(1).min(first.0), first.1)
        } else {
            Fact::Int(first.0, stop_high.saturating_sub(1).max(first.1))
        }
    }

    /// Writes what's known about a local, joined with what was, widened once it's grown a few
    /// times. Says whether it moved.
    fn write(&mut self, local: LocalId, fact: &Fact) -> bool {
        let Some(old) = self.found.locals.get(local.0) else { return false };
        let joined = old.join(fact);
        if joined == *old {
            return false;
        }
        let grew = self.grew.get(local.0).copied().unwrap_or(0);
        let settled = if grew >= GROWTHS_BEFORE_WIDENING { old.widen(&joined) } else { joined };
        if let Some(slot) = self.grew.get_mut(local.0) {
            *slot += 1;
        }
        if let Some(slot) = self.found.locals.get_mut(local.0) {
            *slot = settled;
        }
        true
    }

    /// Notes what a list local holds after a change: replaced, or joined with what's put in.
    fn hold(lists: &mut Lists, local: LocalId, items: Fact, replace: bool) {
        let held = if replace {
            items
        } else {
            lists.get(&local.0).map_or_else(|| items.clone(), |known| known.join(&items))
        };
        lists.insert(local.0, held);
    }

    /// The field's place among a struct type's fields.
    fn field_place(&self, object: &Operand, field: &str) -> Option<usize> {
        let Operand::Local(object) = object else { return None };
        let Type::Named(name) = &self.function.locals.get(object.0)?.ty else { return None };
        let def = self.world.program.structs.iter().find(|def| &def.name == name)?;
        def.fields.iter().position(|(name, _)| name == field)
    }

    /// A struct's fact with the field at `path` changed to also be `value`.
    fn with_field(&self, object: LocalId, known: &Fact, path: &[String], value: &Fact) -> Fact {
        let [field] = path else { return Fact::Any };
        let Fact::Struct(fields) = known else { return Fact::Any };
        let Some(at) = self.field_place(&Operand::Local(object), field) else { return Fact::Any };
        let mut fields = fields.clone();
        if let Some(slot) = fields.get_mut(at) {
            *slot = slot.join(value);
        }
        Fact::Struct(fields)
    }

    /// One instruction. Says whether what's known of a local moved.
    fn step(&mut self, kind: &InstKind, lists: &mut Lists, place: Place) -> bool {
        let block = place.0;
        let mut moved = false;
        match kind {
            InstKind::Copy { dest, value } => moved |= self.copy(*dest, value, lists, block),
            InstKind::Unary { dest, op, value } => {
                let given = self.read(value, lists, block);
                moved |= self.write(*dest, &unary(*op, &given));
            }
            InstKind::Binary { dest, op, lhs, rhs } => {
                let (left, right) = (self.read(lhs, lists, block), self.read(rhs, lists, block));
                if self.noting {
                    self.note_binary(*op, &left, &right, place);
                }
                moved |= self.write(*dest, &binary(*op, &left, &right));
            }
            InstKind::Make { dest, fields, .. } => {
                let fields = fields.iter().map(|field| self.read(field, lists, block)).collect();
                moved |= self.write(*dest, &Fact::Struct(fields));
            }
            InstKind::GetField { dest, object, field } => {
                let fact = self.read(object, lists, block).part(self.field_place(object, field));
                moved |= self.put(*dest, &fact, lists);
            }
            InstKind::SetField { object, path, value } => {
                let value = self.read(value, lists, block);
                let known = self.found.locals.get(object.0).cloned().unwrap_or(Fact::Any);
                let changed = self.with_field(*object, &known, path, &value);
                moved |= self.write(*object, &changed);
            }
            InstKind::MakeTuple { dest, items } => {
                let items = items.iter().map(|item| self.read(item, lists, block)).collect();
                moved |= self.write(*dest, &Fact::Tuple(items));
            }
            InstKind::GetTupleItem { dest, value, at } => {
                let fact = self.read(value, lists, block).part(Some(*at));
                moved |= self.put(*dest, &fact, lists);
            }
            InstKind::MakeList { dest, items } => {
                let held =
                    items.iter().fold(Fact::Unset, |held, item| held.join(&self.read(item, lists, block)));
                // An `array` is made as a list is, but isn't followed as one.
                moved |= self.put(*dest, &Fact::List(Box::new(held)), lists);
            }
            InstKind::GetIndex { dest, object, .. } => {
                let fact = match object {
                    Operand::Local(held) if self.is_list(*held) => {
                        lists.get(&held.0).cloned().unwrap_or(Fact::Any)
                    }
                    _ => Fact::Any,
                };
                moved |= self.put(*dest, &fact, lists);
            }
            InstKind::SetIndex { object, path, value, .. } => {
                let value = self.read(value, lists, block);
                moved |= self.set_index(*object, path.is_empty(), &value, lists);
            }
            InstKind::Builtin { dest, name, args, changes } => {
                let given: Vec<Fact> = args.iter().map(|arg| self.read(arg, lists, block)).collect();
                if *changes {
                    self.changed_by(name, args, &given, lists);
                }
                let fact = self.gives(name, &given, place);
                moved |= dest.is_some_and(|dest| self.put(dest, &fact, lists));
            }
            InstKind::Call { dest, function, args } => {
                moved |= self.call(*dest, function, args, lists, block);
            }
            InstKind::Constant { dest, name } => {
                let fact = self.world.returns.get(name).cloned().unwrap_or(Fact::Any);
                moved |= self.put(*dest, &fact, lists);
            }
            other => {
                if let Some(dest) = other.dest() {
                    moved |= self.put(dest, &Fact::Any, lists);
                }
            }
        }
        moved
    }

    /// A copy: a count's start joins what it's given, and the copy holds what was copied.
    fn copy(&mut self, dest: LocalId, value: &Operand, lists: &mut Lists, block: usize) -> bool {
        let given = self.read(value, lists, block);
        let mut moved = false;
        if self.counts.contains_key(&dest.0) {
            let start = self.starts.get(&dest.0).map_or_else(|| given.clone(), |known| known.join(&given));
            if self.starts.get(&dest.0) != Some(&start) {
                self.starts.insert(dest.0, start);
                moved = true;
            }
        }
        moved | self.put(dest, &given, lists)
    }

    /// A write to an item: a list's own item joins what it holds, and anything deeper, or in
    /// something that isn't a list, knows nothing after.
    fn set_index(&mut self, object: LocalId, own_item: bool, value: &Fact, lists: &mut Lists) -> bool {
        if !self.is_list(object) {
            return self.write(object, &Fact::Any);
        }
        let item = if own_item { value } else { &Fact::Any };
        Self::hold(lists, object, item.clone(), !own_item);
        self.note_put(object, item);
        false
    }

    /// Writes a value to a local: a list's items to what it holds, anything else to its fact.
    fn put(&mut self, dest: LocalId, fact: &Fact, lists: &mut Lists) -> bool {
        if self.is_list(dest) {
            Self::hold(lists, dest, fact.items(), true);
            false
        } else {
            self.write(dest, fact)
        }
    }

    /// Notes what's put in a list parameter lent with `ref`.
    fn note_put(&mut self, object: LocalId, value: &Fact) {
        if let Some(put) = self.puts.get_mut(object.0) {
            *put = put.join(value);
        }
    }

    /// Notes what a `//`, `%` or float comparison can leave out.
    fn note_binary(&mut self, op: BinOp, left: &Fact, right: &Fact, place: Place) {
        match op {
            BinOp::FloorDiv | BinOp::Mod => {
                if let (Some((left_low, _)), Some((right_low, _))) = (left.int(), right.int())
                    && left_low >= 0
                    && right_low >= 0
                {
                    self.found.unsigned.insert(place);
                }
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq => {
                if let (Some(first), Some(second)) = (left.float(), right.float())
                    && !first.nan
                    && !second.nan
                {
                    self.found.not_nan.insert(place);
                }
            }
            _ => {}
        }
    }

    /// What a builtin that changes its first argument does to it.
    fn changed_by(&mut self, name: &str, args: &[Operand], given: &[Fact], lists: &mut Lists) {
        let Some(Operand::Local(first)) = args.first() else { return };
        if !self.is_list(*first) {
            self.write(*first, &Fact::Any);
            return;
        }
        match name {
            "list.append" => {
                let item = given.get(1).cloned().unwrap_or(Fact::Any);
                Self::hold(lists, *first, item.clone(), false);
                self.note_put(*first, &item);
            }
            kept if KEEPS_ITEMS.contains(&kept) => {}
            _ => {
                Self::hold(lists, *first, Fact::Any, true);
                self.note_put(*first, &Fact::Any);
            }
        }
    }

    /// What a builtin gives back, from what it's given.
    fn gives(&mut self, name: &str, given: &[Fact], place: Place) -> Fact {
        let numeric = ["int", "float", "min", "max", "abs", "math.sqrt"].contains(&name);
        if numeric && given.iter().any(|fact| matches!(fact, Fact::Unset)) {
            return Fact::Unset;
        }
        match (name, given) {
            ("list.len" | "len", _) => Fact::Int(0, i64::MAX),
            ("int", [Fact::Float(float)]) => {
                let fits =
                    !float.nan && !float.empty() && float.low > -INT_CEILING && float.high < INT_CEILING;
                if fits {
                    if self.noting {
                        self.found.fits.insert(place);
                    }
                    Fact::Int(truncated(float.low), truncated(float.high))
                } else {
                    Fact::Int(i64::MIN, i64::MAX)
                }
            }
            ("int", [Fact::Int(low, high)]) => Fact::Int(*low, *high),
            ("float", [Fact::Int(low, high)]) => {
                // Up to 2^53 every int is a float exactly; past it, only some.
                let exact = 1_i64 << 53;
                if (-exact..=exact).contains(low) && (-exact..=exact).contains(high) {
                    Fact::Float(Float { low: int_as_float(*low), high: int_as_float(*high), nan: false })
                } else {
                    Fact::Float(Float { low: -INT_CEILING, high: INT_CEILING, nan: false })
                }
            }
            ("min" | "max", [first, second]) => extreme(name == "min", first, second),
            ("abs", [Fact::Int(low, high)]) => {
                let (low, high) = (i128::from(*low), i128::from(*high));
                let smallest = if low <= 0 && high >= 0 { 0 } else { low.abs().min(high.abs()) };
                clamped(smallest, low.abs().max(high.abs()))
            }
            ("abs", [Fact::Float(float)]) => {
                let smallest = if float.holds_zero() { 0.0 } else { float.low.abs().min(float.high.abs()) };
                Fact::Float(Float {
                    low: smallest,
                    high: float.low.abs().max(float.high.abs()),
                    nan: float.nan,
                })
            }
            ("math.sqrt", [Fact::Float(float)]) => {
                let negative = float.empty() || float.low < 0.0;
                Fact::Float(Float {
                    low: float.low.max(0.0).sqrt(),
                    high: float.high.max(0.0).sqrt(),
                    nan: float.nan || negative,
                })
            }
            _ => Fact::Any,
        }
    }

    /// A call by name: what it gives each parameter, what the lists it's lent hold after, and
    /// what it gives back. Says whether a local moved.
    fn call(
        &mut self,
        dest: Option<LocalId>,
        callee: &str,
        args: &[Operand],
        lists: &mut Lists,
        block: usize,
    ) -> bool {
        let given: Vec<Fact> = args.iter().map(|arg| self.read(arg, lists, block)).collect();
        if self.noting {
            let params = self.calls.entry(callee.to_owned()).or_default();
            if params.len() < given.len() {
                params.resize(given.len(), Fact::Unset);
            }
            for (known, fact) in params.iter_mut().zip(&given) {
                *known = known.join(fact);
            }
        }
        let known = self.world.functions.get(callee);
        let lent = known.map(|function| function.mut_params.clone()).unwrap_or_default();
        let mut moved = false;
        for at in lent {
            let Some(Operand::Local(arg)) = args.get(at) else { continue };
            if self.is_list(*arg) {
                // Before the callee's walk has said what it puts in, it's put nothing in yet, as
                // a parameter nothing has called with is given nothing yet.
                let every =
                    self.world.sets_every.get(callee).and_then(|all| all.get(at)).copied().unwrap_or(false);
                let leaves = self.world.leaves.get(callee).and_then(|all| all.get(at)).cloned().flatten();
                let puts = leaves.map_or(Fact::Unset, |leaves| leaves.puts);
                Self::hold(lists, *arg, puts, every);
                // What the callee puts in a list this function was lent is put in it here too.
                let put = lists.get(&arg.0).cloned().unwrap_or(Fact::Any);
                self.note_put(*arg, &put);
            } else {
                moved |= self.write(*arg, &Fact::Any);
            }
        }
        let Some(dest) = dest else { return moved };
        let fact = if known.is_some() {
            self.world.returns.get(callee).cloned().unwrap_or(Fact::Unset)
        } else {
            Fact::Any
        };
        moved | self.put(dest, &fact, lists)
    }

    /// Puts what this function's calls gave, what it gave back and what it put in its lists in
    /// `next`.
    fn hand_over(&mut self, next: &mut Next) {
        for (callee, given) in std::mem::take(&mut self.calls) {
            let params = next.params.entry(callee).or_default();
            if params.len() < given.len() {
                params.resize(given.len(), Fact::Unset);
            }
            for (known, fact) in params.iter_mut().zip(&given) {
                *known = known.join(fact);
            }
        }
        let returned = next.returns.entry(self.function.name.clone()).or_insert(Fact::Unset);
        *returned = returned.join(&self.returned);
        let mut puts = std::mem::take(&mut self.puts);
        // A parameter not lent with `ref`, or not a list, leaves nothing to say.
        for (at, put) in puts.iter_mut().enumerate() {
            if !self.function.mut_params.contains(&at) || !self.is_list(LocalId(at)) {
                *put = Fact::Unset;
            }
        }
        next.puts.insert(self.function.name.clone(), puts);
    }
}

/// What's known about the result of a unary operator.
fn unary(op: UnaryOp, value: &Fact) -> Fact {
    match (op, value) {
        (_, Fact::Unset) => Fact::Unset,
        (UnaryOp::Neg, Fact::Int(low, high)) => clamped(-i128::from(*high), -i128::from(*low)),
        (UnaryOp::Neg, Fact::Float(float)) => {
            if float.empty() {
                return Fact::Float(*float);
            }
            Fact::Float(Float { low: -float.high, high: -float.low, nan: float.nan })
        }
        _ => Fact::Any,
    }
}

/// What's known about the result of a binary operator. A comparison's is a bool, so nothing.
fn binary(op: BinOp, left: &Fact, right: &Fact) -> Fact {
    if matches!(left, Fact::Unset) || matches!(right, Fact::Unset) {
        return Fact::Unset;
    }
    if matches!(op, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq) {
        return Fact::Any;
    }
    match (left, right) {
        (Fact::Int(..), Fact::Int(..)) => int_binary(op, left, right),
        (Fact::Float(first), Fact::Float(second)) => float_binary(op, *first, *second),
        // An int and an unknown, or a float and an unknown, of one type as the checker says.
        (Fact::Int(..), Fact::Any) | (Fact::Any, Fact::Int(..)) => {
            let (Some(first), Some(second)) = (left.as_int(), right.as_int()) else { return Fact::Any };
            int_binary(op, &Fact::Int(first.0, first.1), &Fact::Int(second.0, second.1))
        }
        (Fact::Float(..), Fact::Any) | (Fact::Any, Fact::Float(..)) => {
            let (Some(first), Some(second)) = (left.as_float(), right.as_float()) else { return Fact::Any };
            float_binary(op, first, second)
        }
        _ => Fact::Any,
    }
}

fn int_binary(op: BinOp, left: &Fact, right: &Fact) -> Fact {
    let (Some((left_low, left_high)), Some((right_low, right_high))) = (left.int(), right.int()) else {
        return Fact::Any;
    };
    let (a, b, c, d) =
        (i128::from(left_low), i128::from(left_high), i128::from(right_low), i128::from(right_high));
    match op {
        BinOp::Add => clamped(a + c, b + d),
        BinOp::Sub => clamped(a - d, b - c),
        BinOp::Mul => {
            let products = [a * c, a * d, b * c, b * d];
            clamped(products.iter().copied().min().unwrap_or(0), products.iter().copied().max().unwrap_or(0))
        }
        // A floor `%` by a positive number is from 0 to one less than it, whatever its left side.
        BinOp::Mod if c > 0 => {
            let most = d - 1;
            if a >= 0 { clamped(0, most.min(b)) } else { clamped(0, most) }
        }
        BinOp::FloorDiv if c > 0 => {
            let quotients = [a.div_euclid(c), a.div_euclid(d), b.div_euclid(c), b.div_euclid(d)];
            clamped(
                quotients.iter().copied().min().unwrap_or(0),
                quotients.iter().copied().max().unwrap_or(0),
            )
        }
        _ => Fact::Int(i64::MIN, i64::MAX),
    }
}

fn float_binary(op: BinOp, left: Float, right: Float) -> Fact {
    if left.empty() || right.empty() {
        return Fact::Float(Float {
            low: f64::INFINITY,
            high: f64::NEG_INFINITY,
            nan: left.nan || right.nan,
        });
    }
    let nan = left.nan || right.nan;
    let (low, high, nan) = match op {
        BinOp::Add => {
            let opposite = (left.reaches(f64::INFINITY) && right.reaches(f64::NEG_INFINITY))
                || (left.reaches(f64::NEG_INFINITY) && right.reaches(f64::INFINITY));
            let (low, high) = bounds_of([
                left.low + right.low,
                left.high + right.high,
                left.low + right.low,
                left.high + right.high,
            ]);
            (low, high, nan || opposite)
        }
        BinOp::Sub => {
            let same = (left.reaches(f64::INFINITY) && right.reaches(f64::INFINITY))
                || (left.reaches(f64::NEG_INFINITY) && right.reaches(f64::NEG_INFINITY));
            let (low, high) = bounds_of([
                left.low - right.high,
                left.high - right.low,
                left.low - right.high,
                left.high - right.low,
            ]);
            (low, high, nan || same)
        }
        BinOp::Mul => {
            let zero_times_infinity = (left.holds_zero()
                && (right.reaches(f64::INFINITY) || right.reaches(f64::NEG_INFINITY)))
                || (right.holds_zero() && (left.reaches(f64::INFINITY) || left.reaches(f64::NEG_INFINITY)));
            let (low, high) = bounds_of([
                left.low * right.low,
                left.low * right.high,
                left.high * right.low,
                left.high * right.high,
            ]);
            (low, high, nan || zero_times_infinity)
        }
        BinOp::Div => {
            if right.holds_zero() {
                // A division by zero gives an infinity, or NaN for zero by zero.
                (f64::NEG_INFINITY, f64::INFINITY, nan || left.holds_zero())
            } else {
                let infinities = (left.reaches(f64::INFINITY) || left.reaches(f64::NEG_INFINITY))
                    && (right.reaches(f64::INFINITY) || right.reaches(f64::NEG_INFINITY));
                let (low, high) = bounds_of([
                    left.low / right.low,
                    left.low / right.high,
                    left.high / right.low,
                    left.high / right.high,
                ]);
                (low, high, nan || infinities)
            }
        }
        _ => return Fact::Any,
    };
    Fact::Float(Float { low, high, nan })
}

/// What's known about `min` or `max` of two values. Under the total order NaN sorts last, so
/// `min` gives NaN only when both are, and `max` whenever either is.
fn extreme(min: bool, first: &Fact, second: &Fact) -> Fact {
    match (first, second) {
        (Fact::Unset, _) | (_, Fact::Unset) => Fact::Unset,
        (Fact::Int(a, b), Fact::Int(c, d)) => {
            if min {
                Fact::Int(*a.min(c), *b.min(d))
            } else {
                Fact::Int(*a.max(c), *b.max(d))
            }
        }
        (Fact::Float(one), Fact::Float(other)) => {
            if one.empty() || other.empty() {
                return Fact::Float(one.join(*other));
            }
            Fact::Float(if min {
                Float {
                    low: one.low.min(other.low),
                    high: one.high.min(other.high),
                    nan: one.nan && other.nan,
                }
            } else {
                Float {
                    low: one.low.max(other.low),
                    high: one.high.max(other.high),
                    nan: one.nan || other.nan,
                }
            })
        }
        _ => Fact::Any,
    }
}

/// What's known about a value once `value op other` has come out as `holds`, by floats' total
/// order, in which NaN sorts last.
fn narrow(value: &Fact, op: BinOp, other: &Fact, holds: bool) -> Fact {
    // Nothing reaches the other side yet, so nothing reaches here.
    if matches!(value, Fact::Unset) || matches!(other, Fact::Unset) {
        return Fact::Unset;
    }
    let op = if holds { Some(op) } else { negated(op) };
    let Some(op) = op else { return value.clone() };
    match (value, other) {
        (Fact::Int(..) | Fact::Any, Fact::Int(other_low, other_high)) => {
            let Some((low, high)) = value.as_int() else { return value.clone() };
            let (low, high) = match op {
                BinOp::Lt => (low, high.min(other_high.saturating_sub(1))),
                BinOp::LtEq => (low, high.min(*other_high)),
                BinOp::Gt => (low.max(other_low.saturating_add(1)), high),
                BinOp::GtEq => (low.max(*other_low), high),
                BinOp::Eq => (low.max(*other_low), high.min(*other_high)),
                _ => (low, high),
            };
            if low > high { value.clone() } else { Fact::Int(low, high) }
        }
        (Fact::Float(..) | Fact::Any, Fact::Float(other)) => {
            let Some(known) = value.as_float() else { return value.clone() };
            if other.empty() {
                return value.clone();
            }
            let narrowed = match op {
                // Below something: not NaN, and no higher than its most, which counts only if it
                // can't be NaN, the one value above every number.
                BinOp::Lt | BinOp::LtEq => Float {
                    low: known.low,
                    high: if other.nan { known.high } else { known.high.min(other.high) },
                    nan: known.nan && other.nan && op == BinOp::LtEq,
                },
                // Above something: no lower than its least, and NaN still can be.
                BinOp::Gt | BinOp::GtEq => {
                    Float { low: known.low.max(other.low), high: known.high, nan: known.nan }
                }
                _ => known,
            };
            if narrowed.empty() && !narrowed.nan { value.clone() } else { Fact::Float(narrowed) }
        }
        _ => value.clone(),
    }
}

/// The comparison that's true when this one is false. Under the total order every two floats
/// compare, NaN included, so `not (x < y)` is `x >= y`.
fn negated(op: BinOp) -> Option<BinOp> {
    Some(match op {
        BinOp::Lt => BinOp::GtEq,
        BinOp::LtEq => BinOp::Gt,
        BinOp::Gt => BinOp::LtEq,
        BinOp::GtEq => BinOp::Lt,
        BinOp::NotEq => BinOp::Eq,
        _ => return None,
    })
}

/// What branches say in each block: each comparison that came out one way on the only way in,
/// back along a chain of blocks each only one block leads to.
fn narrowings(function: &Function) -> Vec<Vec<(BinOp, Operand, Operand, bool)>> {
    let mut comes_from: Vec<Vec<usize>> = vec![Vec::new(); function.blocks.len()];
    for (at, block) in function.blocks.iter().enumerate() {
        for target in block.end.targets() {
            if let Some(from) = comes_from.get_mut(target.0) {
                from.push(at);
            }
        }
    }
    let only_way_in = |block: usize| match comes_from.get(block).map(Vec::as_slice) {
        Some([from]) => Some(*from),
        _ => None,
    };
    let mut known = Vec::with_capacity(function.blocks.len());
    for at in 0..function.blocks.len() {
        let mut facts = Vec::new();
        let (mut here, mut seen) = (at, vec![at]);
        while let Some(from) = only_way_in(here).filter(|from| !seen.contains(from)) {
            facts.extend(function.blocks.get(from).and_then(|block| comparison_along(block, here)));
            seen.push(from);
            here = from;
        }
        known.push(facts);
    }
    known
}

/// The comparison a block's branch made, and which way it came out on the way to `to`.
fn comparison_along(from: &Block, to: usize) -> Option<(BinOp, Operand, Operand, bool)> {
    let Terminator::Branch { cond: Operand::Local(cond), then, otherwise } = &from.end else { return None };
    let holds = if then.0 == to && otherwise.0 != to {
        true
    } else if otherwise.0 == to && then.0 != to {
        false
    } else {
        return None;
    };
    from.insts.iter().rev().find_map(|inst| match &inst.kind {
        InstKind::Binary { dest, op, lhs, rhs } if dest == cond => {
            Some((*op, lhs.clone(), rhs.clone(), holds))
        }
        _ => None,
    })
}

/// For each parameter of a function, whether it's a list lent with `ref` whose every item the
/// function sets before it gives back: a count from 0 up to the list's length taken once, in a
/// loop it always finishes, sets the item at the count on every round, and nothing else changes
/// the list's length.
fn sets_every_item(function: &Function) -> Vec<bool> {
    (0..function.params)
        .map(|at| function.mut_params.contains(&at) && sets_every_item_of(function, LocalId(at)))
        .collect()
}

fn sets_every_item_of(function: &Function, list: LocalId) -> bool {
    if !keeps_length(function, list) {
        return false;
    }
    let dominators = dominators(function);
    function.counts.iter().any(|count| !count.down && sets_each_round(function, list, count, &dominators))
}

/// Whether nothing in a function changes a list's length: it's only set by index, read, and
/// passed to calls that don't change it.
fn keeps_length(function: &Function, list: LocalId) -> bool {
    function.blocks.iter().flat_map(|block| &block.insts).all(|inst| match &inst.kind {
        InstKind::Builtin { args, changes: true, name, .. } => {
            args.first() != Some(&Operand::Local(list)) || KEEPS_ITEMS.contains(&name.as_str())
        }
        InstKind::Call { args, .. } => !args.contains(&Operand::Local(list)),
        InstKind::Copy { dest, .. } => *dest != list,
        InstKind::SetIndex { object, path, .. } => *object != list || path.is_empty(),
        other => other.dest() != Some(list),
    })
}

/// Whether one count loop sets `list[count]` on every round, counting from 0 to the list's
/// length, and the function can't give back without finishing it.
fn sets_each_round(
    function: &Function,
    list: LocalId,
    count: &crate::Count,
    dominators: &[Vec<bool>],
) -> bool {
    let insts = || {
        function
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(at, block)| block.insts.iter().map(move |inst| (at, inst)))
    };
    // Its first value is 0, and its limit the list's length.
    let starts_at_zero = insts().any(|(_, inst)| {
        matches!(&inst.kind, InstKind::Copy { dest, value: Operand::Int(0) } if *dest == count.count)
    });
    if !starts_at_zero || !is_length_of(function, count.stop, list) {
        return false;
    }
    // The loop: the block that steps the count, and the one that checks it.
    let Some((latch, _)) = insts().find(|(_, inst)| {
        matches!(&inst.kind, InstKind::Binary { dest, op: BinOp::Add, lhs: Operand::Local(lhs), rhs: Operand::Int(1) }
            if *dest == count.count && *lhs == count.count)
    }) else {
        return false;
    };
    let Some((check, exit)) = function.blocks.iter().enumerate().find_map(|(at, block)| {
        let Terminator::Branch { cond: Operand::Local(cond), otherwise, .. } = &block.end else { return None };
        let compares = block.insts.iter().any(|inst| {
            matches!(&inst.kind, InstKind::Binary { dest, op: BinOp::Lt, lhs: Operand::Local(lhs), rhs: Operand::Local(rhs) }
                if dest == cond && *lhs == count.count && *rhs == count.stop)
        });
        compares.then_some((at, otherwise.0))
    }) else {
        return false;
    };
    // Every way to give back goes through the loop's way out, which only the check leads to.
    let leaves_only_by_check = function
        .blocks
        .iter()
        .enumerate()
        .filter(|(at, _)| *at != check)
        .all(|(_, block)| !block.end.targets().iter().any(|target| target.0 == exit));
    let returns_after =
        function.blocks.iter().enumerate().all(|(at, block)| {
            !matches!(block.end, Terminator::Return(_)) || dominates(dominators, exit, at)
        });
    if !leaves_only_by_check || !returns_after {
        return false;
    }
    // On every round, the item at the count is set: in a block every way round the loop goes
    // through, on its way to the step.
    insts().any(|(at, inst)| match &inst.kind {
        InstKind::SetIndex { object, path, index: Operand::Local(index), .. } => {
            *object == list
                && path.is_empty()
                && is_copy_of(function, *index, count.count)
                && dominates(dominators, at, latch)
        }
        _ => false,
    })
}

/// Whether a local holds the length of a list, taken once: written by `list.len(list)` or by a
/// copy of a local that is, and nothing else.
fn is_length_of(function: &Function, local: LocalId, list: LocalId) -> bool {
    let writes: Vec<&InstKind> = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .map(|inst| &inst.kind)
        .filter(|kind| kind.dest() == Some(local))
        .collect();
    match writes.as_slice() {
        [InstKind::Builtin { name, args, .. }] => {
            name == "list.len" && args.first() == Some(&Operand::Local(list))
        }
        [InstKind::Copy { value: Operand::Local(from), .. }] => {
            *from != local && is_length_of(function, *from, list)
        }
        _ => false,
    }
}

/// Whether a local is a count, or written once by a copy of it.
fn is_copy_of(function: &Function, local: LocalId, count: LocalId) -> bool {
    if local == count {
        return true;
    }
    let writes: Vec<&InstKind> = function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .map(|inst| &inst.kind)
        .filter(|kind| kind.dest() == Some(local))
        .collect();
    matches!(writes.as_slice(), [InstKind::Copy { value: Operand::Local(from), .. }] if *from == count)
}

/// For each block, which blocks every way to it from the start goes through, itself included.
fn dominators(function: &Function) -> Vec<Vec<bool>> {
    let count = function.blocks.len();
    let mut comes_from: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (at, block) in function.blocks.iter().enumerate() {
        for target in block.end.targets() {
            if let Some(from) = comes_from.get_mut(target.0) {
                from.push(at);
            }
        }
    }
    let mut dominated: Vec<Vec<bool>> =
        (0..count).map(|at| (0..count).map(|other| at != 0 || other == 0).collect()).collect();
    loop {
        let mut changed = false;
        for at in 1..count {
            let mut known: Vec<bool> = vec![true; count];
            let froms = comes_from.get(at).cloned().unwrap_or_default();
            if froms.is_empty() {
                known = vec![false; count];
            }
            for from in froms {
                if let Some(theirs) = dominated.get(from) {
                    for (mine, theirs) in known.iter_mut().zip(theirs) {
                        *mine = *mine && *theirs;
                    }
                }
            }
            if let Some(mine) = known.get_mut(at) {
                *mine = true;
            }
            if dominated.get(at) != Some(&known) {
                if let Some(slot) = dominated.get_mut(at) {
                    *slot = known;
                }
                changed = true;
            }
        }
        if !changed {
            return dominated;
        }
    }
}

/// Whether every way from the start to `block` goes through `by`.
fn dominates(dominators: &[Vec<bool>], by: usize, block: usize) -> bool {
    dominators.get(block).and_then(|known| known.get(by)).copied().unwrap_or(false)
}
