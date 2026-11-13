//! The programs the `programs` target writes, as a tree: typed by construction, so every one the
//! generator makes is one the checker takes. The same tree is written out as Fennec and run by
//! the reference evaluator.

/// The types a program's values have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ty {
    Int,
    Bool,
    Str,
    Ints,
    Point,
    Shape,
    /// `u8`, `i32` or `u64`.
    Sized(Size),
    /// `shared Point`, reached only through `lock`.
    Shared,
    /// What `tallied` lends a `with` block.
    Tally,
    /// `shared Bag`: a list and a dict, reached only through `lock`.
    Bag,
    /// `Holder`: a struct holding a list.
    Holder,
    /// `dict[str, list[int]]`.
    Lists,
    /// `() -> int`: a function that sums the list it captured.
    Count,
    Float,
    /// `dict[str, int]`.
    Counts,
    /// `Buffer[int]`.
    Buf,
    /// `shared Outer`: a number and a `shared Bag`.
    Outer,
    /// `Node`: an enum whose cases hold `str`, `list[str]` and `list[Node]`, a tree.
    Node,
    /// `list[Node]`.
    Nodes,
    /// A `Node` from `deep`, thousands of levels deep, only measured, never printed whole.
    Deep,
    /// `Named`: a recursive enum with no list of itself, `Pair` holding two more, which is only
    /// ever read through its digest, never printed, compared, shared or sent, so native code can
    /// keep it in blocks of its own.
    Named,
    /// A `Named` from `chain`, thousands of pairs deep, only measured.
    Chain,
}

/// The types a value made anywhere can have. A `shared Point` and a lent `Tally` come from their
/// own statements.
pub(crate) const TYPES: [Ty; 17] = [
    Ty::Int,
    Ty::Bool,
    Ty::Str,
    Ty::Ints,
    Ty::Point,
    Ty::Shape,
    Ty::Holder,
    Ty::Lists,
    Ty::Float,
    Ty::Sized(Size::U8),
    Ty::Sized(Size::I32),
    Ty::Sized(Size::U64),
    Ty::Counts,
    Ty::Buf,
    Ty::Node,
    Ty::Nodes,
    Ty::Named,
];

/// A sized integer type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    U8,
    I32,
    U64,
}

impl Size {
    /// Its name in Fennec.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Size::U8 => "u8",
            Size::I32 => "i32",
            Size::U64 => "u64",
        }
    }

    /// The least and the most it holds.
    pub(crate) const fn range(self) -> (i128, i128) {
        match self {
            Size::U8 => (0, 255),
            Size::I32 => (-2_147_483_648, 2_147_483_647),
            Size::U64 => (0, 18_446_744_073_709_551_615),
        }
    }
}

/// An arithmetic operator on ints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// A comparison between ints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Compare {
    Less,
    LessEq,
    Equal,
    NotEqual,
    Greater,
}

/// A case of `Shape`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Case {
    Circle,
    Square,
    Empty,
}

#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Int(i64),
    Bool(bool),
    Str(String),
    Name(String),
    Arith(Arith, Box<Expr>, Box<Expr>),
    /// `left % 1009`, keeping a number small where a program isn't meant to fail.
    Small(Box<Expr>),
    Compare(Compare, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    /// `a if condition else b`.
    Choose(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `left + right` on strings.
    Concat(Box<Expr>, Box<Expr>),
    /// `f"<{int}>"`.
    Format(Box<Expr>),
    /// `len(text)`.
    TextLen(Box<Expr>),
    /// A list of ints written out.
    Ints(Vec<Expr>),
    /// `[item * by + plus for item in list]`, or over `parallel(list)` when the flag says so.
    Mapped(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    /// `sum(item * by for item in parallel(list))`.
    ParallelSum(Box<Expr>, Box<Expr>),
    /// `sum(regrown(item) for item in parallel(list))`: each round makes a holder, reads its list
    /// and passes it to a `ref self` method on the same holder.
    Regrown(Box<Expr>),
    /// `len(list)`.
    Len(Box<Expr>),
    /// `list[at]`, which fails outside the list.
    Index(Box<Expr>, Box<Expr>),
    /// `at_or_zero(list, at)`, which never fails.
    AtOrZero(Box<Expr>, Box<Expr>),
    /// `sum(list)`.
    Sum(Box<Expr>),
    /// `Point(x=..., y=...)`.
    Point(Box<Expr>, Box<Expr>),
    /// `point.x` or `point.y`.
    Field(Box<Expr>, bool),
    /// `point.total()`.
    Total(Box<Expr>),
    /// A shape of a case, with its number.
    Shape(Case, Box<Expr>),
    /// `area(shape)`.
    Area(Box<Expr>),
    /// `bump(ref name, by)`: adds `by` to an int name and gives ten times what it holds then.
    Bump(String, Box<Expr>),
    /// `u8(int)`, `i32(int)` or `u64(int)`, which fails outside what the size holds.
    Convert(Size, Box<Expr>),
    /// `left + - * right` on one size, checked against it.
    SizedArith(Size, Arith, Box<Expr>, Box<Expr>),
    /// `left.wrapping_add(right)` on one size.
    SizedWrap(Size, Box<Expr>, Box<Expr>),
    /// `int(sized)`, which fails for a `u64` past the ints.
    IntOf(Box<Expr>),
    /// `sum(counted(limit))`, through a generator.
    SumCounted(Box<Expr>),
    /// `tally.count`, in the `with` block it's lent to.
    Count(String),
    /// `Holder(items=list, tag=int)`.
    Holder(Box<Expr>, Box<Expr>),
    /// `holder.items`.
    HeldItems(Box<Expr>),
    /// `holder.tag`.
    Tag(Box<Expr>),
    /// A `dict[str, list[int]]` written out.
    Lists(Vec<(&'static str, Expr)>),
    /// `lists.get(key) else list[int]()`.
    ListAt(Box<Expr>, &'static str),
    /// `() => sum(list)`, capturing a name that can't change.
    Summing(String),
    /// `count()`.
    Called(String),
    /// A `dict[str, int]` written out.
    Counts(Vec<(&'static str, Expr)>),
    /// `counts.get(key) else 0`.
    CountOf(Box<Expr>, &'static str),
    /// `key in counts`.
    Has(Box<Expr>, &'static str),
    /// `Buffer[int]()`, or with `capacity=`, then each item pushed.
    Buffer(Option<i64>, Vec<Expr>),
    /// `[item for item in buffer]`.
    BufItems(Box<Expr>),
    /// `len(buffer)`.
    BufLen(Box<Expr>),
    /// `buffer[at]`, which fails outside what's filled.
    BufAt(Box<Expr>, Box<Expr>),
    /// `Node.Leaf(name=text)`.
    Leaf(Box<Expr>),
    /// `Node.Number(value=int)`.
    Number(Box<Expr>),
    /// `Node.Branch(label=text, children=nodes)`.
    Branch(Box<Expr>, Box<Expr>),
    /// `Node.Tagged(tags=texts, inner=nodes)`.
    Tagged(Box<Expr>, Box<Expr>),
    /// A `list[str]` written out.
    Texts(Vec<Expr>),
    /// A `list[Node]` written out.
    NodeList(Vec<Expr>),
    /// `node_or_leaf(nodes, at)`, which never fails.
    NodeAt(Box<Expr>, Box<Expr>),
    /// `digest(node)`: the tree as text, through the program's own recursive function.
    Digest(Box<Expr>),
    /// `digest_all(nodes)`.
    DigestAll(Box<Expr>),
    /// `deep(depth, label)`: a chain of branches `depth` deep, built in a loop.
    Deep(i64, Box<Expr>),
    /// `Named.Leaf(name=text)`.
    NamedLeaf(Box<Expr>),
    /// `Named.Pair(left=named, right=named, label=text, tags=texts)`.
    NamedPair(Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>),
    /// `chain(depth, label)`: pairs `depth` deep down their left side, built in a loop.
    Chain(i64, Box<Expr>),
    /// `named_digest(named)`, the only way a program reads a whole `Named`.
    NamedDigest(Box<Expr>),
    /// `named_depth(named)`, walking left sides in a loop.
    NamedDepth(Box<Expr>),
    /// `depth_of(node)`, walking first children in a loop.
    DepthOf(Box<Expr>),
    /// A float written out.
    Float(f64),
    /// `left + - * / right` on floats: `/` by zero fails.
    FloatArith(Arith, Box<Expr>, Box<Expr>),
    /// `float(int)`.
    FloatOf(Box<Expr>),
    /// `int(float)`, which fails outside the ints, and for infinity and NaN.
    Truncated(Box<Expr>),
    /// A comparison between floats.
    FloatCompare(Compare, Box<Expr>, Box<Expr>),
}

/// A number written into a lock's body: a literal, or what a task was sent, times something.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Amount {
    Literal(i64),
    /// `number * by`, in a task's body.
    Sent(i64),
}

/// A change to a `Bag`'s list or dict under `lock bag as ref held:`.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    /// `held.items.append(amount)`.
    Append(Amount),
    /// `_ = held.items.pop()`.
    Pop,
    /// `held.items[at] = amount`, or `+=` with `add`: a place outside the list crashes.
    SetItem(i64, Amount, bool),
    /// `held.counts[key] = amount`, or `+=` with `add`: a key that isn't there crashes then.
    SetCount(&'static str, Amount, bool),
    /// `_ = held.counts.remove(key)`.
    Remove(&'static str),
    /// `held.add_twice(amount)`, a `ref self` method appending it twice.
    AddTwice(Amount),
    /// `grow_all(ref held.items, amount)`, through a `ref` parameter.
    GrowItems(Amount),
    /// `ref slot = held.items[at]`, then `slot += amount` and `slot = slot * 2`, reading it: a
    /// place outside the list crashes at the `ref`.
    RefItem(i64, Amount),
    /// `mut kept = held.counts`, `kept[key] = amount`, then `held.counts = kept`: the dict
    /// changed through a copy and stored back, which a failure or a crash must undo too.
    CountsCopied(&'static str, Amount),
    /// `held.items = held.items` and `held.counts = held.counts`, which change nothing.
    SelfAssigned,
    /// `_ = held.counts.pop(key)`, which crashes on a key that isn't there.
    PopCount(&'static str),
}

/// What an arrow function that fails is given to.
#[derive(Clone, Debug)]
pub(crate) enum Arrow {
    /// `tried`, which prints the value or what it failed with.
    Tried,
    /// `tried_twice`, which calls it twice, passing a failure up with `?` to the call's `else`.
    Twice,
    /// `tried_each` with a list, the arrow taking each item.
    Each(Expr),
    /// `ran_fine`, printing whether it gave its value rather than failing.
    RanFine,
    /// `fallback`, whose failure block gives the value, `why.code * 100`, as its last line.
    Fallback,
    /// `either`, the arrow failing two ways: `capped(n)?`, then `shut_below(other)?`.
    Either(Expr),
}

/// A `Turned` loop: `turned{id}`, of `rounds` turns, its int starting at `start` and changing sign
/// by `how`, stepping down by `step`, or reset to `reset_to` on turn `reset_at`.
#[derive(Clone, Debug)]
pub(crate) struct Turning {
    pub(crate) id: usize,
    pub(crate) how: Turn,
    pub(crate) rounds: i64,
    pub(crate) start: i64,
    pub(crate) step: i64,
    pub(crate) reset_at: i64,
    pub(crate) reset_to: i64,
}

/// A `Streamed` stream: `stream = (f(item) for item in parallel(xs))` over `items`, where `f` is
/// `map`, read by `read`. The runtime runs it as an ordered stream: items worked out on several
/// threads, read in order, a failure cutting it short where it's read.
#[derive(Clone, Debug)]
pub(crate) struct Streaming {
    pub(crate) id: usize,
    pub(crate) items: Vec<i64>,
    pub(crate) map: StreamMap,
    pub(crate) read: StreamRead,
}

/// What a `Streamed` stream works out from each item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamMap {
    /// `item * by + plus`, which can't fail.
    Scaled(i64, i64),
    /// `100 // (item - zero)`, which divides by zero at an item equal to `zero`.
    Divided(i64),
    /// `ys[item]`, of `YS`, which is out of range at an item of 4 or more.
    Indexed,
}

/// The list a `StreamMap::Indexed` stream indexes.
pub(crate) const YS: [i64; 4] = [10, 20, 30, 40];

/// What a name in an `Aliased` function holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// `list[int]`.
    Numbers,
    /// `list[Holder]`.
    Holders,
    Holder,
    /// `dict[str, list[int]]`.
    Lists,
    /// `dict[str, Point]`.
    Spots,
    /// `Table[Point]`.
    Table,
    Point,
    Text,
    /// `dict[str, Holder]`: plain structs holding lists, as a dict's values.
    Keepers,
    /// `Ledger{id}`: a struct holding a `Table[Point]` and a count.
    Ledger,
    /// A `Ledger`'s table taken out whole, its items found by the ledger's `Id`s.
    Entries,
    /// `dict[str, int]`.
    Counts,
    /// `Table[Room{id}]`: items each holding a dict of counts and a label.
    Rooms,
}

impl Held {
    /// What a part of it holds: an item, a value at a key, or a field holding a collection.
    pub(crate) const fn part(self) -> Option<Held> {
        match self {
            Held::Holders | Held::Keepers => Some(Held::Holder),
            Held::Holder | Held::Lists => Some(Held::Numbers),
            Held::Spots | Held::Table | Held::Entries => Some(Held::Point),
            Held::Ledger => Some(Held::Entries),
            Held::Rooms => Some(Held::Counts),
            Held::Numbers | Held::Point | Held::Text | Held::Counts => None,
        }
    }

    /// Whether it's a dict whose values a fresh value can be put in at a key.
    pub(crate) const fn keyed(self) -> bool {
        matches!(self, Held::Lists | Held::Spots | Held::Keepers | Held::Counts)
    }

    /// Whether it's a dict, or a table of items holding dicts, that a walk reads.
    pub(crate) const fn walked(self) -> bool {
        matches!(self, Held::Lists | Held::Spots | Held::Keepers | Held::Counts | Held::Rooms)
    }
}

/// The names every `Aliased` function starts with, in order, and what each holds.
pub(crate) const STARTING: [(&str, Held); 10] = [
    ("numbers", Held::Numbers),
    ("holders", Held::Holders),
    ("lists", Held::Lists),
    ("spots", Held::Spots),
    ("table", Held::Table),
    ("text", Held::Text),
    ("keepers", Held::Keepers),
    ("ledger", Held::Ledger),
    ("counts", Held::Counts),
    ("rooms", Held::Rooms),
];

/// Where `STARTING`'s table is, whose `Id`s, `first_id` and `second_id`, its copies share.
pub(crate) const TABLE: usize = 4;

/// Where `STARTING`'s ledger is, whose table's `Id`s are `entry_a` and `entry_b`.
pub(crate) const LEDGER: usize = 7;

/// Where `STARTING`'s rooms are, whose `Id`s are `room_a` and `room_b`.
pub(crate) const ROOMS: usize = 9;

/// The keys of an `Aliased` function's dicts. Each is always there but while a `Removed` step
/// walks what's left, which then puts it back, last. Each list starts with two items and never
/// loses one.
pub(crate) const KEYS: [&str; 2] = ["a", "b"];

/// An `Aliased` function: names starting as copies of others' values or parts, changed through
/// each way of writing in place, then each printed. Each name must hold only what was written
/// to it, never what was written to the one it was copied from, or to one copied from it.
#[derive(Clone, Debug)]
pub(crate) struct Aliasing {
    pub(crate) id: usize,
    /// What the starting values are made from.
    pub(crate) base: i64,
    pub(crate) steps: Vec<Alias>,
}

impl Aliasing {
    /// What each name holds, the starting ones first, then one for each copy in order.
    pub(crate) fn names(&self) -> Vec<Held> {
        let mut held: Vec<Held> = STARTING.iter().map(|(_, held)| *held).collect();
        for step in &self.steps {
            match *step {
                Alias::Copied { from } => held.push(held.get(from).copied().unwrap_or(Held::Numbers)),
                Alias::Part { from, .. } => {
                    held.push(held.get(from).and_then(|from| from.part()).unwrap_or(Held::Numbers));
                }
                Alias::Built { .. } => held.push(Held::Holder),
                Alias::Read { how: Reading::Kept, .. } => held.push(Held::Point),
                Alias::Written { .. }
                | Alias::Tasked { .. }
                | Alias::Walked { .. }
                | Alias::Drained { .. }
                | Alias::Read { .. } => {}
            }
        }
        held
    }
}

/// The name of an `Aliased` function's `n`th name: a starting one's, or `kept1` on for copies.
pub(crate) fn alias_name(n: usize) -> String {
    STARTING.get(n).map_or_else(|| format!("kept{}", n + 1 - STARTING.len()), |(name, _)| (*name).to_owned())
}

/// A value an `Aliased` function holds, as the reference evaluator keeps it. A dict's entries
/// are in the order their keys went in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    Numbers(Vec<i64>),
    /// Each `Holder`'s items and tag.
    Holders(Vec<(Vec<i64>, i64)>),
    Holder(Vec<i64>, i64),
    Lists(Vec<(String, Vec<i64>)>),
    Spots(Vec<(String, (i64, i64))>),
    /// Each point, the first with `first_id` and the second with `second_id`.
    Table(Vec<(i64, i64)>),
    Point(i64, i64),
    Text(String),
    Keepers(Vec<(String, (Vec<i64>, i64))>),
    /// Its table's points, `entry_a`'s first, and its count.
    Ledger(Vec<(i64, i64)>, i64),
    Entries(Vec<(i64, i64)>),
    Counts(Vec<(String, i64)>),
    /// Each room's counts and label, `room_a`'s first.
    Rooms(Vec<(Vec<(String, i64)>, String)>),
}

/// What `STARTING`'s names hold at first, made from `base`.
pub(crate) fn starting(base: i64) -> Vec<Kept> {
    let keyed = |values: [i64; 2]| -> Vec<(String, i64)> {
        KEYS.iter().zip(values).map(|(key, value)| ((*key).to_owned(), value)).collect()
    };
    vec![
        Kept::Numbers(vec![base, base + 1]),
        Kept::Holders(vec![(vec![base + 2, base + 3], base + 4), (vec![base + 5, base + 6], base + 7)]),
        Kept::Lists(vec![("a".to_owned(), vec![base, base + 8]), ("b".to_owned(), vec![base + 9, base + 1])]),
        Kept::Spots(vec![("a".to_owned(), (base, base + 1)), ("b".to_owned(), (base + 2, base + 3))]),
        Kept::Table(vec![(base, 0), (base + 1, 0)]),
        Kept::Text(format!("t{base}")),
        Kept::Keepers(vec![
            ("a".to_owned(), (vec![base + 1, base + 2], base + 3)),
            ("b".to_owned(), (vec![base + 4, base + 5], base + 6)),
        ]),
        Kept::Ledger(vec![(base + 2, 0), (base + 3, 0)], base),
        Kept::Counts(keyed([base, base + 1])),
        Kept::Rooms(vec![(keyed([base, 1]), "r".to_owned()), (keyed([2, base]), "s".to_owned())]),
    ]
}

/// A step of an `Aliased` function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Alias {
    /// `kept{n} = from`, a copy of a whole value.
    Copied { from: usize },
    /// `kept{n} = from[at]`, by place, key or `Id`, or a field holding a collection.
    Part { from: usize, at: usize },
    /// A change to `to`, at place, key or `Id` `at` where it takes one, by `n`.
    Written { to: usize, how: Writing, at: usize, n: i64 },
    /// `[handed{id}_{kind}(from, n=i) for i in parallel(range(3))]`, each task changing its own
    /// copy of what it's handed and giving back a number made from it, printed.
    Tasked { from: usize },
    /// A dict's `values()`, `keys()` and `items()` walked, or each room's dict in a table of
    /// them, printed.
    Walked { from: usize },
    /// `spare{s} = from`, then `kept{n} = Holder(items=spare{s}, tag=n)`: a struct literal taking
    /// a list nothing reads again.
    Built { from: usize, n: i64 },
    /// A `for item in table` loop that only reads, each item borrowed from its slot, over a
    /// `Table`, a ledger's table field or the rooms, done as `how` with `n`. `Reading::Kept`
    /// makes a name: the last item read with an `x` of `n` or more.
    Read { from: usize, how: Reading, n: i64 },
    /// `mut last{s} = from`, its key `at` set to a fresh value made from `n`, then
    /// `print(last{s}.values())` as the dict's last use, so the walk is all that holds what it
    /// gives back once the dict is gone.
    Drained { from: usize, at: usize, n: i64 },
}

/// How a `Read` step's loop reads a table's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Each item's fields summed.
    Summed,
    /// The last item with an `x` of `n` or more kept by another name, printed at the end.
    Kept,
    /// The table replaced, on the first item, by a copy of it held apart, which frees the block
    /// being read; each item read is printed after.
    Replaced,
    /// `break` at the first item whose `x` is over `n`.
    Broken,
    /// `first_over{id}(table, n)`: the first item whose `x` is over `n`, returned from its loop.
    Returned,
    /// `refused_over{id}(table, n)`: the `y`s summed, failing at the first `x` over `n`.
    Failed,
}

/// How an `Aliased` step changes a name, each meaning what it says for the kinds that take it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Writing {
    /// `to.append(n)`, or `to[key].append(n)` / `to[at].items.append(n)` / `to.items.append(n)`.
    Append,
    /// `to[at] = n`, or `to[key][0] = n`.
    Set,
    /// `for ref item in to:`, changing each.
    ForRef,
    /// `to[at].tag = n`, `to[key].x += n`, `to[id].x = n`, `to.tag = n` or `to.x = n`.
    Field,
    /// `mut taken = to[at]`, changed, then `to[at] = taken`.
    PutBack,
    /// A `ref self` method: `grown_by` or `moved`.
    Method,
    /// `poked{id}(ref to, n=n)`, a `ref` parameter.
    Ref,
    /// `to = to + piece`.
    Joined,
    /// `to += piece`.
    Added,
    /// `to = f"{to}piece"`.
    Formatted,
    /// `to = to + piece + to`, a later piece reading what's being grown.
    Chained,
    /// `to += piece + to + "ç"`, three pieces, one of them what's being grown.
    Summed,
    /// `to += piece`, a piece past 23 bytes with letters past ASCII.
    Long,
    /// `_ = to.remove(key)`, a walk of what's left, then the key put back with a new value.
    Removed,
}

/// A `Constants` statement: module constants holding a list, a dict, an empty list of pairs and
/// a struct, made from `base`, read `reads` times on the main thread first, then used and
/// changed in copies by `used{id}(n)`: on the main thread first when `main_first`, in a launched
/// task and in `parallel`'s bodies, in the order `launch_first` says, then on the main thread
/// again. A struct's fields default to the constants and are changed through `ref self`
/// methods. Each use must see the constants as they started, and its changes as made.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Constants {
    pub(crate) id: usize,
    pub(crate) base: i64,
    pub(crate) reads: i64,
    pub(crate) main_first: bool,
    pub(crate) launch_first: bool,
}

/// A `FieldCalls` statement: `box.field += f(...)` on a struct's `str`, `int` and `list[int]`
/// fields, each call printing, failing, or reading the field through another path, so the order
/// the field is read in and the call made shows, and a failure must leave the field as it was.
#[derive(Clone, Debug)]
pub(crate) struct FieldCalls {
    pub(crate) id: usize,
    pub(crate) base: i64,
    pub(crate) steps: Vec<FieldCall>,
}

/// One `+=` of a `FieldCalls`: on `field`, the call made as `call`, with `a + f(b)` when `plus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FieldCall {
    pub(crate) field: Field,
    pub(crate) call: Calling,
    pub(crate) plus: bool,
    pub(crate) n: i64,
}

/// A field of a `FieldCalls` struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Field {
    Text,
    Count,
    Items,
}

/// What the call on a `FieldCall`'s right side does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Calling {
    /// Prints, then gives a value made from `n`.
    Printed,
    /// `f(n)?` in a function taking the struct by `ref`, failing when `n` is over 5, which is
    /// handled with `except` where it's called.
    Failed,
    /// Reads the field through a copy of the struct, made just before, and prints it.
    Copied,
    /// Reads the field through a list holding a copy of the struct, and prints it.
    Listed,
    /// A closure made just before, which captured the struct, gives the field.
    Closure,
    /// The `+=` is to a `shared` struct's field, under `lock`, with a call that prints.
    Shared,
}

/// A `Yielded` statement: generators yielding a struct holding a text, a list and a dict, one
/// kept in a local and changed across yields, tuples and enum cases holding texts and lists,
/// each value kept and read after more yields, then sent through a task's inbox loop and back.
/// The texts are `widths` long, past the 23 bytes a str holds in place.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Yielding {
    pub(crate) id: usize,
    pub(crate) count: i64,
    pub(crate) widths: [i64; 3],
}

/// A `Woken` statement: a launched task answering each of `count` numbers over a channel, which
/// crashes on `at`, or fails with `Refused` when `failing`, while its launcher waits for the
/// answer. The launcher's wait has to end, and its block with the task's crash or failure.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Waking {
    pub(crate) id: usize,
    pub(crate) count: i64,
    pub(crate) at: i64,
    pub(crate) failing: bool,
    /// How many times it's done, each with a channel of its own made and freed, as many as 70
    /// so threads' caches of the channels they found have to trim.
    pub(crate) rounds: i64,
}

/// How a `Streamed` stream is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamRead {
    /// A `for` loop printing each, breaking after the `n`th when it's given.
    Loop(Option<i64>),
    /// `sum(y for y in stream)`.
    Sum,
    /// `[y + 1 for y in stream]`.
    Listed,
}

/// How a `Turned` loop's int changes sign after its first turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Turn {
    /// `at = start`, then `at -= step` each turn: stepped down past 0.
    StepDown,
    /// `sign * start`, with `sign` flipped each turn.
    Flip,
    /// `n = start`, then `n += 1` each turn, reset to `reset_to`, below 0, on turn `reset_at`.
    Reset,
}

/// The list a `Turned` loop indexes: 4 items, so the places from -4 to 3 are in it.
pub(crate) const XS: [i64; 4] = [10, 20, 30, 40];

/// How round `at` of a `Left` loop leaves its `with` blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leave {
    /// `got = present(i, at) else return`.
    ElseReturn,
    /// `got = present(i, at) else fail Refused(code=i)`.
    ElseFail,
    /// `got = present(i, at) else continue`.
    ElseContinue,
    /// `got = present(i, at) else break`.
    ElseBreak,
    /// `got = capped(i, limit=at - 1)?`, a line of its own.
    Question,
    /// `total = total + capped(i, limit=at - 1)? * 2`, inside an expression.
    QuestionInside,
    /// As [`Self::QuestionInside`], with a `?` nested inside the call's argument too, and the
    /// inner call the one whose limit is `at - 1`: `capped(capped(i, limit=at - 1)?, limit=1000)?`.
    QuestionNestedInner,
    /// As [`Self::QuestionNestedInner`], with the outer call the one whose limit is `at - 1`.
    QuestionNestedOuter,
    /// `return` inside an `if`, inside a `match`.
    NestedReturn,
    /// `return` under `if i == at`.
    Return,
    /// `break` under `if i == at`.
    Break,
    /// `continue` under `if i == at`.
    Continue,
    /// Indexing `[1, 2]` out of range under `if i == at`, a crash.
    Crash,
    /// `got = capped(i, limit=at - 1) except why:`, printing `why` and returning: handled, so
    /// the cleanups after `lend` run.
    ExceptReturn,
    /// The same, with `continue`, which every round from `at` on takes.
    ExceptContinue,
    /// `got = capped(i, limit=at - 1) except Refused(): -1`, one case on the line, giving a value
    /// and carrying on.
    ExceptValue,
    /// `except why:` with one arm, `case _:`, that prints `why` and breaks.
    ExceptArms,
}

impl Leave {
    /// Whether leaving this way is a failure, which runs the cleanups in `lend x except:`, rather
    /// than the code after `lend`.
    pub(crate) const fn fails(self) -> bool {
        matches!(
            self,
            Self::ElseFail
                | Self::Question
                | Self::QuestionInside
                | Self::QuestionNestedInner
                | Self::QuestionNestedOuter
        )
    }
}

/// What holds a float in `FloatKeys`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum FloatHeld {
    /// `Gauge(at=value, n=1)`, a struct's `float` field.
    Gauge,
    /// `Gauge32(at=f32(value))`, a struct's `f32` field.
    Gauge32,
    /// `Level.At(value=value)`, an enum case's `float`, or `Level.Off`.
    Level,
    /// `Level32.At(value=f32(value))`, or `Level32.Off`.
    Level32,
    /// A `Gauge` held as `Marked`, a union with `str`, its set holding `"a"` too.
    Marked,
}

/// The floats `FloatKeys` picks from, each with the key it is: `0.0` and `-0.0` are one, as are
/// two NaNs made differently. The last, `Off`, is only for an enum.
pub(crate) const FLOATS: [(&str, u8); 8] =
    [("0.0", 0), ("-0.0", 0), ("nan()", 1), ("-nan()", 1), ("1.5", 2), ("-1.5", 3), ("2.0", 4), ("Off", 5)];

/// How a union's member is called on.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Via {
    /// After `match store:` with a case for each member, which narrows it as `is` does.
    Match,
    /// After `if store is Member:` for each member, `Lite`'s through `opened`, a function
    /// whose last line is a `with` that returns.
    Is,
    /// `run` straight on the union, with no match, and its `with` source after `is`.
    Direct,
    /// After `if store is Lite: return`, on what's left, `Pg | Mem`: `run` called on it, it
    /// given where a `Pg | Mem` goes, and a `match` telling the two apart. Three members only.
    Rest,
    /// `with store.session()` and `run` straight on the union, with no `is` or `match`: each
    /// runs the member's own.
    Plain,
}

/// The containers `CopiedNested` copies and changes deep inside.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Nested {
    /// `dict[str, Unit]`, a unit's `tags` appended to.
    DictOfUnits,
    /// `Army`, a struct holding a `dict[str, Unit]`.
    ArmyOfUnits,
    /// `dict[str, dict[str, int]]`, an inner number added to.
    DictOfDicts,
    /// `list[dict[str, int]]`.
    ListOfDicts,
    /// `dict[str, list[Unit]]`.
    DictOfUnitLists,
    /// `dict[str, Unit]`, a unit's `hp` added to.
    DictOfUnitsHp,
    /// `dict[str, list[int]]`, a list appended to.
    DictOfIntLists,
    /// `dict[str, Unit]`, a unit replaced by a new one.
    ReplacedInCopy,
}

/// What a local `ref` names: a field or an item of something held in a `mut` name, its index
/// worked out once, at the `ref`.
#[derive(Clone, Debug)]
pub(crate) enum RefPlace {
    /// `list[at]`.
    Item(String, Expr),
    /// `point.x` or `point.y`.
    Field(String, bool),
    /// `holder.items[at]`.
    HeldItem(String, Expr),
    /// `holder.items`, the whole list.
    HeldList(String),
    /// `holder.tag`.
    Tag(String),
    /// `nodes[at]`, a tree.
    NodeItem(String, Expr),
}

/// A change to a `Buffer[int]`. Each fails where a list's would, and `pop` on an empty one.
#[derive(Clone, Debug)]
pub(crate) enum BufOp {
    Push(Expr),
    /// `_ = buffer.pop()`.
    Pop,
    /// `buffer[at] = value`.
    Set(Expr, Expr),
    /// `buffer.insert(at, value)`, which fails past the end.
    Insert(Expr, Expr),
    /// `_ = buffer.remove(at)`.
    Remove(Expr),
    /// `buffer.truncate(count)`.
    Truncate(Expr),
    /// `buffer.extend(other)`, another buffer.
    Extend(String),
}

/// Where an assignment writes.
#[derive(Clone, Debug)]
pub(crate) enum Place {
    Name(String),
    /// `list[at]`.
    Item(String, Expr),
    /// `point.x` or `point.y`.
    Field(String, bool),
}

#[derive(Clone, Debug)]
pub(crate) enum Stmt {
    /// `name = value`, or `mut name = value`.
    Let {
        name: String,
        value: Expr,
        changeable: bool,
    },
    /// `place = value`, or `place += value` when `add` says so.
    Assign {
        place: Place,
        value: Expr,
        add: bool,
    },
    /// `list.append(value)`.
    Append(String, Expr),
    /// `_ = list.pop()`.
    Pop(String),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    /// `for name in range(count):`.
    Range(String, Expr, Vec<Stmt>),
    /// `for name in list:`, over a list nothing in the body changes.
    Each(String, Expr, Vec<Stmt>),
    /// `for ref name in list:`, the body adding to each item.
    EachRef(String, String, Expr),
    /// `for ref name in list:` with `name = name + at_or_zero(list, at)`, reading the list it's
    /// changing: at a fixed place, or where the item itself says when `at` is None.
    EachRefRead(String, String, Option<i64>),
    /// `for ref name in list:` with `name += 1`, then printing `list[at]`: the item changed
    /// through the `ref` has to read back changed by its place, in the same round, which #1202
    /// fixed.
    EachRefThenRead(String, String, i64),
    /// `lock name as held:` printing an `Outer`'s number.
    PrintOuter(String),
    /// A `Table[Point]` made with a point for each of `xs`, one of them taken out when `removed`
    /// says, then `for ref point in table:` adding `by` to each point's `x` and printing the `x` of
    /// the point at `read` by its `Id`, which has to read live, then the table.
    TableRound {
        table: String,
        ids: Vec<String>,
        xs: Vec<Expr>,
        removed: Option<usize>,
        read: usize,
        by: Expr,
    },
    /// `shared name = Outer(inner=bag, n=0)`.
    ShareOuter(String, String),
    /// `bump_outer(name, by, refuse=...)`: adds to its number under its lock, and a refusal
    /// undoes it, printing `refused`.
    OuterBump(String, Expr, Expr),
    /// `summed_until(limit, stop, stdout)`: sums a generator lending from a source until an item
    /// is `stop`, letting it go there. The source prints as it opens and closes, and its cleanup
    /// fails for an even `limit`, which fails the loop: that prints `shut`.
    Summed(Expr, Expr),
    /// An arrow function that fails, `() => capped(n, limit=limit)?`, its `?` passing its own
    /// failure up to what it's given to, as #1305 has it. `capped` fails with `n` when it's over
    /// `limit`.
    Arrowed(Arrow, Expr, i64),
    /// `guarded_shown(code, refuse, crash, noted)`: a `with` on `guarded`, a source whose `lend
    /// x else:` cleanup runs when the block fails or crashes. The cleanup fails for a `code`
    /// that's a multiple of 3, and, in `guarded_noted`, through `capped(code, limit=20)?` for one
    /// over 20: either becomes a note on the block's failure, and the rest of the cleanup is
    /// skipped. `crash` indexes `[1, 2]` by `code`, which crashes unless it's -2 to 1.
    Guarded {
        code: i64,
        refuse: bool,
        crash: bool,
        noted: bool,
    },
    /// `guard_all(items)`: a launched task runs a `with guarded(item)` for each item it's sent,
    /// crashing on 5 and failing on 7, each running the cleanup. Either prints `stopped`.
    GuardAll(Vec<i64>),
    /// `left{id}(stdout)`: a loop of `rounds` rounds, each opening `depth` `ward`s, in nested
    /// `with` blocks or `together` in one, whose cleanups print. Round `at` leaves them all at
    /// once by `how`; an `at` of `rounds` never does. A failure is printed as `refused`.
    Left {
        id: usize,
        depth: usize,
        together: bool,
        rounds: i64,
        at: i64,
        how: Leave,
    },
    /// `turned{id}(stdout)`: a loop of `rounds` turns whose int is at least 0 on the first turn
    /// and changes sign later, by `how`. Each turn prints it, `XS[it]`, it times 3, and it added
    /// to each end of `int`, so a proof that it's never negative, made from the first turn alone,
    /// shows: from -4 to 3 all of those hold, and past them the program stops.
    Turned(Turning),
    /// `streamed{id}(stdout)`: a generator over `parallel(xs)`, read in one of the ways a stream is.
    Streamed(Streaming),
    /// `aliased{id}(stdout)`: copies of collections and their parts, each changed in place, then
    /// all printed.
    Aliased(Aliasing),
    /// `constants{id}(stdout)`: module constants read on the main thread, then used and changed
    /// in copies there, in a launched task and in `parallel`'s bodies.
    Constants(Constants),
    /// `fielded{id}(stdout)`: `+=` on a struct's fields with calls on the right.
    FieldCalls(FieldCalls),
    /// `yielded{id}(stdout)`: generators yielding structs, tuples and enums holding texts, lists
    /// and dicts, and an inbox loop sending them back.
    Yielded(Yielding),
    /// `woken{id}(stdout)`: a task that crashes or fails while its launcher waits on a channel.
    Woken(Waking),
    /// Keys holding a float or an `f32`, as #1306 has them: a dict with a key for each of
    /// `values`, places in `FLOATS`, a set of them, a lookup of `values[probe]`, `in` with
    /// `FLOATS[asked]`, and `==` and `!=` on `pair`, two places in `values`.
    FloatKeys {
        held: FloatHeld,
        names: (String, String),
        values: Vec<usize>,
        probe: usize,
        asked: usize,
        pair: (usize, usize),
    },
    /// `while counter < limit:`, counting up each round so it ends.
    While(String, i64, Vec<Stmt>),
    /// `if condition: break`.
    Break(Expr),
    /// `if condition: continue`.
    Continue(Expr),
    /// `point.moved(by)`.
    Moved(String, Expr),
    /// `match shape:` adding `by` to whichever number its case holds, through `ref`.
    Grow(String, Expr),
    Print(Vec<(Expr, Ty)>),
    /// `shared name = Point(x=..., y=...)`.
    Share(String, Expr, Expr),
    /// `change(name, by, refuse)`, which under `lock name as ref` adds `by` to `x` and doubles
    /// `y`, then fails if `refuse`, which undoes both. A failure prints `refused`.
    Change(String, Expr, Expr),
    /// `lock name as held:` printing it.
    PrintShared(String),
    /// `shared name = Bag(items=list, counts={...})`.
    ShareBag(String, Expr, Vec<(&'static str, i64)>),
    /// `locked<id>(name, refuse=...)`: a function of its own takes the bag's lock, makes each
    /// change, then fails if asked to, which undoes them all. A failure prints `refused`.
    Locked {
        id: usize,
        name: String,
        ops: Vec<Op>,
        refuse: Expr,
    },
    /// `sent<id>(name, items=list)`: a launched task makes each change for each number it's sent,
    /// under the lock each time. One that crashes undoes that round's changes and prints
    /// `crashed`.
    Sent {
        id: usize,
        name: String,
        ops: Vec<Op>,
        items: Expr,
    },
    /// `tally(name, items=list)`: a launched task takes each item from its inbox and, under the
    /// lock, counts it in `y` and adds it to `x`. A task that overflows crashes, which undoes
    /// that lock's changes and prints `crashed`.
    Tally(String, Expr),
    /// `for name in range(len(list)):` reading `list[name]` first each round, with the body free
    /// to change the list, so a bound worked out once can stop holding.
    Indexed(String, String, Vec<Stmt>),
    /// `counts[key] = value`, or `+=` with `add`, which fails for a key that isn't there.
    SetKey(String, &'static str, Expr, bool),
    /// `_ = counts.remove(key)`.
    RemoveKey(String, &'static str),
    /// A change to a `Buffer[int]`.
    Buffered(String, BufOp),
    /// A change to a `list[Node]`: an append, a pop, or an item set where it's kept in the list.
    NodesAppend(String, Expr),
    NodesPop(String),
    /// `if len(nodes) > 0: nodes[at % len(nodes)] = node`.
    NodesSet(String, Expr, Expr),
    /// `match node:` changing whichever case it is through `ref`: a branch's label grows by `text`
    /// and its children by `child`, a tagged node's tags and inner nodes likewise, a leaf's name
    /// grows and a number counts up.
    Reshape(String, Expr, Expr),
    /// `match named:` changing it through `ref`: a pair's label grows by `text`, its tags gain
    /// the new label, and its left side becomes `left`; a leaf's name grows by `text`.
    Rename(String, Expr, Expr),
    /// `holder.items.append(holder.tag)`, `own = holder.items`, then `holder.grown_by(own)`: a
    /// `ref self` method given a value read from the list it grows, just after that list grew in
    /// place, and both printed, under `if len(holder.items) < 64:`.
    GrownBy(String, String),
    /// `if len(list) < 64:` then `own = list`, `appended_all(ref list, own)` and both printed:
    /// a list lent to a call through `ref` beside a value read from it.
    LentRef(String, String),
    /// `mut rows = [items, [7]]`, then under `if len(rows[0]) < 64:`, `own = rows[0]`,
    /// `appended_all(ref rows[0], own)` and the rows and `own` printed: a list's item lent.
    LentItem(String, String, Expr),
    /// `holder.items = holder.items` and `holder.tag = holder.tag`, which change nothing.
    SelfField(String),
    /// `shadowed<id>(stdout, by=value)`, a function whose locals are named `change`,
    /// `bump_outer` and `tally`, like functions that take a `shared` value and may go uncalled.
    Shadowed(usize, Expr),
    /// Collect, then apply: `mut world = [Spot(...), ...]`, then `changes = [plan(item, world)
    /// for item in parallel(xs)]`, a `Change` for each item worked out on the threads from the
    /// world as it is, then a plain loop applying them in order through a local `ref`, and the
    /// world printed.
    PlanApply(String, Vec<(&'static str, Expr)>, Expr, String),
    /// `stdout.print(<how><members>(<member>, text))`: a union of `members` structs, two or
    /// three, each with a `run` method and a `session` `with` source of its own, given the
    /// member numbered `member`, which must be the one whose `session` and `run` are called.
    UnionCall(Via, usize, usize, Expr),
    /// `given = <a container holding containers>`, `mut copy = given`, a change deep inside the
    /// copy by `value`, then both printed: the original mustn't see it, and must still be whole.
    CopiedNested(Nested, String, String, Expr),
    /// `ref name = place`, then the statements that use it, the last of them its last use.
    LocalRef(String, RefPlace, Vec<Stmt>),
    /// `holder.items.append(value)`.
    HolderAppend(String, Expr),
    /// `holder.tag += value`.
    HolderTag(String, Expr),
    /// `lists[key] = list`, storing a list's value.
    PutList(String, &'static str, Expr),
    /// `if key in lists: lists[key].append(value)`.
    ListAppend(String, &'static str, Expr),
    /// `grow_all(ref list, by)`, adding `by` to each item through a `ref` parameter.
    GrowAll(String, Expr),
    /// `for name in counted(limit):`, over a generator.
    EachCounted(String, Expr, Vec<Stmt>),
    /// `with tallied(start, stdout) as name:`, which prints when it opens, when it closes and
    /// when the block crashes.
    With(String, Expr, Vec<Stmt>),
}

/// A whole program: `main`'s body.
#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) body: Vec<Stmt>,
    /// Whether nothing in it launches a task or runs `parallel`, so its `Named` values can be
    /// native blocks.
    pub(crate) threadless: bool,
}
