//! What's known about the values a program's ints and floats can hold, for a backend to leave out
//! the checks a value in that range can't fail: what a negative int costs, as the correction a
//! floor `//` or `%` makes for a negative side, the way a list index counts from the end and the
//! signed overflow check of a `*`; `int(x)`'s stop for a float that doesn't fit or is NaN; and
//! the NaN test a float comparison makes for the total order. It works over the whole program and depends on
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
//! as a program runs in the IR's evaluator (`crate::eval::check_ranges`). A backend that drops a
//! check on one checks it again in its build that checks proofs, as it does the length facts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use fennec_check::Type;
use fennec_syntax::{BinOp, UnaryOp};

use crate::lengths::{called_other_ways, every};
use crate::{Block, Function, InstKind, LocalId, Operand, Program, StructDef, Terminator};

/// How many times a fact grows before what's still moving in it is widened.
const GROWTHS_BEFORE_WIDENING: usize = 4;

/// How many rounds there are at most, widening included: past them, what's still moving is
/// taken as unknown.
const MOST_ROUNDS: usize = 24;

/// Where an int's least value that's still falling stops before it's let go: 0, which keeps
/// what isn't negative, and -1, which a missing index is often written as. Each is passed at most
/// once, so widening still ends.
const INT_FLOORS: [i64; 2] = [0, -1];

/// The changes to a list that keep its items, and its length.
const KEEPS_ITEMS: [&str; 2] = ["list.sort", "list.reverse"];

/// The changes to a list that put nothing new in it: they take items out or move them about.
const ADDS_NOTHING: [&str; 6] =
    ["list.sort", "list.reverse", "list.pop", "list.clear", "list.remove", "list.swap"];

/// The builtins that give a collection's or a string's length.
const LENGTHS: [&str; 5] = ["len", "list.len", "dict.len", "set.len", "str.len"];

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
                if new_low < low {
                    INT_FLOORS.into_iter().find(|floor| floor <= new_low).unwrap_or(i64::MIN)
                } else {
                    *low
                },
                if new_high > high { i64::MAX } else { *high },
            ),
            (Fact::Float(old), Fact::Float(new)) => {
                if old.empty() {
                    return Fact::Float(*new);
                }
                Fact::Float(Float {
                    low: if new.low < old.low {
                        if new.low >= 0.0 { 0.0 } else { f64::NEG_INFINITY }
                    } else {
                        old.low
                    },
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
    /// Each instruction whose ints are all known not negative: both sides of a `//`, `%` or `*`,
    /// or a list's index.
    not_negative: BTreeSet<Place>,
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

    /// Whether the instruction at this place has its ints known not negative:
    /// - both sides of a floor `//` or `%`, so it's the same as the unsigned operation;
    /// - both sides of an int `*`, so it overflows only past `i64::MAX`, as an unsigned multiply
    ///   finds;
    /// - the index of a list's item, read or set straight into the list, so it never counts from
    ///   the end.
    #[must_use]
    pub fn not_negative(&self, place: Place) -> bool {
        self.not_negative.contains(&place)
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

    /// The instructions whose ints are known not negative, the `int(x)` known to fit and the
    /// comparisons known not NaN, by place.
    pub fn places(&self) -> (&BTreeSet<Place>, &BTreeSet<Place>, &BTreeSet<Place>) {
        (&self.not_negative, &self.fits, &self.not_nan)
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

/// A fact known across functions, by what it's of: a function's parameter (`PARAM`), what a
/// function or constant gives back (`RETURN`), what a function leaves in a list it's lent
/// (`LEAVES`), or what a struct's field holds (`FIELD`), with the parameter's or field's place.
type Across = (String, usize, u8);
const PARAM: u8 = 0;
const RETURN: u8 = 1;
const LEAVES: u8 = 2;
const FIELD: u8 = 3;

/// What's known about the values of a program's ints and floats.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ranges {
    functions: BTreeMap<String, FunctionRanges>,
    /// What didn't settle within its rounds, when something didn't, so nothing is known.
    unsettled: Option<Unsettled>,
}

/// What was still moving after every round it had, so the program's facts were let go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsettled {
    /// A function's walk.
    Function(String),
    /// The rounds across the program's functions.
    Program,
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

    /// What was still moving after every round it had, if anything was, so that nothing about
    /// the program is known. Widening settles every walk long before then; this says when one
    /// didn't, so a lost fact never goes unseen.
    #[must_use]
    pub fn unsettled(&self) -> Option<&Unsettled> {
        self.unsettled.as_ref()
    }

    /// Works out what's known about a program's values.
    #[must_use]
    pub fn of(program: &Program) -> Self {
        let whole = Whole::of(program);
        let mut world = whole.world();
        let functions: Vec<&Function> = every(program).collect();
        // What each function's last walk found, or None when it didn't settle, and what it read
        // across functions: a round walks again only those that read a fact the round before
        // changed, since a walk is worked out from its function and those facts alone, and what a
        // walk gives the next round was taken in already when it was last walked.
        let mut found: Vec<Option<Option<FunctionRanges>>> = vec![None; functions.len()];
        let mut readers: BTreeMap<Across, BTreeSet<usize>> = BTreeMap::new();
        let mut read: Vec<BTreeSet<Across>> = vec![BTreeSet::new(); functions.len()];
        let mut walking: BTreeSet<usize> = (0..functions.len()).collect();
        for _ in 0..MOST_ROUNDS {
            let mut next = Next::default();
            for &at in &walking {
                let Some(function) = functions.get(at) else { continue };
                let (ranges, reads) = world.function(function, &mut next);
                if let Some(slot) = found.get_mut(at) {
                    *slot = Some(ranges);
                }
                if let Some(old) = read.get_mut(at) {
                    for fact in old.difference(&reads) {
                        if let Some(set) = readers.get_mut(fact) {
                            set.remove(&at);
                        }
                    }
                    for fact in &reads {
                        readers.entry(fact.clone()).or_default().insert(at);
                    }
                    *old = reads;
                }
            }
            let changed = world.take(next);
            if changed.is_empty() {
                return Self::found(&functions, found);
            }
            walking = changed.iter().filter_map(|fact| readers.get(fact)).flatten().copied().collect();
        }
        // Still moving after every round: nothing worked out can be trusted.
        Ranges { functions: BTreeMap::new(), unsettled: Some(Unsettled::Program) }
    }

    /// What each function's last walk found, once nothing across functions moves.
    fn found(functions: &[&Function], found: Vec<Option<Option<FunctionRanges>>>) -> Self {
        let mut ranges = BTreeMap::new();
        for (function, last) in functions.iter().zip(found) {
            match last.flatten() {
                Some(found) => {
                    ranges.insert(function.name.clone(), found);
                }
                // A function's walk that never settled ended on facts still moving: neither what
                // it found nor what it handed its callees can be trusted.
                None => {
                    let unsettled = Some(Unsettled::Function(function.name.clone()));
                    return Ranges { functions: BTreeMap::new(), unsettled };
                }
            }
        }
        Ranges { functions: ranges, unsettled: None }
    }
}

/// What's worked out once about a whole program, before any round.
struct Whole<'p> {
    functions: HashMap<&'p str, &'p Function>,
    other_ways: HashSet<&'p str>,
    closures: HashMap<&'p str, usize>,
    outside: HashSet<String>,
    sets_every: HashMap<&'p str, Vec<bool>>,
    /// The program's types by name, found once rather than searched for at each read and write.
    named: Named<'p>,
}

/// The program's structs and errors by name, as searching them in order finds them.
struct Named<'p> {
    /// The first struct of each name that isn't a `shared` type.
    plain: HashMap<&'p str, &'p StructDef>,
    /// The names of the `shared` types.
    shared: HashSet<&'p str>,
    /// The fields of the first struct, or else error, of each name (`fields_of`).
    fields: HashMap<&'p str, &'p [(String, Type)]>,
}

impl<'p> Named<'p> {
    fn of(program: &'p Program) -> Self {
        let mut plain = HashMap::new();
        let mut shared = HashSet::new();
        let mut fields = HashMap::new();
        for def in &program.structs {
            if def.shared {
                shared.insert(def.name.as_str());
            } else {
                plain.entry(def.name.as_str()).or_insert(def);
            }
        }
        for def in program.structs.iter().chain(&program.errors) {
            fields.entry(def.name.as_str()).or_insert(def.fields.as_slice());
        }
        Named { plain, shared, fields }
    }
}

impl<'p> Whole<'p> {
    fn of(program: &'p Program) -> Self {
        let functions: HashMap<&str, &Function> =
            every(program).map(|function| (function.name.as_str(), function)).collect();
        let other_ways = called_other_ways(program);
        let closures = closures(program);
        let outside = outside_structs(program, &other_ways, &closures);
        let sets_every =
            functions.iter().map(|(name, function)| (*name, sets_every_item(function))).collect();
        let named = Named::of(program);
        Whole { functions, other_ways, closures, outside, sets_every, named }
    }

    /// Nothing known across functions yet.
    fn world(&self) -> World<'_> {
        World {
            functions: &self.functions,
            params: BTreeMap::new(),
            returns: BTreeMap::new(),
            leaves: BTreeMap::new(),
            sets_every: &self.sets_every,
            other_ways: &self.other_ways,
            closures: &self.closures,
            outside: &self.outside,
            fields: BTreeMap::new(),
            grew: BTreeMap::new(),
            named: &self.named,
        }
    }
}

/// What's known across functions, round by round.
struct World<'p> {
    functions: &'p HashMap<&'p str, &'p Function>,
    /// What every call gives each parameter, by function.
    params: BTreeMap<String, Vec<Fact>>,
    /// What every `return` gives back, by function.
    returns: BTreeMap<String, Fact>,
    /// What each function leaves in each list it's lent with `ref`, by function and parameter.
    leaves: BTreeMap<String, Vec<Option<Leaves>>>,
    sets_every: &'p HashMap<&'p str, Vec<bool>>,
    other_ways: &'p HashSet<&'p str>,
    /// Each function only ever reached through a closure, with how many of its first parameters
    /// every closure made of it captures.
    closures: &'p HashMap<&'p str, usize>,
    /// The structs whose values can come from outside the program's own code, or be changed
    /// there, so what its writes give a field isn't all a field can hold.
    outside: &'p HashSet<String>,
    /// What every write in the program gives each followed struct field, by struct and place.
    fields: BTreeMap<(String, usize), Fact>,
    /// How many times each fact across functions has grown, by what it's of, for widening.
    grew: BTreeMap<(String, usize, u8), usize>,
    named: &'p Named<'p>,
}

/// What one round found, for the next.
#[derive(Default)]
struct Next {
    params: BTreeMap<String, Vec<Fact>>,
    returns: BTreeMap<String, Fact>,
    puts: BTreeMap<String, Vec<Fact>>,
    fields: BTreeMap<(String, usize), Fact>,
}

impl World<'_> {
    /// Takes what a round found, joined with what was known, widening each fact once it's grown
    /// a few times. Gives back each fact that changed.
    fn take(&mut self, next: Next) -> BTreeSet<Across> {
        let mut changed = BTreeSet::new();
        let grew = &mut self.grew;
        let mut merge = |key: Across, old: &mut Fact, new: &Fact| {
            let joined = old.join(new);
            if joined == *old {
                return;
            }
            let times = grew.entry(key.clone()).or_insert(0);
            *times += 1;
            *old = if *times > GROWTHS_BEFORE_WIDENING { old.widen(&joined) } else { joined };
            changed.insert(key);
        };
        for (name, facts) in next.params {
            let known = self.params.entry(name.clone()).or_default();
            if known.len() < facts.len() {
                known.resize(facts.len(), Fact::Unset);
            }
            for (at, (old, new)) in known.iter_mut().zip(&facts).enumerate() {
                merge((name.clone(), at, PARAM), old, new);
            }
        }
        for (name, fact) in next.returns {
            let old = self.returns.entry(name.clone()).or_insert(Fact::Unset);
            merge((name, 0, RETURN), old, &fact);
        }
        for ((name, at), fact) in next.fields {
            let old = self.fields.entry((name.clone(), at)).or_insert(Fact::Unset);
            merge((name, at, FIELD), old, &fact);
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
                merge((name.clone(), at, LEAVES), &mut leaves.puts, new);
            }
        }
        changed
    }

    /// What's known of a parameter of a function, from every call that gives it one.
    fn param(&self, function: &Function, at: usize) -> Fact {
        // A closure's first parameters are what each closure made of it captured, and the rest are
        // what whatever calls the closure gives it.
        let captured = self.closures.get(function.name.as_str()).copied().unwrap_or(0);
        if self.other_ways.contains(function.name.as_str()) && at >= captured {
            return Fact::Any;
        }
        self.params.get(&function.name).and_then(|params| params.get(at)).cloned().unwrap_or(Fact::Unset)
    }

    /// Whether what every write gives a struct's field is followed: an int or float field, or a
    /// list of them, of a struct the program makes all of, and only its own code changes.
    fn followed_field(&self, name: &str, at: usize) -> bool {
        if self.outside.contains(name) {
            return false;
        }
        let Some(def) = self.named.plain.get(name) else {
            return false;
        };
        def.fields.get(at).is_some_and(|(_, ty)| match ty {
            Type::Int | Type::Float => true,
            Type::List(item) => matches!(**item, Type::Int | Type::Float),
            _ => false,
        })
    }

    /// The struct and the place of the field a write along `path` from a value of type `ty`
    /// ends at.
    fn field_at(&self, ty: &Type, path: &[String]) -> Option<(String, usize)> {
        let (last, through) = path.split_last()?;
        let mut ty = ty.clone();
        for name in through {
            let Type::Named(owner) = &ty else { return None };
            ty = self.named.fields.get(owner.as_str())?.iter().find(|(field, _)| field == name)?.1.clone();
        }
        let Type::Named(owner) = &ty else { return None };
        let at = self.named.fields.get(owner.as_str())?.iter().position(|(field, _)| field == last)?;
        Some((owner.clone(), at))
    }

    /// Works out what's known in one function, given what's known across functions, and puts
    /// what its calls give their callees, what it gives back, and what it puts in the lists it's
    /// lent, in `next`. None when its walk is still moving after every round.
    fn function(&self, function: &Function, next: &mut Next) -> (Option<FunctionRanges>, BTreeSet<Across>) {
        let mut walk = Walk::new(self, function);
        let settled = (0..MOST_ROUNDS).any(|_| !walk.round(false));
        // The last round, with nothing moving, writes what's found.
        walk.round(true);
        walk.hand_over(next);
        (settled.then_some(walk.found), walk.read.into_inner())
    }
}

/// What's known about locals where a block starts or ends: each list's items, and what each
/// followed local holds there, as after a branch on it or a write to it earlier in the block. Any
/// other local, or a followed one not yet written on every way here, is what every write gives it.
type State = BTreeMap<usize, Fact>;

/// One function's walk, round after round until nothing moves.
struct Walk<'w, 'p> {
    world: &'w World<'p>,
    function: &'w Function,
    /// The blocks that lead to each block.
    comes_from: Vec<Vec<usize>>,
    /// The blocks the function's start reaches, each after every block that leads to it but by
    /// going round a loop: so a round sees what each way in left before it walks a block.
    order: Vec<usize>,
    /// The locals followed through the function, each of which can hold less somewhere than every
    /// write gives it: one written more than once, or one a branch compares. Which these are
    /// depends only on the code, so what's carried never comes and goes from round to round.
    followed: Vec<bool>,
    /// What's known where each block ends.
    ends: Vec<Option<State>>,
    /// What's known where each block starts, as the last round had it.
    entries: Vec<Option<State>>,
    /// How many rounds each local's fact where each block starts has grown, by block and local:
    /// one that keeps growing round a loop is let go, for what every write gives it, which widens.
    entry_grew: Vec<BTreeMap<usize, usize>>,
    /// The locals let go where each block starts.
    let_go: Vec<BTreeSet<usize>>,
    /// How many rounds something grew where each block starts.
    block_grew: Vec<usize>,
    /// How many times each local's fact grew, for widening.
    grew: Vec<usize>,
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
    /// What's written to each followed struct field, by struct and field place.
    fields: BTreeMap<(String, usize), Fact>,
    /// Whether this is the last round, which notes what checks can be left out, what calls
    /// give, and what's given back.
    noting: bool,
    /// Every fact across functions the walk read, for which rounds walk it again.
    read: std::cell::RefCell<BTreeSet<Across>>,
}

impl<'w, 'p> Walk<'w, 'p> {
    fn new(world: &'w World<'p>, function: &'w Function) -> Self {
        let mut found =
            FunctionRanges { locals: vec![Fact::Unset; function.locals.len()], ..FunctionRanges::default() };
        let mut read = BTreeSet::new();
        for at in 0..function.params {
            if let Some(slot) = found.locals.get_mut(at) {
                *slot = world.param(function, at);
                read.insert((function.name.clone(), at, PARAM));
            }
        }
        let mut comes_from = vec![Vec::new(); function.blocks.len()];
        for (at, block) in function.blocks.iter().enumerate() {
            for target in block.end.targets() {
                if let Some(from) = comes_from.get_mut(target.0) {
                    from.push(at);
                }
            }
        }
        let changes = |callee: &str| {
            world.functions.get(callee).map(|callee| callee.mut_params.clone()).unwrap_or_default()
        };
        let facts = crate::facts::Facts::of(function, &changes);
        let mut followed: Vec<bool> =
            (0..function.locals.len()).map(|at| !facts.holds_one_value(LocalId(at))).collect();
        for block in &function.blocks {
            for target in block.end.targets() {
                let Some((_, lhs, rhs, _, _)) = comparison_along(block, target.0) else { continue };
                for side in [lhs, rhs] {
                    let Operand::Local(local) = side else { continue };
                    for same in same_as(block, local) {
                        if let Some(slot) = followed.get_mut(same.0) {
                            *slot = true;
                        }
                    }
                }
            }
        }
        // A count's range comes from its first value and its limit, wherever it's read.
        for count in &function.counts {
            if let Some(slot) = followed.get_mut(count.count.0) {
                *slot = false;
            }
        }
        let blocks = function.blocks.len();
        Walk {
            world,
            function,
            comes_from,
            order: walk_order(function),
            followed,
            ends: vec![None; blocks],
            entries: vec![None; blocks],
            entry_grew: vec![BTreeMap::new(); blocks],
            let_go: vec![BTreeSet::new(); blocks],
            block_grew: vec![0; blocks],
            grew: vec![0; function.locals.len()],
            counts: function.counts.iter().map(|count| (count.count.0, (count.stop, count.down))).collect(),
            starts: BTreeMap::new(),
            found,
            calls: BTreeMap::new(),
            returned: Fact::Unset,
            puts: vec![Fact::Unset; function.params],
            fields: BTreeMap::new(),
            noting: false,
            read: std::cell::RefCell::new(read),
        }
    }

    /// Notes that the walk read a fact across functions.
    fn reads(&self, name: &str, at: usize, of: u8) {
        self.read.borrow_mut().insert((name.to_owned(), at, of));
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
            self.fields.clear();
            self.returned = Fact::Unset;
            self.puts = vec![Fact::Unset; self.function.params];
        }
        for at in self.order.clone() {
            let Some(block) = self.function.blocks.get(at) else { continue };
            let Some(mut state) = self.entering(at) else { continue };
            for (place, inst) in block.insts.iter().enumerate() {
                moved |= self.step(&inst.kind, &mut state, (at, place));
            }
            if last && let Terminator::Return(Some(value)) = &block.end {
                let given = self.read(value, &state, at);
                self.returned = self.returned.join(&given);
            }
            let ended = Some(state);
            if self.ends.get(at) != Some(&ended) {
                moved = true;
                if let Some(end) = self.ends.get_mut(at) {
                    *end = ended;
                }
            }
        }
        moved
    }

    /// What's known where a block starts: what each block leading to it left, with what its
    /// branch says on the way here, joined; or what the parameters are given where the function
    /// starts. It's joined with what the last round had here too, so it only grows, and a local's
    /// fact that keeps growing here is let go: so the walk settles.
    fn entering(&mut self, block: usize) -> Option<State> {
        let mut state: Option<State> = None;
        if block == 0 {
            let mut start = State::new();
            for at in 0..self.function.params {
                if self.is_list(LocalId(at)) {
                    self.reads(&self.function.name, at, PARAM);
                    start.insert(at, self.world.param(self.function, at).items());
                }
            }
            state = Some(start);
        }
        for from in self.comes_from.get(block).cloned().unwrap_or_default() {
            let Some(Some(ended)) = self.ends.get(from) else { continue };
            // What a block leaves reaches as it is along a way with no comparison on it, so it's
            // read where it is rather than copied.
            let compares =
                self.function.blocks.get(from).is_some_and(|at| comparison_along(at, block).is_some());
            state = Some(match (state, compares) {
                (None, true) => self.along(from, block, ended.clone()),
                (None, false) => ended.clone(),
                (Some(known), true) => self.joined(known, &self.along(from, block, ended.clone())),
                (Some(known), false) => self.joined(known, ended),
            });
        }
        // Nothing leads here yet, so nothing's known to reach it: it isn't walked this round.
        let mut state = state?;
        let let_go = self.let_go.get(block).cloned().unwrap_or_default();
        self.let_go_of(&let_go, &mut state);
        // What the last round had here, taken: this round's is put in its place below. Joining is
        // the same either way round, so it's joined into this round's rather than copied.
        let before = self.entries.get_mut(block).and_then(Option::take);
        if let Some(before) = &before {
            state = self.joined(state, before);
        }
        let before = before.unwrap_or_default();
        // A block where something has grown in a few rounds widens whatever grows there from then
        // on, as far as it can go: counted by block, not by local, as values that go round several
        // locals in turn, like `sum`'s eight running totals, grow each local only every few rounds.
        let widening = self.block_grew.get(block).copied().unwrap_or(0) >= GROWTHS_BEFORE_WIDENING;
        let mut grew = false;
        for (local, fact) in &mut state {
            let Some(old) = before.get(local) else { continue };
            if *fact == *old {
                continue;
            }
            grew = true;
            let Some(times) = self.entry_grew.get_mut(block).map(|grew| grew.entry(*local).or_insert(0))
            else {
                continue;
            };
            *times += 1;
            // Still growing after it's been widened, as a struct's or a list's can, it's let go.
            if *times > 2 * GROWTHS_BEFORE_WIDENING {
                if let Some(let_go) = self.let_go.get_mut(block) {
                    let_go.insert(*local);
                }
            } else if widening {
                *fact = old.widen(fact);
            }
        }
        if grew && let Some(count) = self.block_grew.get_mut(block) {
            *count += 1;
        }
        let let_go = self.let_go.get(block).cloned().unwrap_or_default();
        self.let_go_of(&let_go, &mut state);
        if let Some(entry) = self.entries.get_mut(block) {
            *entry = Some(state.clone());
        }
        Some(state)
    }

    /// Lets go of locals where a block starts: one that isn't a list is what every write gives it,
    /// and a list holds anything, since a list that's missing is one not yet made.
    fn let_go_of(&self, let_go: &BTreeSet<usize>, state: &mut State) {
        for local in let_go {
            if self.is_list(LocalId(*local)) {
                state.insert(*local, Fact::Any);
            } else {
                state.remove(local);
            }
        }
    }

    /// Two ways into a block, joined: a list holds what it holds either way, and another local is
    /// known better than by every write only if it's known so both ways.
    fn joined(&self, mut known: State, other: &State) -> State {
        known.retain(|local, _| self.is_list(LocalId(*local)) || other.contains_key(local));
        for (local, fact) in other {
            if let Some(slot) = known.get_mut(local) {
                *slot = slot.join(fact);
            } else if self.is_list(LocalId(*local)) {
                known.insert(*local, fact.clone());
            }
        }
        known
    }

    /// What a block leaves on the way to `to`: what its branch's comparison says there, of each
    /// side that's a local nothing writes after the comparison.
    fn along(&self, from: usize, to: usize, mut state: State) -> State {
        let Some(block) = self.function.blocks.get(from) else { return state };
        let Some((op, lhs, rhs, holds, at)) = comparison_along(block, to) else { return state };
        // A loop's count against anything is the count's own test, whose range its first value
        // and its limit give: it would narrow the limit by the count, and the count by that
        // limit, round after round.
        let is_count =
            |side: &Operand| matches!(side, Operand::Local(local) if self.counts.contains_key(&local.0));
        if is_count(&lhs) || is_count(&rhs) {
            return state;
        }
        // A side written after the comparison isn't what was compared where the block ends.
        let rewritten =
            |side: &Operand| matches!(side, Operand::Local(local) if written_after(block, at, *local));
        if rewritten(&lhs) || rewritten(&rhs) {
            return state;
        }
        let sides = [(&lhs, &rhs, Some(op)), (&rhs, &lhs, crate::facts::flipped(op))];
        let mut narrowed = Vec::new();
        for (side, other, op) in sides {
            let (Operand::Local(local), Some(op)) = (side, op) else { continue };
            if self.is_list(*local) {
                continue;
            }
            let other = self.read(other, &state, from);
            // What a comparison says of a copy it says of what the copy was made of, and of every
            // other copy of that, while none of them is written.
            for same in same_as(block, *local) {
                let known = self.read(&Operand::Local(same), &state, from);
                narrowed.push((same.0, narrow(&known, op, &other, holds)));
            }
        }
        state.extend(narrowed);
        state
    }

    /// What's known about an operand read in a block.
    fn read(&self, value: &Operand, state: &State, block: usize) -> Fact {
        match value {
            Operand::Int(number) => Fact::Int(*number, *number),
            Operand::Float(number) => Fact::Float(Float::exactly(*number)),
            Operand::Local(local) if self.is_list(*local) => {
                Fact::List(Box::new(state.get(&local.0).cloned().unwrap_or(Fact::Any)))
            }
            Operand::Local(local) => {
                if let Some((stop, down)) = self.counts.get(&local.0) {
                    return self.count_range(*local, *stop, *down, state, block);
                }
                state.get(&local.0).or_else(|| self.found.locals.get(local.0)).cloned().unwrap_or(Fact::Any)
            }
            _ => Fact::Any,
        }
    }

    /// A count's range where the loop's body reads it: from its first value to its limit, as
    /// `crate::facts` has it.
    fn count_range(&self, count: LocalId, stop: LocalId, down: bool, state: &State, block: usize) -> Fact {
        let first = self.starts.get(&count.0).cloned().unwrap_or(Fact::Unset);
        let stop = self.read(&Operand::Local(stop), state, block);
        // Nothing reaches its first value or its limit yet, so nothing reaches it.
        if matches!(first, Fact::Unset) || matches!(stop, Fact::Unset) {
            return Fact::Unset;
        }
        let (Some(first), Some((stop_low, stop_high))) = (first.int(), stop.int()) else { return Fact::Any };
        if down {
            Fact::Int(stop_low.saturating_add(1).min(first.0), first.1)
        } else {
            Fact::Int(first.0, stop_high.saturating_sub(1).max(first.1))
        }
    }

    /// Writes what's known about a local: from here on in the block it's this, and every write's
    /// fact joins this, widened once it's grown a few times. Says whether that moved.
    fn write(&mut self, local: LocalId, fact: &Fact, state: &mut State) -> bool {
        if self.followed.get(local.0).copied().unwrap_or(false) {
            state.insert(local.0, fact.clone());
        }
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
    fn hold(state: &mut State, local: LocalId, items: Fact, replace: bool) {
        let held = if replace {
            items
        } else {
            state.get(&local.0).map_or_else(|| items.clone(), |known| known.join(&items))
        };
        state.insert(local.0, held);
    }

    /// The field's place among a struct type's fields.
    fn field_place(&self, object: &Operand, field: &str) -> Option<usize> {
        let Operand::Local(object) = object else { return None };
        let Type::Named(name) = &self.function.locals.get(object.0)?.ty else { return None };
        self.world.named.fields.get(name.as_str())?.iter().position(|(name, _)| name == field)
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
    fn step(&mut self, kind: &InstKind, state: &mut State, place: Place) -> bool {
        let block = place.0;
        let mut moved = false;
        match kind {
            InstKind::Copy { dest, value } => moved |= self.copy(*dest, value, state, block),
            InstKind::Unary { dest, op, value } => {
                let given = self.read(value, state, block);
                moved |= self.write(*dest, &unary(*op, &given), state);
            }
            InstKind::Binary { dest, op, lhs, rhs } => {
                let (left, right) = (self.read(lhs, state, block), self.read(rhs, state, block));
                if self.noting {
                    self.note_binary(*op, *dest, &left, &right, place);
                }
                moved |= self.write(*dest, &binary(*op, &left, &right), state);
            }
            InstKind::Make { dest, type_name, fields } => {
                let fields: Vec<Fact> = fields.iter().map(|field| self.read(field, state, block)).collect();
                moved |= self.make(*dest, type_name, fields, state);
            }
            InstKind::GetField { dest, object, field } => {
                let fact = self.field_read(object, field, state, block);
                moved |= self.put(*dest, &fact, state);
            }
            InstKind::SetField { object, path, value } => {
                let value = self.read(value, state, block);
                moved |= self.set_field(*object, path, &value, state, block);
            }
            InstKind::MakeTuple { dest, items } => {
                let items = items.iter().map(|item| self.read(item, state, block)).collect();
                moved |= self.write(*dest, &Fact::Tuple(items), state);
            }
            InstKind::GetTupleItem { dest, value, at } => {
                let fact = self.read(value, state, block).part(Some(*at));
                moved |= self.put(*dest, &fact, state);
            }
            InstKind::MakeList { dest, items } => {
                let held =
                    items.iter().fold(Fact::Unset, |held, item| held.join(&self.read(item, state, block)));
                // An `array` is made as a list is, but isn't followed as one.
                moved |= self.put(*dest, &Fact::List(Box::new(held)), state);
            }
            InstKind::GetIndex { dest, object, index } => {
                self.note_index(object, index, true, state, place);
                // A list's items, as the walk has them; anything else's are unknown.
                let fact = self.read(object, state, block).items();
                moved |= self.put(*dest, &fact, state);
            }
            InstKind::SetIndex { .. } => moved |= self.set_item(kind, state, place),
            InstKind::Builtin { .. } => moved |= self.builtin(kind, state, place),
            InstKind::Call { dest, function, args } => {
                moved |= self.call(*dest, function, args, state, block);
            }
            InstKind::Constant { dest, name } => {
                self.reads(name, 0, RETURN);
                let fact = self.world.returns.get(name).cloned().unwrap_or(Fact::Any);
                moved |= self.put(*dest, &fact, state);
            }
            InstKind::MakeClosure { dest, function, captures } => {
                let given: Vec<Fact> =
                    captures.iter().map(|capture| self.read(capture, state, block)).collect();
                self.note_given(function, &given);
                moved |= self.put(*dest, &Fact::Any, state);
            }
            other => {
                if let Some(dest) = other.dest() {
                    moved |= self.put(dest, &Fact::Any, state);
                }
            }
        }
        moved
    }

    /// A struct made: each field joins what every write of it gives. A `shared` value is a handle
    /// to a cell anything holding the handle can change, so nothing's known of what it holds
    /// from where it was made.
    fn make(&mut self, dest: LocalId, type_name: &str, fields: Vec<Fact>, state: &mut State) -> bool {
        for (at, fact) in fields.iter().enumerate() {
            self.note_field(type_name, at, fact);
        }
        let shared = self.world.named.shared.contains(type_name);
        let made = if shared { Fact::Any } else { Fact::Struct(fields) };
        self.write(dest, &made, state)
    }

    /// A field written along `path` from a local: what every write of that field gives joins
    /// it, and the local's own fact takes it.
    fn set_field(
        &mut self,
        object: LocalId,
        path: &[String],
        value: &Fact,
        state: &mut State,
        block: usize,
    ) -> bool {
        let ty = self.function.locals.get(object.0).map(|local| local.ty.clone());
        if let Some((name, at)) = ty.and_then(|ty| self.world.field_at(&ty, path)) {
            self.note_field(&name, at, value);
        }
        let known = self.read(&Operand::Local(object), state, block);
        let changed = self.with_field(object, &known, path, value);
        self.write(object, &changed, state)
    }

    /// An item set: in a list held in a local, or, along `path`, in a list a struct's field holds.
    fn set_item(&mut self, kind: &InstKind, state: &mut State, place: Place) -> bool {
        let InstKind::SetIndex { object, path, index, value } = kind else { return false };
        self.note_index(&Operand::Local(*object), index, path.is_empty(), state, place);
        let value = self.read(value, state, place.0);
        let ty = self.function.locals.get(object.0).map(|local| local.ty.clone());
        if let Some((name, at)) = ty.and_then(|ty| self.world.field_at(&ty, path)) {
            self.note_field(&name, at, &Fact::List(Box::new(value.clone())));
        }
        self.set_index(*object, path.is_empty(), &value, state)
    }

    /// A builtin: what it changes and what it gives back. An intrinsic can write any local it's
    /// handed, not only its first, as `channel_take` writes why it stopped.
    fn builtin(&mut self, kind: &InstKind, state: &mut State, place: Place) -> bool {
        let InstKind::Builtin { dest, name, args, changes } = kind else { return false };
        let given: Vec<Fact> = args.iter().map(|arg| self.read(arg, state, place.0)).collect();
        if *changes {
            self.changed_by(name, args, &given, state);
        }
        let mut moved = false;
        if name.starts_with("intrinsics.") {
            for arg in args.iter().skip(usize::from(*changes)) {
                let Operand::Local(local) = arg else { continue };
                if self.is_list(*local) {
                    Self::hold(state, *local, Fact::Any, true);
                    self.note_put(*local, &Fact::Any);
                } else {
                    moved |= self.write(*local, &Fact::Any, state);
                }
            }
        }
        let fact = self.gives(name, &given, place);
        moved | dest.is_some_and(|dest| self.put(dest, &fact, state))
    }

    /// A copy: a count's start joins what it's given, and the copy holds what was copied.
    fn copy(&mut self, dest: LocalId, value: &Operand, state: &mut State, block: usize) -> bool {
        let given = self.read(value, state, block);
        let mut moved = false;
        if self.counts.contains_key(&dest.0) {
            let start = self.starts.get(&dest.0).map_or_else(|| given.clone(), |known| known.join(&given));
            if self.starts.get(&dest.0) != Some(&start) {
                self.starts.insert(dest.0, start);
                moved = true;
            }
        }
        moved | self.put(dest, &given, state)
    }

    /// A write to an item: a list's own item joins what it holds, and anything deeper, or in
    /// something that isn't a list, knows nothing after.
    fn set_index(&mut self, object: LocalId, own_item: bool, value: &Fact, state: &mut State) -> bool {
        if !self.is_list(object) {
            return self.write(object, &Fact::Any, state);
        }
        let item = if own_item { value } else { &Fact::Any };
        Self::hold(state, object, item.clone(), !own_item);
        self.note_put(object, item);
        false
    }

    /// Writes a value to a local: a list's items to what it holds, anything else to its fact.
    fn put(&mut self, dest: LocalId, fact: &Fact, state: &mut State) -> bool {
        if self.is_list(dest) {
            Self::hold(state, dest, fact.items(), true);
            // A list parameter lent with `ref` written whole is what its caller holds after.
            self.note_put(dest, &fact.items());
            false
        } else {
            self.write(dest, fact, state)
        }
    }

    /// Notes a list's index known not negative, read or set straight into the list.
    fn note_index(&mut self, object: &Operand, index: &Operand, own_item: bool, state: &State, place: Place) {
        let Operand::Local(list) = object else { return };
        if self.noting && own_item && self.is_list(*list) && not_negative(&self.read(index, state, place.0)) {
            self.found.not_negative.insert(place);
        }
    }

    /// Notes what's put in a list parameter lent with `ref`.
    fn note_put(&mut self, object: LocalId, value: &Fact) {
        if let Some(put) = self.puts.get_mut(object.0) {
            *put = put.join(value);
        }
    }

    /// Notes what a `//`, `%`, `*` or float comparison can leave out.
    fn note_binary(&mut self, op: BinOp, dest: LocalId, left: &Fact, right: &Fact, place: Place) {
        match op {
            BinOp::FloorDiv | BinOp::Mod | BinOp::Mul => {
                let ints =
                    self.function.locals.get(dest.0).is_some_and(|local| matches!(local.ty, Type::Int));
                if ints && not_negative(left) && not_negative(right) {
                    self.found.not_negative.insert(place);
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
    fn changed_by(&mut self, name: &str, args: &[Operand], given: &[Fact], state: &mut State) {
        let Some(Operand::Local(first)) = args.first() else { return };
        if !self.is_list(*first) {
            self.write(*first, &Fact::Any, state);
            return;
        }
        match name {
            "list.append" | "list.insert" | "list.extend" => {
                let item = match name {
                    "list.append" => given.get(1).cloned(),
                    "list.insert" => given.get(2).cloned(),
                    _ => given.get(1).map(Fact::items),
                };
                let item = item.unwrap_or(Fact::Any);
                Self::hold(state, *first, item.clone(), false);
                self.note_put(*first, &item);
            }
            kept if ADDS_NOTHING.contains(&kept) => {}
            _ => {
                Self::hold(state, *first, Fact::Any, true);
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
            (length, _) if LENGTHS.contains(&length) => Fact::Int(0, i64::MAX),
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

    /// Notes, in the last round, what's written to a struct's field, by its place.
    fn note_field(&mut self, name: &str, at: usize, fact: &Fact) {
        if self.noting && self.world.followed_field(name, at) {
            let known = self.fields.entry((name.to_owned(), at)).or_insert(Fact::Unset);
            *known = known.join(fact);
        }
    }

    /// What reading a field gives: what's known of the object's field, or, where that's nothing,
    /// what every write of that field in the program gives it.
    fn field_read(&self, object: &Operand, field: &str, state: &State, block: usize) -> Fact {
        let at = self.field_place(object, field);
        let fact = self.read(object, state, block).part(at);
        if !matches!(fact, Fact::Any) {
            return fact;
        }
        let Operand::Local(local) = object else { return fact };
        let Some(Type::Named(name)) = self.function.locals.get(local.0).map(|local| &local.ty) else {
            return fact;
        };
        match at {
            Some(at) if self.world.followed_field(name, at) => {
                self.reads(name, at, FIELD);
                self.world.fields.get(&(name.clone(), at)).cloned().unwrap_or(Fact::Unset)
            }
            _ => fact,
        }
    }

    /// Notes, in the last round, what's given a function's first parameters: by a call, or as a
    /// closure's captures.
    fn note_given(&mut self, callee: &str, given: &[Fact]) {
        if !self.noting {
            return;
        }
        let params = self.calls.entry(callee.to_owned()).or_default();
        if params.len() < given.len() {
            params.resize(given.len(), Fact::Unset);
        }
        for (known, fact) in params.iter_mut().zip(given) {
            *known = known.join(fact);
        }
    }

    /// A call by name: what it gives each parameter, what the lists it's lent hold after, and
    /// what it gives back. Says whether a local moved.
    fn call(
        &mut self,
        dest: Option<LocalId>,
        callee: &str,
        args: &[Operand],
        state: &mut State,
        block: usize,
    ) -> bool {
        let given: Vec<Fact> = args.iter().map(|arg| self.read(arg, state, block)).collect();
        self.note_given(callee, &given);
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
                self.reads(callee, at, LEAVES);
                let leaves = self.world.leaves.get(callee).and_then(|all| all.get(at)).cloned().flatten();
                let puts = leaves.map_or(Fact::Unset, |leaves| leaves.puts);
                Self::hold(state, *arg, puts, every);
                // What the callee puts in a list this function was lent is put in it here too.
                let put = state.get(&arg.0).cloned().unwrap_or(Fact::Any);
                self.note_put(*arg, &put);
            } else {
                moved |= self.write(*arg, &Fact::Any, state);
            }
        }
        let Some(dest) = dest else { return moved };
        let fact = if known.is_some() {
            self.reads(callee, 0, RETURN);
            self.world.returns.get(callee).cloned().unwrap_or(Fact::Unset)
        } else {
            Fact::Any
        };
        moved | self.put(dest, &fact, state)
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
        for (key, fact) in std::mem::take(&mut self.fields) {
            let known = next.fields.entry(key).or_insert(Fact::Unset);
            *known = known.join(&fact);
        }
    }
}

/// The structs whose values can come from outside the program's own code, or be changed there:
/// one a builtin gives back or changes in place, other than a collection giving back what the
/// program put in it, or a parameter of a function something other than the program's own calls
/// may call, beyond a closure's captures; and every struct inside one of those.
fn outside_structs(
    program: &Program,
    other_ways: &HashSet<&str>,
    closures: &HashMap<&str, usize>,
) -> HashSet<String> {
    let mut made_outside: Vec<Type> = Vec::new();
    for function in every(program) {
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            let InstKind::Builtin { dest, name, args, changes } = &inst.kind else { continue };
            let holds = name == "Table"
                || ["Table.", "list.", "dict.", "set."].iter().any(|prefix| name.starts_with(prefix));
            if holds {
                continue;
            }
            if let Some(dest) = dest
                && let Some(local) = function.locals.get(dest.0)
            {
                made_outside.push(local.ty.clone());
            }
            if *changes
                && let Some(Operand::Local(first)) = args.first()
                && let Some(local) = function.locals.get(first.0)
            {
                made_outside.push(local.ty.clone());
            }
        }
        if other_ways.contains(function.name.as_str()) {
            let captured = closures.get(function.name.as_str()).copied().unwrap_or(0);
            made_outside.extend(
                function.locals.iter().take(function.params).skip(captured).map(|local| local.ty.clone()),
            );
        }
    }
    let mut outside = HashSet::new();
    while let Some(ty) = made_outside.pop() {
        named_in(&ty, &mut |name| {
            if outside.insert(name.to_owned()) {
                made_outside.extend(fields_of(program, name).into_iter().flatten().map(|(_, ty)| ty.clone()));
                let cases = program.enums.iter().filter(|def| def.name == name).flat_map(|def| &def.cases);
                made_outside.extend(cases.flat_map(|case| &case.fields).map(|(_, ty)| ty.clone()));
            }
        });
    }
    outside
}

/// The fields of a struct or an error, by its name.
fn fields_of<'p>(program: &'p Program, name: &str) -> Option<&'p [(String, Type)]> {
    let def = program.structs.iter().chain(&program.errors).find(|def| def.name == name)?;
    Some(&def.fields)
}

/// Calls `found` with each struct or other named type `ty` names, however deep.
fn named_in(ty: &Type, found: &mut dyn FnMut(&str)) {
    match ty {
        Type::Named(name) => found(name),
        Type::List(inner)
        | Type::Buffer(inner)
        | Type::Array(inner, _)
        | Type::Set(inner)
        | Type::Iterator(inner, None)
        | Type::Optional(inner)
        | Type::Id(inner)
        | Type::Table(inner) => named_in(inner, found),
        Type::Iterator(inner, Some(failure)) => {
            named_in(inner, found);
            named_in(failure, found);
        }
        Type::Dict(key, value) | Type::Result(key, value) => {
            named_in(key, found);
            named_in(value, found);
        }
        Type::Tuple(items) | Type::Union(items) => {
            for item in items {
                named_in(item, found);
            }
        }
        Type::Generic(name, args) => {
            found(name);
            for arg in args {
                named_in(arg, found);
            }
        }
        Type::Arrow(params, returns) => {
            for param in params {
                named_in(param, found);
            }
            named_in(returns, found);
        }
        _ => {}
    }
}

/// Each function only ever reached through a closure, with the fewest captures any closure made
/// of it has: one also called by name, or a generator's, or `main`, gets nothing from them.
fn closures(program: &Program) -> HashMap<&str, usize> {
    let mut closures: HashMap<&str, usize> = HashMap::new();
    let mut called: HashSet<&str> = HashSet::from(["main"]);
    for def in &program.generators {
        called.insert(def.name.as_str());
        called.insert(def.resume.as_str());
        called.extend(def.abandon.as_deref());
    }
    for inst in every(program).flat_map(|function| &function.blocks).flat_map(|block| &block.insts) {
        match &inst.kind {
            InstKind::MakeClosure { function, captures, .. } => {
                let fewest = closures.entry(function.as_str()).or_insert(captures.len());
                *fewest = (*fewest).min(captures.len());
            }
            InstKind::Call { function, .. } => {
                called.insert(function.as_str());
            }
            _ => {}
        }
    }
    closures.retain(|name, _| !called.contains(name));
    closures
}

/// Whether an int is known not to be negative. One nothing reaches can't be: an operation on it
/// never runs, so what's said of its sides is never wrong.
fn not_negative(fact: &Fact) -> bool {
    matches!(fact, Fact::Unset) || fact.int().is_some_and(|(low, _)| low >= 0)
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
            // An infinity by an infinity is NaN, whatever else the divisor can be.
            let infinities = (left.reaches(f64::INFINITY) || left.reaches(f64::NEG_INFINITY))
                && (right.reaches(f64::INFINITY) || right.reaches(f64::NEG_INFINITY));
            if right.holds_zero() {
                // A division by zero gives an infinity, or NaN for zero by zero.
                (f64::NEG_INFINITY, f64::INFINITY, nan || left.holds_zero() || infinities)
            } else {
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
                // The least of a NaN and a number is the number, so a side that can be NaN lets
                // the result be as high as the other side goes.
                let mut high = one.high.min(other.high);
                if one.nan {
                    high = high.max(other.high);
                }
                if other.nan {
                    high = high.max(one.high);
                }
                Float { low: one.low.min(other.low), high, nan: one.nan && other.nan }
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
                // Not one number: a range that ends at it ends one short of it.
                BinOp::NotEq if other_low == other_high && low == *other_low => (low.saturating_add(1), high),
                BinOp::NotEq if other_low == other_high && high == *other_low => {
                    (low, high.saturating_sub(1))
                }
                _ => (low, high),
            };
            // No value passes the test, so nothing reaches here.
            if low > high { Fact::Unset } else { Fact::Int(low, high) }
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
            if narrowed.empty() && !narrowed.nan { Fact::Unset } else { Fact::Float(narrowed) }
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
        BinOp::Eq => BinOp::NotEq,
        _ => return None,
    })
}

/// The blocks the start of a function reaches, in reverse postorder: each after every block that
/// leads to it, except along a way back round a loop.
fn walk_order(function: &Function) -> Vec<usize> {
    let mut seen = vec![false; function.blocks.len()];
    let mut finished = Vec::with_capacity(function.blocks.len());
    // Each block on the way down, with how many of its targets have been looked at.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    if let Some(first) = seen.first_mut() {
        *first = true;
        stack.push((0, 0));
    }
    while let Some((block, next)) = stack.last().copied() {
        let targets = function.blocks.get(block).map(|found| found.end.targets()).unwrap_or_default();
        if let Some(target) = targets.get(next) {
            if let Some(top) = stack.last_mut() {
                top.1 += 1;
            }
            if let Some(slot) = seen.get_mut(target.0)
                && !*slot
            {
                *slot = true;
                stack.push((target.0, 0));
            }
        } else {
            finished.push(block);
            stack.pop();
        }
    }
    finished.reverse();
    finished
}

/// The comparison a block's branch made, which way it came out on the way to `to`, and where in
/// the block it was made.
fn comparison_along(from: &Block, to: usize) -> Option<(BinOp, Operand, Operand, bool, usize)> {
    let Terminator::Branch { cond: Operand::Local(cond), then, otherwise } = &from.end else { return None };
    let holds = if then.0 == to && otherwise.0 != to {
        true
    } else if otherwise.0 == to && then.0 != to {
        false
    } else {
        return None;
    };
    // The last write of the condition, which must be the comparison itself.
    let (at, last) = from.insts.iter().enumerate().rev().find(|(_, inst)| may_write(&inst.kind, *cond))?;
    match &last.kind {
        InstKind::Binary { dest, op, lhs, rhs } if dest == cond => {
            Some((*op, lhs.clone(), rhs.clone(), holds, at))
        }
        _ => None,
    }
}

/// The locals that hold the same value as `local` where a block ends, itself included: each copy
/// made in the block of one of them, or that one was made from, where nothing writes either after.
fn same_as(block: &Block, local: LocalId) -> Vec<LocalId> {
    let copies: Vec<(LocalId, LocalId)> = block
        .insts
        .iter()
        .enumerate()
        .filter_map(|(at, inst)| match &inst.kind {
            InstKind::Copy { dest, value: Operand::Local(from) }
                if !written_after(block, at, *dest) && !written_after(block, at, *from) =>
            {
                Some((*dest, *from))
            }
            _ => None,
        })
        .collect();
    let mut same = vec![local];
    let mut grew = true;
    while grew {
        grew = false;
        for (dest, from) in &copies {
            for (one, other) in [(dest, from), (from, dest)] {
                if same.contains(one) && !same.contains(other) {
                    same.push(*other);
                    grew = true;
                }
            }
        }
    }
    same
}

/// Whether anything in a block after the instruction at `at` may write a local: as what it
/// gives, as what it changes in place, or as what it hands a call or a builtin that may change it.
fn written_after(block: &Block, at: usize, local: LocalId) -> bool {
    block.insts.iter().skip(at.saturating_add(1)).any(|inst| may_write(&inst.kind, local))
}

/// Whether an instruction may write a local: as what it gives, as what it changes in place, or
/// as what it hands a call or a builtin that may change it.
fn may_write(kind: &InstKind, local: LocalId) -> bool {
    match kind {
        InstKind::SetField { object, .. } | InstKind::SetIndex { object, .. } if *object == local => true,
        InstKind::Call { args, .. } | InstKind::Builtin { args, .. }
            if args.contains(&Operand::Local(local)) =>
        {
            true
        }
        other => other.dest() == Some(local),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A list let go where a block starts holds anything there, so what's appended to it after
    /// joins anything. A list missing from what's known is one not yet made, which would take
    /// only what's appended.
    #[test]
    fn a_list_let_go_holds_anything() {
        let source = "def main(stdout: Stdout):\n    mut values = [1, 2]\n    values.append(3)\n    stdout.print(values)\n";
        let mut module = fennec_syntax::parse(source).unwrap();
        let checked = fennec_check::check_and_resolve(&mut module).unwrap();
        let program = crate::lower(&module, &checked);
        let whole = Whole::of(&program);
        let world = whole.world();
        let main = program.functions.iter().find(|function| function.name == "main").unwrap();
        let walk = Walk::new(&world, main);
        let list = (0..main.locals.len()).map(LocalId).find(|local| walk.is_list(*local)).unwrap();
        let mut state = State::from([(list.0, Fact::Int(1, 2))]);
        walk.let_go_of(&BTreeSet::from([list.0]), &mut state);
        assert_eq!(state.get(&list.0), Some(&Fact::Any));
        Walk::hold(&mut state, list, Fact::Int(3, 3), false);
        assert_eq!(state.get(&list.0), Some(&Fact::Any));
    }

    /// The rounds as they were before only what read a changed fact was walked again: every
    /// function walked in every round, what each finds as the last round had it.
    fn walking_every_function(program: &Program) -> Ranges {
        let whole = Whole::of(program);
        let mut world = whole.world();
        let mut found = BTreeMap::new();
        for _ in 0..MOST_ROUNDS {
            let mut next = Next::default();
            found.clear();
            let mut unsettled = None;
            for function in every(program) {
                match world.function(function, &mut next).0 {
                    Some(ranges) => {
                        found.insert(function.name.clone(), ranges);
                    }
                    None => {
                        unsettled = unsettled.or_else(|| Some(Unsettled::Function(function.name.clone())));
                    }
                }
            }
            if world.take(next).is_empty() {
                return match unsettled {
                    None => Ranges { functions: found, unsettled: None },
                    Some(unsettled) => Ranges { functions: BTreeMap::new(), unsettled: Some(unsettled) },
                };
            }
        }
        Ranges { functions: BTreeMap::new(), unsettled: Some(Unsettled::Program) }
    }

    /// Walking again only the functions that read a fact the round before changed finds just
    /// what walking every function every round does, for every kernel, example and real program.
    #[test]
    fn walking_again_only_what_changed_finds_what_walking_everything_does() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut programs = 0;
        for folder in ["bench/kernels", "examples", "bench/realworld"] {
            let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join(folder))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|extension| extension == "fen"))
                .collect();
            files.sort();
            for path in files {
                let source = std::fs::read_to_string(&path).unwrap();
                let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
                let Ok(checked) = fennec_check::check_and_resolve(&mut module) else { continue };
                let program = crate::lower(&module, &checked);
                assert!(Ranges::of(&program) == walking_every_function(&program), "{}", path.display());
                programs += 1;
            }
        }
        assert!(programs >= 100, "only {programs} programs");
    }

    /// Scratch: writes what's found for every kernel, example and real program, to compare.
    #[test]
    fn scratch_dump_ranges() {
        let Some(out) = std::env::var_os("RANGES_DUMP_DIR") else { return };
        let out = std::path::PathBuf::from(out);
        std::fs::create_dir_all(&out).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for folder in ["bench/kernels", "examples", "bench/realworld"] {
            for entry in std::fs::read_dir(root.join(folder)).unwrap() {
                let path = entry.unwrap().path();
                if !path.extension().is_some_and(|extension| extension == "fen") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let Ok(mut module) = fennec_syntax::parse(&source) else { continue };
                let Ok(checked) = fennec_check::check_and_resolve(&mut module) else { continue };
                let program = crate::lower(&module, &checked);
                let name = format!(
                    "{}-{}.txt",
                    folder.replace('/', "_"),
                    path.file_stem().unwrap().to_string_lossy()
                );
                std::fs::write(out.join(name), format!("{:#?}", Ranges::of(&program))).unwrap();
            }
        }
    }
}
