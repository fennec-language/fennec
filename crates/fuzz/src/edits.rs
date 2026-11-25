//! The `edits` target: a program is built, edited a piece at a time, and built again after each
//! edit, reusing what the build before it kept. What's reused has to make just what a build that
//! keeps nothing makes, so a stale reuse, which would be silent, is a failure here.
//!
//! - The loader keeps each file's parse, and its rewriting of names across files, for the next
//!   load (`fennec_kit::load_keeping`), with its own comparison forced on. Each load has to be
//!   what loading it all again gives, or fail as that does.
//! - The checker keeps its check (`check_to_build_keeping`) and carries it over into the next,
//!   with its own comparison forced on. What it finds, a check or what's wrong, has to be what a
//!   whole check of the same text finds.
//! - LLVM keeps each function's object in the edit cache. A build reusing it, with
//!   `FENNEC_CACHE_AUDIT` writing each reused unit again to check it, has to make the same
//!   executable and the same objects, byte for byte, as a build with `FENNEC_NO_CACHE`.
//! - LLVM keeps each function's analysis from one build to the next, as the daemon does. Each
//!   version is lowered here and its analyses worked out using again what the version before
//!   kept, every one used again checked against analyzing it again.
//!
//! The program is made of pieces, each with a first way of writing it and others an edit switches
//! to: a body, a signature a test calls, a bound, a callee changing what its caller loops over,
//! directly or two calls down, a constant and a field default from a function whose type changes,
//! a `with` source's body, a function waiting inside a lock, a field's type, a function added,
//! removed or renamed, a field added, and a number that turns negative away from where it's used
//! as an index. Others are about files beside the program's: an import renamed with `as` or
//! written as the module's name, the module an enum and a function reached by a leading dot are
//! imported from swapped, a function and an import moved to another file, and a file growing past
//! its 256 KiB span slot, which moves the files after it. Then lines put in above declarations,
//! and a parameter's and a field's default read by a function written before them. Last, a callee
//! that stops keeping what it's handed, or changing it, while its callers stay as written. The input
//! picks 1 to 4 edits in a row, each switching one or two pieces and sometimes moving one, so
//! later declarations move, and whether each version's types are laid out boxed. An edit may
//! leave the program failing to check, which is compared too.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use fennec_check::{KeptCheck, check_to_build_keeping, check_to_build_saying};

/// A piece of the program: its first way of writing it, then the others an edit may switch to.
/// What a way writes goes in `program.fen` until a line `# file: <name>` sends what follows to that
/// file beside it, its imports at the top of the file it's in. A line `# pad: <bytes>` is that many
/// bytes of comment.
struct Piece {
    variants: &'static [&'static str],
}

/// The file the program is built from. The others are beside it.
const PROGRAM: &str = "program.fen";

/// What starts a line sending what follows to another file.
const FILE_MARK: &str = "# file: ";

/// What starts a line standing for a comment of that many bytes.
const PAD_MARK: &str = "# pad: ";

/// Every piece. `main` calls into each.
const PIECES: [Piece; 20] = [
    // A body.
    Piece {
        variants: &[
            "def double(n: int) -> int:\n    return n * 2\n",
            "def double(n: int) -> int:\n    return n * 2 + 1\n",
            "def double(n: int) -> int:\n    if n < 0:\n        return 0\n    return n - 1\n",
        ],
    },
    // A signature, with a test calling it.
    Piece {
        variants: &[
            "def label(n: int) -> int:\n    return n + 1\n\ntest \"label\":\n    total: int = label(2)\n    assert total == 3\n",
            "def label(n: int) -> str:\n    return f\"l{n}\"\n\ntest \"label\":\n    total: int = label(2)\n    assert total == 3\n",
            "def label(n: int) -> int:\n    return n + 2\n\ntest \"label\":\n    total: int = label(2)\n    assert total == 4\n",
        ],
    },
    // A bound its caller's argument doesn't fit.
    Piece {
        variants: &[
            "interface Shape:\n    def area(self) -> float\n\nstruct Dot:\n    x: int\n\ndef measure[T](item: T) -> int:\n    return 1\n",
            "interface Shape:\n    def area(self) -> float\n\nstruct Dot:\n    x: int\n\ndef measure[T: Shape](item: T) -> int:\n    return 1\n",
            "interface Shape:\n    def area(self) -> float\n\nstruct Dot:\n    x: int\n\ndef measure[T](item: T) -> int:\n    return 2\n",
        ],
    },
    // A callee changing what its caller loops over, directly or two calls down.
    Piece {
        variants: &[
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        bump(ref bag, item)\n\ndef bump(ref bag: Bag, by: int):\n    bag.total += by\n",
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        bump(ref bag, item)\n\ndef bump(ref bag: Bag, by: int):\n    bag.items.append(by)\n",
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        middle(ref bag, item)\n\ndef middle(ref bag: Bag, by: int):\n    bump(ref bag, by)\n\ndef bump(ref bag: Bag, by: int):\n    bag.total += by\n",
            "struct Bag:\n    items = list[int]()\n    total = 0\n\ndef run(ref bag: Bag):\n    for item in bag.items:\n        middle(ref bag, item)\n\ndef middle(ref bag: Bag, by: int):\n    bump(ref bag, by)\n\ndef bump(ref bag: Bag, by: int):\n    bag.items.append(by)\n",
        ],
    },
    // A constant from a function whose type changes.
    Piece {
        variants: &[
            "LIMIT = pick()\n\ndef over(n: int) -> bool:\n    return n > LIMIT\n\ndef under(n: int) -> bool:\n    return n < LIMIT\n\ndef pick() -> int:\n    return 2\n",
            "LIMIT = pick()\n\ndef over(n: int) -> bool:\n    return n > LIMIT\n\ndef under(n: int) -> bool:\n    return n < LIMIT\n\ndef pick() -> str:\n    return \"2\"\n",
            "LIMIT = pick()\n\ndef over(n: int) -> bool:\n    return n > LIMIT\n\ndef under(n: int) -> bool:\n    return n < LIMIT\n\ndef pick() -> int:\n    return 5\n",
        ],
    },
    // A field typed by its default, from a function whose type changes.
    Piece {
        variants: &[
            "struct Limit:\n    most = pick_most()\n\ndef pick_most() -> int:\n    return 2\n",
            "struct Limit:\n    most = pick_most()\n\ndef pick_most() -> str:\n    return \"2\"\n",
            "struct Limit:\n    most = pick_most()\n\ndef pick_most() -> int:\n    return -1\n",
        ],
    },
    // A `with` source's body.
    Piece {
        variants: &[
            "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"open\")\n    lend 1\n    stdout.print(\"close\")\n",
            "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"opens\")\n    lend 1\n    stdout.print(\"close\")\n",
            "def opened(stdout: Stdout) -> with int:\n    stdout.print(\"open\")\n    lend 2\n",
        ],
    },
    // A function that starts waiting, called inside a lock.
    Piece {
        variants: &[
            "struct Scores:\n    total = 0\n\ndef rest(clock: Clock):\n    _ = clock.now()\n",
            "struct Scores:\n    total = 0\n\ndef rest(clock: Clock):\n    clock.sleep(1ms)\n",
            "struct Scores:\n    total = 0\n\ndef rest(clock: Clock):\n    _ = clock.now()\n    _ = clock.now()\n",
        ],
    },
    // A field's type.
    Piece {
        variants: &[
            "struct Point:\n    x: int\n\ndef made_point() -> Point:\n    return Point(x=2)\n",
            "struct Point:\n    x: float\n\ndef made_point() -> Point:\n    return Point(x=2.5)\n",
            "struct Point:\n    x: int\n\ndef made_point() -> Point:\n    return Point(x=2.5)\n",
        ],
    },
    // A function added, removed or renamed.
    Piece {
        variants: &[
            "def spare() -> int:\n    return 1\n",
            "",
            "def spare_renamed() -> int:\n    return 1\n",
            "def spare() -> int:\n    return 1\n\ndef another_spare() -> int:\n    return 2\n",
        ],
    },
    // A field added after another or before it, which changes the struct's layout.
    Piece {
        variants: &[
            "struct Rec:\n    a: int\n\ndef made_rec() -> Rec:\n    return Rec(a=1)\n\ndef rec_a(rec: Rec) -> int:\n    return rec.a\n",
            "struct Rec:\n    a: int\n    b = 0\n\ndef made_rec() -> Rec:\n    return Rec(a=1)\n\ndef rec_a(rec: Rec) -> int:\n    return rec.a\n",
            "struct Rec:\n    z: int\n    a: int\n\ndef made_rec() -> Rec:\n    return Rec(z=7, a=1)\n\ndef rec_a(rec: Rec) -> int:\n    return rec.a + rec.z\n",
        ],
    },
    // A number that turns negative in one function, used as an index in another.
    Piece {
        variants: &[
            "struct Slot:\n    at = 0\n\ndef set_at(ref slot: Slot):\n    slot.at = 1\n\ndef read_slot(items: list[int]) -> int:\n    mut slot = Slot()\n    set_at(ref slot)\n    return items[slot.at]\n",
            "struct Slot:\n    at = 0\n\ndef set_at(ref slot: Slot):\n    slot.at = -1\n\ndef read_slot(items: list[int]) -> int:\n    mut slot = Slot()\n    set_at(ref slot)\n    return items[slot.at]\n",
            "struct Slot:\n    at = 0\n\ndef set_at(ref slot: Slot):\n    slot.at = 2\n\ndef read_slot(items: list[int]) -> int:\n    mut slot = Slot()\n    set_at(ref slot)\n    return items[slot.at]\n",
        ],
    },
    // An import renamed with `as`, written as the module's name, and the body behind it changed.
    Piece {
        variants: &[
            "from parts_a import scaled\n\ndef use_scaled() -> int:\n    return scaled(2)\n# file: parts_a.fen\nexport def scaled(n: int) -> int:\n    return n * 3\n",
            "from parts_a import scaled as grown\n\ndef use_scaled() -> int:\n    return grown(2)\n# file: parts_a.fen\nexport def scaled(n: int) -> int:\n    return n * 3\n",
            "import parts_a\n\ndef use_scaled() -> int:\n    return parts_a.scaled(2)\n# file: parts_a.fen\nexport def scaled(n: int) -> int:\n    return n * 3\n",
            "from parts_a import scaled as grown\n\ndef use_scaled() -> int:\n    return grown(2)\n# file: parts_a.fen\nexport def scaled(n: int) -> int:\n    return n * 4\n",
        ],
    },
    // The module the function `mode_cost` names is imported from, swapped: each takes an enum of
    // its own by a leading dot, whose case is at another place, or has a payload. A program's
    // files share their top-level names, so the two have their own, and `as` gives them one.
    Piece {
        variants: &[
            "from parts_a import cost_a as mode_cost\n\ndef costed() -> int:\n    return mode_cost(.Fast)\n# file: parts_a.fen\nexport enum Mode:\n    Fast\n    Slow\n\nexport def cost_a(mode: Mode) -> int:\n    match mode:\n        case .Fast:\n            return 1\n        case .Slow:\n            return 2\n# file: parts_b.fen\nexport enum Gear:\n    Stopped\n    Slow\n    Fast\n\nexport def cost_b(gear: Gear) -> int:\n    match gear:\n        case .Stopped:\n            return 10\n        case .Slow:\n            return 20\n        case .Fast:\n            return 30\n",
            "from parts_b import cost_b as mode_cost\n\ndef costed() -> int:\n    return mode_cost(.Fast)\n# file: parts_a.fen\nexport enum Mode:\n    Fast\n    Slow\n\nexport def cost_a(mode: Mode) -> int:\n    match mode:\n        case .Fast:\n            return 1\n        case .Slow:\n            return 2\n# file: parts_b.fen\nexport enum Gear:\n    Stopped\n    Slow\n    Fast\n\nexport def cost_b(gear: Gear) -> int:\n    match gear:\n        case .Stopped:\n            return 10\n        case .Slow:\n            return 20\n        case .Fast:\n            return 30\n",
            "from parts_b import cost_b as mode_cost\n\ndef costed() -> int:\n    return mode_cost(.Fast)\n# file: parts_a.fen\nexport enum Mode:\n    Fast\n    Slow\n\nexport def cost_a(mode: Mode) -> int:\n    match mode:\n        case .Fast:\n            return 1\n        case .Slow:\n            return 2\n# file: parts_b.fen\nexport enum Gear:\n    Slow\n    Fast(level: int)\n\nexport def cost_b(gear: Gear) -> int:\n    match gear:\n        case .Slow:\n            return 20\n        case .Fast(level):\n            return 30 + level\n",
        ],
    },
    // A function moved to another file, then the import of it moved there too.
    Piece {
        variants: &[
            "def tripled(n: int) -> int:\n    return n * 3\n\ndef use_tripled() -> int:\n    return tripled(4)\n",
            "from parts_b import tripled\n\ndef use_tripled() -> int:\n    return tripled(4)\n# file: parts_b.fen\nexport def tripled(n: int) -> int:\n    return n * 3\n",
            "from parts_a import use_tripled\n# file: parts_a.fen\nfrom parts_b import tripled\n\nexport def use_tripled() -> int:\n    return tripled(4)\n# file: parts_b.fen\nexport def tripled(n: int) -> int:\n    return n * 3\n",
        ],
    },
    // A file growing past its 256 KiB span slot, a module or the program's own, so every file
    // after it starts somewhere else.
    Piece {
        variants: &[
            "from parts_a import padded_size\n# file: parts_a.fen\nexport def padded_size() -> int:\n    return 4\n",
            "from parts_a import padded_size\n# file: parts_a.fen\n# pad: 270000\nexport def padded_size() -> int:\n    return 4\n",
            "# pad: 270000\nfrom parts_a import padded_size\n# file: parts_a.fen\nexport def padded_size() -> int:\n    return 5\n",
        ],
    },
    // A function reading defaults: written before the piece after it in the first order.
    Piece { variants: &["def use_defaults() -> int:\n    return padded() + Margin().left\n"] },
    // A parameter's and a field's default, read by the function before them.
    Piece {
        variants: &[
            "def padded(width = 3) -> int:\n    return width\n\nstruct Margin:\n    left = 2\n",
            "def padded(width = 4) -> int:\n    return width\n\nstruct Margin:\n    left = 2\n",
            "def padded(width = 3) -> int:\n    return width\n\nstruct Margin:\n    left = -2\n",
            "def padded(width = 3.5) -> float:\n    return width\n\nstruct Margin:\n    left = 2\n",
            "def padded(width = 3) -> int:\n    return width\n\nstruct Margin:\n    left = margin_default()\n\ndef margin_default() -> int:\n    return 2\n",
        ],
    },
    // Lines put in above what follows, or taken out.
    Piece { variants: &["", "# A note above what follows.\n\n# And another.\n", "\n\n"] },
    // What a callee says of what it's handed changes while its callers stay as written: whether
    // it keeps what it's handed, and whether it changes it. The LLVM lane keeps each caller's
    // analysis across builds with what it asked of its callees.
    Piece {
        variants: &[
            "def kept_list(items: list[int]) -> list[int]:\n    return items\n\ndef holder() -> int:\n    items = [1, 2, 3]\n    held = kept_list(items)\n    return len(held) + items[0]\n\ndef touch(ref items: list[int]):\n    items.append(4)\n\ndef toucher() -> int:\n    mut items = [1, 2]\n    touch(ref items)\n    return items[0]\n",
            "def kept_list(items: list[int]) -> list[int]:\n    return [4, 5]\n\ndef holder() -> int:\n    items = [1, 2, 3]\n    held = kept_list(items)\n    return len(held) + items[0]\n\ndef touch(ref items: list[int]):\n    items.append(4)\n\ndef toucher() -> int:\n    mut items = [1, 2]\n    touch(ref items)\n    return items[0]\n",
            "def kept_list(items: list[int]) -> list[int]:\n    return items\n\ndef holder() -> int:\n    items = [1, 2, 3]\n    held = kept_list(items)\n    return len(held) + items[0]\n\ndef touch(ref items: list[int]):\n    _ = len(items)\n\ndef toucher() -> int:\n    mut items = [1, 2]\n    touch(ref items)\n    return items[0]\n",
        ],
    },
];

/// `main`, which calls into every piece.
const MAIN: &str = "def main(stdout: Stdout, clock: Clock):
    stdout.print(double(3))
    stdout.print(label(2))
    stdout.print(measure(Dot(x=1)))
    mut bag = Bag()
    bag.items.append(1)
    run(ref bag)
    stdout.print(bag.total)
    stdout.print(over(3), under(3))
    stdout.print(Limit().most)
    with opened(stdout) as one:
        stdout.print(one)
    shared scores = Scores()
    lock scores as held:
        rest(clock)
        stdout.print(held.total)
    stdout.print(made_point().x)
    stdout.print(rec_a(made_rec()))
    stdout.print(read_slot([4, 5, 6]))
    stdout.print(use_scaled(), costed(), use_tripled(), padded_size())
    stdout.print(use_defaults())
    stdout.print(holder(), toucher())
    return
";

/// An edit: the pieces it switches and the variant each switches to, whether it moves the first of
/// them to the end, and whether the version after it lays its types out boxed
/// (`FENNEC_BOXED_TYPES`), as they were before step 4.
struct Edit {
    switched: Vec<(usize, usize)>,
    moved: bool,
    boxed: bool,
}

/// What the input chooses: the order the pieces are written in at first, then each edit.
struct Plan {
    order: Vec<usize>,
    edits: Vec<Edit>,
}

fn plan(input: &[u8]) -> Plan {
    let mut bytes = input.iter().copied();
    let mut byte = || bytes.next().unwrap_or(0);
    let mut order: Vec<usize> = (0..PIECES.len()).collect();
    // The first order: a few swaps the input picks.
    for _ in 0..usize::from(byte() % 4) {
        let (one, other) = (usize::from(byte()) % PIECES.len(), usize::from(byte()) % PIECES.len());
        order.swap(one, other);
    }
    let edits = (0..=usize::from(byte() % 4))
        .map(|_| {
            let switched = (0..=usize::from(byte() % 2))
                .map(|_| {
                    let piece = usize::from(byte()) % PIECES.len();
                    let count = PIECES.get(piece).map_or(1, |piece| piece.variants.len());
                    (piece, usize::from(byte()) % count)
                })
                .collect();
            let way = byte();
            Edit { switched, moved: way % 6 == 0, boxed: way % 5 == 1 }
        })
        .collect();
    Plan { order, edits }
}

/// The files of a version, `program.fen` first, each by name with its text.
type Files = Vec<(String, String)>;

/// A version of the program: its files, and whether it's built with its types boxed.
struct Version {
    files: Files,
    boxed: bool,
}

/// A file being written: its imports, then the rest.
#[derive(Default)]
struct Writing {
    name: String,
    imports: String,
    rest: String,
}

/// `bytes` of comment, in lines.
fn padding(bytes: usize) -> String {
    let line = "# Padding, so what follows starts past a span slot.\n";
    line.repeat(bytes.div_ceil(line.len()))
}

/// The files with each piece written as `chosen` says, in `order`, and `main` at the end of the
/// program's own.
fn written(order: &[usize], chosen: &[usize]) -> Files {
    let mut files = vec![Writing { name: PROGRAM.to_owned(), ..Writing::default() }];
    for &piece in order {
        let variant = chosen.get(piece).copied().unwrap_or(0);
        let text = PIECES.get(piece).and_then(|piece| piece.variants.get(variant)).copied().unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let mut at = 0;
        let mut touched = vec![0];
        for line in text.lines() {
            if let Some(name) = line.strip_prefix(FILE_MARK) {
                at = files.iter().position(|file| file.name == name).unwrap_or_else(|| {
                    files.push(Writing { name: name.to_owned(), ..Writing::default() });
                    files.len() - 1
                });
                touched.push(at);
                continue;
            }
            let Some(file) = files.get_mut(at) else { continue };
            if let Some(bytes) = line.strip_prefix(PAD_MARK) {
                file.rest.push_str(&padding(bytes.parse().unwrap_or(0)));
            } else if line.starts_with("from ") || line.starts_with("import ") {
                let _ = writeln!(file.imports, "{line}");
            } else {
                let _ = writeln!(file.rest, "{line}");
            }
        }
        // A blank line after each piece, in each file it wrote to.
        touched.sort_unstable();
        touched.dedup();
        for at in touched {
            if let Some(file) = files.get_mut(at) {
                file.rest.push('\n');
            }
        }
    }
    if let Some(program) = files.first_mut() {
        program.rest.push_str(MAIN);
    }
    files
        .into_iter()
        .map(|file| {
            let gap = if file.imports.is_empty() { "" } else { "\n" };
            (file.name, format!("{}{gap}{}", file.imports, file.rest))
        })
        .collect()
}

/// Each version of the program the input makes: the first, then one after each edit.
fn versions(plan: &Plan) -> Vec<Version> {
    let mut order = plan.order.clone();
    let mut chosen = vec![0; PIECES.len()];
    let mut out = vec![Version { files: written(&order, &chosen), boxed: false }];
    for edit in &plan.edits {
        for &(piece, variant) in &edit.switched {
            if let Some(held) = chosen.get_mut(piece) {
                *held = variant;
            }
        }
        if edit.moved
            && let Some(&(first, _)) = edit.switched.first()
        {
            order.retain(|&piece| piece != first);
            order.push(first);
        }
        out.push(Version { files: written(&order, &chosen), boxed: edit.boxed });
    }
    out
}

/// The files of a version, one after another under their names, for messages and `fennec-fuzz
/// show edits`. A padded file shows its padding as the line that stands for it.
fn shown_files(version: &Version) -> String {
    let mut out = String::new();
    if version.boxed {
        out.push_str("# ==== built with FENNEC_BOXED_TYPES=1 ====\n");
    }
    for (name, text) in &version.files {
        let _ = writeln!(out, "# ==== {name} ====");
        let mut padded = false;
        for line in text.lines() {
            if line.starts_with("# Padding,") {
                if !padded {
                    let _ = writeln!(out, "{PAD_MARK}...");
                }
                padded = true;
                continue;
            }
            padded = false;
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// The program the input makes, each version after the one before it, for `fennec-fuzz show
/// edits`.
pub(crate) fn shown(input: &[u8]) -> String {
    let versions = versions(&plan(input));
    versions.iter().map(shown_files).collect::<Vec<_>>().join("\n# ---- the next edit ----\n\n")
}

/// Writes a version's files into `folder`, made afresh, so no file of another version is left.
fn write_files(folder: &Path, files: &Files) -> Result<(), String> {
    fresh_private(folder)?;
    for (name, text) in files {
        let path = folder.join(name);
        std::fs::write(&path, text).map_err(|err| format!("{}: {err}", path.display()))?;
    }
    Ok(())
}

/// What a check gives back: its signatures, shown with each open type variable's number taken out,
/// since two checks that agree can number theirs apart, and whether they're of the module as it's
/// left. Or what it finds wrong.
type Checked = Result<(String, bool), Vec<String>>;

/// `text` with each open type variable's number taken out: `Var(12)` as `Var(_)`.
fn without_var_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("Var(") {
        let (before, after) = rest.split_at(at + "Var(".len());
        out.push_str(before);
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            out.push('_');
        }
        rest = after.get(digits..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

fn as_checked(found: Result<(fennec_check::Signatures, bool), Vec<fennec_syntax::Diagnostic>>) -> Checked {
    found
        .map(|(signatures, as_left)| (without_var_numbers(&format!("{signatures:?}")), as_left))
        .map_err(|problems| problems.iter().map(|problem| format!("{problem:?}")).collect())
}

/// The program's module, loaded from its files in `folder` as `fennec build` loads them, or what
/// stops it loading.
fn loaded(files: &Files, folder: &Path) -> Result<fennec_syntax::Module, Vec<String>> {
    loaded_with_sources(files, folder).map(|(module, _)| module)
}

/// `loaded`, with the files' texts as the load laid them out.
fn loaded_with_sources(
    files: &Files,
    folder: &Path,
) -> Result<(fennec_syntax::Module, fennec_syntax::Sources), Vec<String>> {
    write_files(folder, files).map_err(|err| vec![err])?;
    let overlay = fennec_kit::Overlay::new();
    fennec_kit::load_with(&folder.join(PROGRAM), PROGRAM, fennec_kit::Entry::Main, &overlay)
        .map(|program| (program.module, program.sources))
        .map_err(|problems| problems.diagnostics.iter().map(|problem| format!("load: {problem:?}")).collect())
}

/// What a check kept, with the files it was found in.
type Kept = (KeptCheck, fennec_syntax::Sources);

/// The files loaded using what the load before kept, with the loader's own comparison with loading
/// it all again on, against loading them all again here: they load the same, or fail the same.
/// Whether the load used anything kept.
fn loads_agree(files: &Files, folder: &Path, kept: &mut fennec_kit::KeptLoad) -> Result<bool, String> {
    write_files(folder, files)?;
    let compare = fennec_kit::Compare::Always;
    let found =
        fennec_kit::load_keeping(&folder.join(PROGRAM), PROGRAM, fennec_kit::Entry::Main, kept, compare)
            .map(|program| program.module)
            .map_err(|problems| {
                problems.diagnostics.iter().map(|problem| format!("load: {problem:?}")).collect::<Vec<_>>()
            });
    let used = found.is_ok() && (kept.used().parses > 0 || kept.used().rewritings > 0);
    let again = loaded(files, folder);
    if found != again {
        return Err(format!(
            "a load using what the one before kept loaded\n{found:?}\nwhere loading it all again loaded\n{again:?}"
        ));
    }
    Ok(used)
}

/// A check of the files that keeps what it finds, carrying over from `kept` what the edit since
/// left as it was, as the build daemon does (`fennec_check::Edit::between`), with the checker's
/// own comparison on.
fn kept_check(files: &Files, folder: &Path, kept: Option<Kept>) -> (Checked, Option<Kept>) {
    let (mut module, sources) = match loaded_with_sources(files, folder) {
        Ok(loaded) => loaded,
        Err(problems) => return (Err(problems), None),
    };
    let carried =
        kept.and_then(|(kept, before)| Some((kept, fennec_check::Edit::between(&before, &sources)?)));
    let found = match carried {
        Some((kept, edit)) => check_to_build_keeping(&mut module, (Some(kept), Some(&edit)), true),
        None => check_to_build_keeping(&mut module, (None, None), true),
    };
    match found {
        Ok((signatures, as_left, kept)) => (as_checked(Ok((signatures, as_left))), Some((kept, sources))),
        Err(problems) => (as_checked(Err(problems)), None),
    }
}

/// A whole check of the files, keeping nothing.
fn whole_check(files: &Files, folder: &Path) -> Checked {
    let mut module = loaded(files, folder)?;
    as_checked(check_to_build_saying(&mut module))
}

/// A folder of its own for a run's checks, so runs side by side don't share one.
fn check_folder() -> PathBuf {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    crate::llvm::work().join(format!("edits-check-{}", RUNS.fetch_add(1, Ordering::Relaxed)))
}

/// How many versions carried anything over from the one before: of its load, and of its check.
#[derive(Clone, Copy, Default)]
struct Carried {
    loads: usize,
    checks: usize,
}

/// Each version loaded using what the load before it kept, and checked carrying over what the
/// check before it kept: each loads as loading it all again does, and finds what a whole check
/// finds.
fn checks_agree(versions: &[Version]) -> Result<Carried, String> {
    let folder = check_folder();
    let agreed = checks_agree_in(versions, &folder);
    let _ = std::fs::remove_dir_all(&folder);
    agreed
}

fn checks_agree_in(versions: &[Version], folder: &Path) -> Result<Carried, String> {
    let mut kept = None;
    let mut kept_load = fennec_kit::KeptLoad::default();
    let mut carried = Carried::default();
    for (at, version) in versions.iter().enumerate() {
        let used = loads_agree(&version.files, folder, &mut kept_load)
            .map_err(|why| format!("after edit {at}, {why}\nin:\n{}", shown_files(version)))?;
        carried.loads += usize::from(used);
        let (found, next) = kept_check(&version.files, folder, kept);
        if next.as_ref().is_some_and(|(next, _)| next.carried_over() > 0) {
            carried.checks += 1;
        }
        let whole = whole_check(&version.files, folder);
        if found != whole {
            return Err(format!(
                "after edit {at}, a check carrying over what the one before kept found\n{found:?}\nwhere a whole \
                 check found\n{whole:?}\nin:\n{}",
                shown_files(version)
            ));
        }
        kept = next;
    }
    Ok(carried)
}

/// The version's program as a build lowers it, if it checks and every function lowers.
fn lowered(files: &Files, folder: &Path) -> Option<fennec_ir::Program> {
    let mut module = loaded(files, folder).ok()?;
    check_to_build_saying(&mut module).ok()?;
    let reached = fennec_build::what_main_reaches(&module);
    let signatures = fennec_check::check_to_emit(&reached).ok()?;
    let options =
        fennec_ir::Options { outline_parallel: true, stop_on_failed_main: true, serve_in_order: false };
    let mut lowered = fennec_ir::lower_with(&reached, &signatures, options);
    if fennec_llvm::uses_parallel(&lowered) {
        fennec_ir::guard_shared(&mut lowered);
    }
    let main_at = reached.items.iter().find_map(|item| match item {
        fennec_syntax::Item::Function(def) if def.name == "main" => Some(def.span),
        _ => None,
    });
    fennec_llvm::wrap_main(&mut lowered, main_at.unwrap_or_default());
    lowered.not_lowered.is_empty().then_some(lowered)
}

/// Each version that checks and lowers has its functions' analyses worked out using again what
/// the version before kept, as an edit build through the daemon does, and every one used again
/// is what analyzing it again gives. How many versions used any again.
fn analyses_agree(versions: &[Version]) -> Result<usize, String> {
    let folder = check_folder();
    let agreed = analyses_agree_in(versions, &folder);
    let _ = std::fs::remove_dir_all(&folder);
    agreed
}

fn analyses_agree_in(versions: &[Version], folder: &Path) -> Result<usize, String> {
    let mut kept = fennec_llvm::KeptForNext::default();
    let mut used = 0;
    for (at, version) in versions.iter().enumerate() {
        // A version that doesn't build leaves what the one before kept, as it does in the daemon.
        let Some(lowered) = lowered(&version.files, folder) else { continue };
        let (differ, used_again) = fennec_llvm::analyses_kept_and_checked(&lowered, &mut kept)
            .map_err(|why| format!("after edit {at}, {why}, in:\n{}", shown_files(version)))?;
        if !differ.is_empty() {
            return Err(format!(
                "after edit {at}, the analyses of {differ:?} used again from the version before aren't what \
                 analyzing them again gives, in:\n{}",
                shown_files(version)
            ));
        }
        if used_again > 0 {
            used += 1;
        }
    }
    Ok(used)
}

/// How a build is made: with the edit cache or nothing kept, and with types boxed or not.
#[derive(Clone, Copy)]
struct Way {
    cached: bool,
    boxed: bool,
}

/// `fennec build` of the program at `program` into `executable`, with `envs` set: whether it
/// built. A build that goes wrong, rather than finding the program wrong, is an error.
fn built(
    fennec: &Path,
    program: &Path,
    executable: &Path,
    envs: &[(&str, &Path)],
    way: Way,
) -> Result<bool, String> {
    let mut command = Command::new(fennec);
    command.args(["build", "--llvm"]).arg(program).arg("-o").arg(executable).env("FENNEC_NO_DAEMON", "1");
    for (name, value) in envs {
        command.env(name, value);
    }
    if way.boxed {
        command.env("FENNEC_BOXED_TYPES", "1");
    } else {
        command.env_remove("FENNEC_BOXED_TYPES");
    }
    if way.cached {
        command.env("FENNEC_CACHE_AUDIT", "1");
    } else {
        command.env("FENNEC_NO_CACHE", "1");
    }
    let lines = std::fs::read_to_string(program).map_or(0, |source| source.lines().count());
    let built = crate::llvm::built_within(&mut command, lines)?;
    let said = String::from_utf8_lossy(&built.stderr);
    if built.status.success() {
        return Ok(true);
    }
    if built.status.code() == Some(1) && !crate::llvm::BROKEN.iter().any(|broken| said.contains(broken)) {
        return Ok(false);
    }
    Err(format!("it didn't build ({}):\n{said}", built.status))
}

/// An empty folder at `path`, made afresh, reachable only by this user, as a build's home has to
/// be.
fn fresh_private(path: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(path);
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path).map_err(|err| format!("{}: {err}", path.display()))
}

/// The files in `folder`, by name, with what each holds.
fn files_in(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            std::fs::read(entry.path()).ok().map(|bytes| (name, bytes))
        })
        .collect();
    files.sort();
    files
}

/// Each version built in order with the edit cache, and each after the first built again with
/// nothing kept: where both build, the executables and the linked objects are the same, byte for
/// byte. Gives back how many versions were compared.
fn builds_agree(fennec: &Path, versions: &[Version], work: &Path) -> Result<usize, String> {
    let (kept_home, cold_home) = (work.join("home-kept"), work.join("home-cold"));
    fresh_private(&kept_home)?;
    let src = work.join("src");
    let program = src.join(PROGRAM);
    let mut compared = 0;
    for (at, version) in versions.iter().enumerate() {
        write_files(&src, &version.files)?;
        let source = shown_files(version);
        let (kept_objects, cold_objects) = (work.join("objects-kept"), work.join("objects-cold"));
        fresh_private(&kept_objects)?;
        let kept_executable = work.join("kept");
        let kept_built = built(
            fennec,
            &program,
            &kept_executable,
            &[("FENNEC_HOME", &kept_home), ("FENNEC_LINK_KEEP", &kept_objects)],
            Way { cached: true, boxed: version.boxed },
        )
        .map_err(|err| format!("version {at} built with the cache: {err}\n{source}"))?;
        if at == 0 || !kept_built {
            continue;
        }
        fresh_private(&cold_home)?;
        fresh_private(&cold_objects)?;
        let cold_executable = work.join("cold");
        let cold_built = built(
            fennec,
            &program,
            &cold_executable,
            &[("FENNEC_HOME", &cold_home), ("FENNEC_LINK_KEEP", &cold_objects)],
            Way { cached: false, boxed: version.boxed },
        )
        .map_err(|err| format!("version {at} built with nothing kept: {err}\n{source}"))?;
        if !cold_built {
            return Err(format!("version {at} built with the cache but not with nothing kept:\n{source}"));
        }
        let read = |path: &PathBuf| std::fs::read(path).map_err(|err| format!("{}: {err}", path.display()));
        if read(&kept_executable)? != read(&cold_executable)? {
            return Err(format!(
                "after edit {at}, the executable built with the cache differs from one built with nothing kept:\n{source}"
            ));
        }
        let (kept_files, cold_files) = (files_in(&kept_objects), files_in(&cold_objects));
        // A program whose whole build is kept links nothing, so it leaves no objects to compare.
        if !kept_files.is_empty() && kept_files != cold_files {
            let differing: Vec<&str> = kept_files
                .iter()
                .zip(&cold_files)
                .filter(|(kept, cold)| kept != cold)
                .map(|(kept, _)| kept.0.as_str())
                .collect();
            return Err(format!(
                "after edit {at}, the objects built with the cache differ from those built with nothing kept \
                 ({} against {} objects; differing: {differing:?}):\n{source}",
                kept_files.len(),
                cold_files.len()
            ));
        }
        compared += 1;
    }
    Ok(compared)
}

/// The target: every check carrying over agrees with a whole one, every analysis used again is
/// what analyzing it again gives, and where LLVM can build here, every build reusing the edit
/// cache makes what one keeping nothing makes.
pub(crate) fn edits(input: &[u8]) -> Result<(), String> {
    let versions = versions(&plan(input));
    checks_agree(&versions)?;
    analyses_agree(&versions)?;
    if crate::llvm::ready().is_err() {
        return Ok(());
    }
    builds_agree(&crate::llvm::fennec(), &versions, &crate::llvm::work().join("edits"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Edit, PIECES, Plan, analyses_agree, builds_agree, check_folder, checks_agree, loaded, versions,
    };

    /// An edit switching `switched`, moving none, laid out as `boxed` says.
    fn edit(switched: &[(usize, usize)], moved: bool, boxed: bool) -> Edit {
        Edit { switched: switched.to_vec(), moved, boxed }
    }

    /// The first way of writing every piece makes a program, of several files, that checks.
    #[test]
    fn the_first_program_checks() {
        let versions = versions(&Plan { order: (0..PIECES.len()).collect(), edits: Vec::new() });
        let first = &versions.first().unwrap_or_else(|| unreachable!()).files;
        assert!(first.len() >= 3, "{first:?}");
        let folder = check_folder();
        let mut module = loaded(first, &folder).unwrap_or_else(|err| unreachable!("{err:?}\n{first:?}"));
        let _ = std::fs::remove_dir_all(&folder);
        if let Err(problems) = fennec_check::check_to_build_saying(&mut module) {
            unreachable!("{problems:?}\n{first:?}");
        }
    }

    /// A short run of edits, as the gate runs: a body, a number turning negative away from where it
    /// indexes, a field added and a declaration moved, a file growing past its span slot, an
    /// import renamed, an enum's module swapped, a function and its import moved to another file
    /// and a default changed, some built with types boxed. Each is built reusing the edit cache and
    /// with nothing kept, byte for byte the same. It needs `fennec` built beside the tests, and
    /// says so where it isn't rather than passing.
    #[test]
    fn a_short_run_of_edits_builds_the_same_with_the_cache_and_without() {
        let fennec = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/fennec");
        if !fennec.exists() {
            eprintln!("skipped: {} isn't built", fennec.display());
            return;
        }
        let plan = Plan {
            order: (0..PIECES.len()).collect(),
            edits: vec![
                edit(&[(0, 1)], false, false),
                edit(&[(11, 1)], false, false),
                edit(&[(10, 1), (0, 2)], true, true),
                edit(&[(11, 2), (10, 2)], false, false),
                edit(&[(15, 1)], false, false),
                edit(&[(12, 1), (13, 1)], false, false),
                edit(&[(14, 2), (17, 1)], false, true),
            ],
        };
        let work = crate::llvm::work().join("edits-test");
        let compared =
            builds_agree(&fennec, &versions(&plan), &work).unwrap_or_else(|err| unreachable!("{err}"));
        let _ = std::fs::remove_dir_all(&work);
        assert!(compared >= 6, "only {compared} edits were built both ways");
    }

    /// Every switch of every piece, one at a time and then back, loads as loading it all again
    /// does and checks carrying over just what a whole check finds, and some of them carry
    /// something over.
    #[test]
    fn every_single_edit_checks_carrying_over_what_a_whole_check_finds() {
        let (mut loads, mut checks) = (0, 0);
        for (piece, kind) in PIECES.iter().enumerate() {
            for variant in 1..kind.variants.len() {
                let plan = Plan {
                    order: (0..PIECES.len()).collect(),
                    edits: vec![edit(&[(piece, variant)], false, false), edit(&[(piece, 0)], false, false)],
                };
                let carried = checks_agree(&versions(&plan)).unwrap_or_else(|err| unreachable!("{err}"));
                loads += carried.loads;
                checks += carried.checks;
            }
        }
        assert!(checks > 0, "no edit carried anything of a check over");
        assert!(loads > 0, "no edit used anything a load kept");
    }

    /// Every switch of every piece, one at a time and then back, uses again only analyses that are
    /// what analyzing them again gives, and most versions use some again.
    #[test]
    fn every_single_edit_uses_again_only_analyses_as_analyzing_again_gives() {
        let (mut used, mut versions_made) = (0, 0);
        for (piece, kind) in PIECES.iter().enumerate() {
            for variant in 1..kind.variants.len() {
                let plan = Plan {
                    order: (0..PIECES.len()).collect(),
                    edits: vec![edit(&[(piece, variant)], false, false), edit(&[(piece, 0)], false, false)],
                };
                used += analyses_agree(&versions(&plan)).unwrap_or_else(|err| unreachable!("{err}"));
                versions_made += 3;
            }
        }
        assert!(used * 2 > versions_made, "only {used} of {versions_made} versions used any analysis again");
    }
}
