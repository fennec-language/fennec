//! A realistic Fennec program of any size, the same each time from a seed: an app kit that
//! depends on a kit of generic helpers. The app is in zones of 20 modules each, every module a
//! few units of five kinds, with a test each:
//!
//! - a stock: a struct with methods, an enum with payloads matched on, lists, dicts and f-strings,
//!   and the helper kit's generic box, pair and `heaviest`;
//! - a ledger: a `shared` struct that a task, `launch`ed, adds to under its `lock`;
//! - words: a constant list, a dict counted into, and `max` of a generator;
//! - a grid: an enum scored a row at a time with `parallel`;
//! - a shelf: a generic struct and a generic function, used with three types.
//!
//! Every module also has a small function that a later module, picked at random, imports and
//! calls, so the imports form a graph across zones, as a real program's do, not just a tree.
//! Each zone has an index that adds up its modules' runs, and `main` adds up every zone's.
//!
//! The text is as `fennec fmt` writes it and `fennec lint` passes it, so what's measured is the
//! work a real program of that size takes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// How many modules each zone has.
const PARTS_PER_ZONE: usize = 20;

/// The kit of generic helpers the app depends on, which it never changes.
const HELPERS: &str = "\
## Small generic holders the app's zones share.

export struct Box[T]:
    value: T

    def get(self) -> T:
        return self.value

export struct Pair[A]:
    left: A
    right: A

    def swapped(self) -> Pair[A]:
        return Pair(left=self.right, right=self.left)

export interface Weighed:
    def weight(self) -> float

export def heaviest[T: Weighed](items: list[T]) -> float:
    mut most = 0.0
    for item in items:
        if item.weight() > most:
            most = item.weight()
    return most

export def tagged(prefix: str, at: int) -> str:
    return f\"{prefix}-{at}\"
";

/// A unit's kind, with its text, in which `{z}`, `{p}` and `{u}` are its zone, module and place
/// in the module, `{s}` names its functions (`{z}_{p}_{u}`), `{S}` its types (`Z{z}P{p}U{u}`) and
/// `{k}` is its `scale`, which the one-line edit changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Stock,
    Ledger,
    Words,
    Grid,
    Shelf,
}

impl Kind {
    /// Picked by weight: stocks and shelves most, as plain data and generic code are most of a
    /// real program, and ledgers least, as tasks are fewest.
    fn picked(rng: &mut Rng) -> Kind {
        match rng.below(20) {
            0..5 => Kind::Stock,
            5..8 => Kind::Ledger,
            8..12 => Kind::Words,
            12..15 => Kind::Grid,
            _ => Kind::Shelf,
        }
    }

    /// Whether its run can fail, with `Crashed`, as a task's can.
    fn fails(self) -> bool {
        self == Kind::Ledger
    }

    /// What it uses from the helper kit.
    fn helpers(self) -> &'static [&'static str] {
        match self {
            Kind::Stock => &["Box", "Pair", "heaviest", "tagged"],
            Kind::Grid => &["Pair"],
            Kind::Shelf => &["Box"],
            Kind::Ledger | Kind::Words => &[],
        }
    }

    fn text(self) -> &'static str {
        match self {
            Kind::Stock => STOCK,
            Kind::Ledger => LEDGER,
            Kind::Words => WORDS,
            Kind::Grid => GRID,
            Kind::Shelf => SHELF,
        }
    }
}

const STOCK: &str = "\
# A stock of items, what state each is in, and what that costs.

struct Item{S}:
    id: int
    name: str
    mass: float
    tags: list[str]

    def weight(self) -> float:
        return self.mass

    def label(self) -> str:
        return f\"{self.name}#{self.id}\"

enum State{S}:
    Idle
    Moving(speed: float)
    Broken(why: str)

    def cost(self) -> float:
        match self:
            case .Idle:
                return 0.0
            case .Moving(speed):
                return speed * 2.0
            case .Broken(_):
                return 10.0

def items_{s}(count: int) -> list[Item{S}]:
    scale = {k}
    mut made = list[Item{S}]()
    for i in range(count):
        mass = float(i % scale) + 0.5
        made.append(Item{S}(id=i, name=f\"item{i}\", mass=mass, tags=[tagged(\"z{z}\", i)]))
    return made

def states_{s}(count: int) -> list[State{S}]:
    mut made = list[State{S}]()
    for i in range(count):
        if i % 3 == 0:
            made.append(.Idle)
        elif i % 3 == 1:
            made.append(.Moving(float(i)))
        else:
            made.append(.Broken(f\"part {i}\"))
    return made

def by_tag_{s}(stock: list[Item{S}]) -> dict[str, int]:
    mut counted = dict[str, int]()
    for item in stock:
        for tag in item.tags:
            counted[tag] = (counted.get(tag) else 0) + 1
    return counted

def run_{s}(n: int) -> int:
    stock = items_{s}(n)
    boxed = Box(value=len(stock))
    pair = Pair(left=1, right=n).swapped()
    cost = sum(state.cost() for state in states_{s}(n))
    tags = len(by_tag_{s}(stock))
    return boxed.get() + pair.left + tags + int(cost) + int(heaviest(stock))

test \"zone {z} part {p} unit {u}: a stock adds up\":
    assert run_{s}(3) > 0
    assert len(items_{s}(4)) == 4
";

const LEDGER: &str = "\
# A ledger that a task adds to, under its lock.

struct Ledger{S}:
    entries: list[int]
    total: int

    def add(ref self, amount: int):
        self.entries.append(amount)
        self.total += amount

def amounts_{s}(n: int) -> list[int]:
    scale = {k}
    return [i * scale % 97 for i in range(n)]

def credit_{s}(ledger: shared Ledger{S}, amounts: list[int]):
    for amount in amounts:
        lock ledger as ref held:
            held.add(amount)

def run_{s}(n: int) -> int fails Crashed:
    shared ledger = Ledger{S}(entries=list[int](), total=0)
    amounts = amounts_{s}(n)
    with launch(() => credit_{s}(ledger, amounts))?:
        pass
    lock ledger as held:
        return held.total + len(held.entries)

test \"zone {z} part {p} unit {u}: a ledger adds what it's given\":
    mut ledger = Ledger{S}(entries=list[int](), total=0)
    ledger.add(5)
    ledger.add(7)
    assert ledger.total == 12
    assert len(amounts_{s}(4)) == 4
";

const WORDS: &str = "\
# Words counted, and the longest kept.

WORDS{S} = [\"fox\", \"den\", \"burrow\", \"tail\", \"night\", \"field\", \"z{z}\", \"p{p}\"]

def words_{s}(n: int) -> list[str]:
    scale = {k}
    return [WORDS{S}[(i * scale) % len(WORDS{S})] for i in range(n)]

def counted_{s}(words: list[str]) -> dict[str, int]:
    mut counts = dict[str, int]()
    for word in words:
        counts[word] = (counts.get(word) else 0) + 1
    return counts

def longest_{s}(words: list[str]) -> int:
    return max(len(word) for word in words) else 0

def run_{s}(n: int) -> int:
    words = words_{s}(n)
    counts = counted_{s}(words)
    return len(counts) + longest_{s}(words) + len(f\"{counts}\")

test \"zone {z} part {p} unit {u}: words are counted\":
    counts = counted_{s}([\"a\", \"b\", \"a\"])
    assert counts[\"a\"] == 2
    assert longest_{s}([\"ab\", \"abc\"]) == 3
";

const GRID: &str = "\
# A grid of cells, scored a row at a time on every core.

enum Cell{S}:
    Empty
    Wall
    Score(points: int)

    def points(self) -> int:
        match self:
            case .Score(points):
                return points
            case _:
                return 0

def cell_{s}(at: int) -> Cell{S}:
    scale = {k}
    if at % scale == 0:
        return .Wall
    elif at % 3 == 0:
        return .Score(at % 11)
    return .Empty

def score_{s}(row: int) -> int:
    return sum(cell_{s}(row * 16 + col).points() for col in range(16))

def run_{s}(n: int) -> int:
    rows = [score_{s}(row) for row in parallel(range(n))]
    corner = Pair(left=n, right=len(rows)).swapped()
    return sum(rows) + corner.left

test \"zone {z} part {p} unit {u}: a grid scores its cells\":
    assert cell_{s}(0) == .Wall
    assert score_{s}(1) >= 0
";

const SHELF: &str = "\
# A shelf of anything, generic in what it holds, and a job that fills it.

struct Shelf{S}[T]:
    held: list[T]

    def count(self) -> int:
        return len(self.held)

def filled_{s}[T](item: T, times: int) -> Shelf{S}[T]:
    scale = {k}
    mut held = list[T]()
    for _ in range(times % scale + 1):
        held.append(item)
    return Shelf{S}(held=held)

def run_{s}(n: int) -> int:
    numbers = filled_{s}(n, n)
    names = filled_{s}(f\"shelf {n}\", n)
    marks = filled_{s}(Box(value=n), n)
    return numbers.count() + names.count() + marks.count() + marks.held[0].get()

test \"zone {z} part {p} unit {u}: a shelf holds what it's given\":
    assert filled_{s}(1, 2).count() == 3
    assert filled_{s}(\"a\", 0).count() == 1
";

/// What was generated: where, how big, and the one line the edit changes.
pub(crate) struct Generated {
    /// The app kit's folder, which is what's checked, built and tested.
    pub(crate) app: PathBuf,
    pub(crate) lines: usize,
    pub(crate) files: usize,
    pub(crate) tests: usize,
    /// The module the edit changes, near the middle of the program.
    pub(crate) edited: PathBuf,
    /// Its first unit's `scale`, as generated, which is the first in the module.
    scale: usize,
}

impl Generated {
    /// The edited module's text with its `scale` line changed, `at` up from what was generated,
    /// so each edit differs from the last, and every test still passes.
    pub(crate) fn edit(&self, at: usize) -> Result<(), String> {
        let shown = self.edited.display();
        let text = std::fs::read_to_string(&self.edited).map_err(|err| format!("{shown}: {err}"))?;
        let from = format!("    scale = {}\n", self.scale);
        let changed = text.replacen(&from, &format!("    scale = {}\n", self.scale + at), 1);
        if changed == text {
            return Err(format!("`{}` isn't in {shown}", from.trim()));
        }
        std::fs::write(&self.edited, changed).map_err(|err| format!("{shown}: {err}"))
    }
}

/// A seeded pseudo-random sequence, SplitMix64, so the same seed makes the same program on every
/// machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A number from 0 up to, not including, `count`.
    fn below(&mut self, count: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(count).unwrap_or(u64::MAX)).unwrap_or(0)
    }
}

/// One module: its place, its units' kinds and scales, and the earlier module whose `reach` it
/// calls.
struct Part {
    zone: usize,
    part: usize,
    units: Vec<(Kind, usize)>,
    reaches: (usize, usize),
}

impl Part {
    fn fails(&self) -> bool {
        self.units.iter().any(|(kind, _)| kind.fails())
    }
}

/// Writes a program of about `lines` lines into `root`, as the kits `helpers` and `app`, which
/// must not be there yet.
pub(crate) fn generate(root: &Path, lines: usize, seed: u64) -> Result<Generated, String> {
    let mut rng = Rng(seed);
    let mut parts: Vec<Part> = Vec::new();
    let mut written = 0;
    // About 190 lines a module; the last zone is filled out whole, so every zone is alike.
    while written < lines || parts.len() % PARTS_PER_ZONE != 0 {
        let at = parts.len();
        let (zone, part) = (at / PARTS_PER_ZONE, at % PARTS_PER_ZONE);
        let units = (0..3 + rng.below(4)).map(|_| (Kind::picked(&mut rng), 5 + rng.below(9))).collect();
        let reached = if at == 0 { 0 } else { rng.below(at) };
        let made = Part { zone, part, units, reaches: (reached / PARTS_PER_ZONE, reached % PARTS_PER_ZONE) };
        written += module(&made, seed).lines().count();
        parts.push(made);
    }
    let helpers = root.join("helpers");
    let app = root.join("app");
    write(&helpers.join("kit.toml"), "name = \"helpers\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n")?;
    write(&helpers.join("src/boxes.fen"), HELPERS)?;
    write(
        &app.join("kit.toml"),
        "name = \"app\"\nversion = \"0.1.0\"\nfennec = \"0.0.1\"\n\n[dependencies]\nhelpers = { path = \"../helpers\" }\n",
    )?;
    let mut lines_written = HELPERS.lines().count();
    let mut tests = 0;
    for made in &parts {
        let text = module(made, seed);
        lines_written += text.lines().count();
        tests += made.units.len();
        write(&app.join(format!("src/zone{}/part{}.fen", made.zone, made.part)), &text)?;
    }
    let zones: Vec<&[Part]> = parts.chunks(PARTS_PER_ZONE).collect();
    for (zone, zone_parts) in zones.iter().enumerate() {
        let text = index(zone, zone_parts);
        lines_written += text.lines().count();
        write(&app.join(format!("src/zone{zone}/index.fen")), &text)?;
    }
    let text = main_module(&zones, seed);
    lines_written += text.lines().count();
    write(&app.join("src/main.fen"), &text)?;
    // The edit is in the middle zone's middle module, in its first unit's scale.
    let middle = zones.get(zones.len() / 2).and_then(|zone| zone.get(PARTS_PER_ZONE / 2));
    let Some((middle, (_, scale))) = middle.and_then(|middle| Some((middle, middle.units.first()?))) else {
        return Err("the program has no module in its middle to edit".to_owned());
    };
    let edited = app.join(format!("src/zone{}/part{}.fen", middle.zone, middle.part));
    Ok(Generated {
        app,
        lines: lines_written,
        files: parts.len() + zones.len() + 2,
        tests,
        edited,
        scale: *scale,
    })
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    }
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

/// One module's text: its imports, its units, its `reach`, and its run adding them up.
fn module(made: &Part, seed: u64) -> String {
    let (zone, part) = (made.zone, made.part);
    let mut text = format!("## Zone {zone}, part {part}, generated by `fennec-bench big-codebase` from seed {seed}.\n\n");
    if made.fails() {
        text.push_str("from std.concurrent import Crashed\n\n");
    }
    let mut helpers: Vec<&str> = Vec::new();
    for (kind, _) in &made.units {
        for name in kind.helpers() {
            if !helpers.contains(name) {
                helpers.push(name);
            }
        }
    }
    // In the order `fennec fmt` puts them, which is the order of their text.
    helpers.sort_unstable();
    if !helpers.is_empty() {
        let _ = writeln!(text, "from helpers.boxes import {}", helpers.join(", "));
    }
    let reaches = made.reaches != (zone, part);
    if reaches {
        let (other_zone, other_part) = made.reaches;
        let _ = writeln!(text, "from zone{other_zone}.part{other_part} import reach_{other_zone}_{other_part}");
    }
    for (place, (kind, scale)) in made.units.iter().enumerate() {
        let unit = kind
            .text()
            .replace("{s}", &format!("{zone}_{part}_{place}"))
            .replace("{S}", &format!("Z{zone}P{part}U{place}"))
            .replace("{z}", &zone.to_string())
            .replace("{p}", &part.to_string())
            .replace("{u}", &place.to_string())
            .replace("{k}", &scale.to_string());
        if !text.ends_with("\n\n") {
            text.push('\n');
        }
        text.push_str(&unit);
    }
    let reach_scale = 3 + (zone + part) % 7;
    let _ = write!(
        text,
        "\n# How far this part reaches, which later parts ask of it.\n\n\
         def reach_{zone}_{part}(n: int) -> int:\n    return n % {reach_scale} + 1\n\n\
         # All of this part, added up.\n\n"
    );
    let fails = if made.fails() { " fails Crashed" } else { "" };
    let (reached_zone, reached_part) = made.reaches;
    let _ = writeln!(text, "def run_{zone}_{part}(n: int) -> int{fails}:");
    let _ = writeln!(text, "    mut total = reach_{reached_zone}_{reached_part}(n)");
    for (place, (kind, _)) in made.units.iter().enumerate() {
        let tried = if kind.fails() { "?" } else { "" };
        let _ = writeln!(text, "    total += run_{zone}_{part}_{place}(n){tried}");
    }
    text.push_str("    return total\n");
    text
}

/// A zone's index: every module's run, added up.
fn index(zone: usize, parts: &[Part]) -> String {
    let fails = parts.iter().any(Part::fails);
    let mut text = format!("## Zone {zone}: each part's run, added up.\n\n");
    if fails {
        text.push_str("from std.concurrent import Crashed\n\n");
    }
    for part in sorted_numbers(parts.len()) {
        let _ = writeln!(text, "from zone{zone}.part{part} import run_{zone}_{part}");
    }
    let _ = writeln!(text, "\ndef run_zone_{zone}(n: int) -> int{}:", if fails { " fails Crashed" } else { "" });
    text.push_str("    mut total = 0\n");
    for made in parts {
        let tried = if made.fails() { "?" } else { "" };
        let _ = writeln!(text, "    total += run_{zone}_{}(n){tried}", made.part);
    }
    text.push_str("    return total\n");
    text
}

/// The numbers up to `count`, in the order `fennec fmt` sorts imports of the modules they name:
/// as text, so 10 comes before 2.
fn sorted_numbers(count: usize) -> Vec<usize> {
    let mut numbers: Vec<usize> = (0..count).collect();
    numbers.sort_by_key(ToString::to_string);
    numbers
}

/// `main`: every zone's run, added up.
fn main_module(zones: &[&[Part]], seed: u64) -> String {
    let mut text = format!(
        "## The whole app, every zone's run added up, generated by `fennec-bench big-codebase`\n\
         ## from seed {seed}.\n\n\
         from std.concurrent import Crashed\n\n"
    );
    for zone in sorted_numbers(zones.len()) {
        let _ = writeln!(text, "from zone{zone}.index import run_zone_{zone}");
    }
    text.push_str("\ndef main(stdout: Stdout) fails Crashed:\n    mut total = 0\n");
    for (zone, zone_parts) in zones.iter().enumerate() {
        let fails = zone_parts.iter().any(Part::fails);
        let _ = writeln!(text, "    total += run_zone_{zone}(5){}", if fails { "?" } else { "" });
    }
    text.push_str("    stdout.print(total)\n");
    text
}

#[cfg(test)]
mod tests {
    use super::generate;

    fn every_file(folder: &std::path::Path, found: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(folder).unwrap().map(|entry| entry.unwrap().path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                every_file(&path, found);
            } else {
                found.push((path.display().to_string(), std::fs::read(&path).unwrap()));
            }
        }
    }

    #[test]
    fn a_seed_makes_the_same_program_each_time_and_another_seed_another() {
        let folder = |name: &str| fennec_temp::Temp::folder(name).unwrap();
        let (first, second, third) = (folder("big-first"), folder("big-second"), folder("big-third"));
        let made = generate(first.path(), 5000, 7).unwrap();
        generate(second.path(), 5000, 7).unwrap();
        generate(third.path(), 5000, 8).unwrap();
        let read = |root: &std::path::Path| {
            let mut found = Vec::new();
            every_file(root, &mut found);
            found
                .into_iter()
                .map(|(path, bytes)| (path.replacen(&root.display().to_string(), "", 1), bytes))
                .collect::<Vec<_>>()
        };
        assert!(read(first.path()) == read(second.path()));
        assert!(read(first.path()) != read(third.path()));
        assert!(made.lines >= 5000 && made.lines < 5000 * 2, "{} lines", made.lines);
        assert!(made.tests > 0);
    }

    #[test]
    fn the_edit_changes_one_scale_and_each_edit_differs() {
        let root = fennec_temp::Temp::folder("big-edit").unwrap();
        let made = generate(root.path(), 5000, 1).unwrap();
        let before = std::fs::read_to_string(&made.edited).unwrap();
        made.edit(1).unwrap();
        let once = std::fs::read_to_string(&made.edited).unwrap();
        assert_ne!(before, once);
        let changed: Vec<_> = before.lines().zip(once.lines()).filter(|(was, now)| was != now).collect();
        assert_eq!(changed.len(), 1, "{changed:?}");
        std::fs::write(&made.edited, &before).unwrap();
        made.edit(2).unwrap();
        assert_ne!(std::fs::read_to_string(&made.edited).unwrap(), once);
    }
}
