//! Small programs, each with what the oracle prints for it and how it ends.

use fennec_oracle::{Ended, Workdir, run_source_in, run_source_within};

/// Runs a program, with single quotes standing in for double ones and its indentation taken off,
/// in the folder the test runs in.
fn ran(text: &str) -> (String, Ended) {
    ran_in(text, Workdir::current())
}

/// Runs a program as `ran` does, in an empty folder at `/work`, which the disk is never looked at
/// for.
fn ran_in_work(text: &str) -> (String, Ended) {
    ran_in(text, Workdir::empty(std::path::Path::new("/work")))
}

fn prints_in_work(text: &str, expected: &str) {
    let (printed, ended) = ran_in_work(text);
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, expected);
}

fn ran_in(text: &str, workdir: Workdir) -> (String, Ended) {
    let text = text.replace('\'', "\"");
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let lines: Vec<&str> = text.lines().map(|line| line.get(indent..).unwrap_or("")).collect();
    let source = format!("{}\n", lines.join("\n").trim_matches('\n'));
    let outcome = run_source_in(&source, "test.fen", workdir);
    (String::from_utf8_lossy(&outcome.stdout).into_owned(), outcome.ended)
}

fn prints(text: &str, expected: &str) {
    let (printed, ended) = ran(text);
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, expected);
}

#[test]
fn a_name_holds_its_own_value_and_ref_changes_yours() {
    prints(
        "
        struct Unit:
            name: str
            health = 100

        def heal_all(ref units: list[Unit]):
            for ref unit in units:
                unit.health += 10

        def main(stdout: Stdout):
            mut units = [Unit(name='Ana', health=50), Unit(name='Bo', health=70)]
            units[0].health -= 10
            heal_all(ref units)
            target = units[1]
            mut copied = units
            copied[0].health = 1
            stdout.print(units, target.health + 1, copied[0].health)
        ",
        "[Unit(name=\"Ana\", health=50), Unit(name=\"Bo\", health=80)] 81 1\n",
    );
}

#[test]
fn except_handles_the_failure_of_its_call() {
    prints(
        "
        error Missing:
            name: str

        def find(names: list[str], name: str) -> int fails Missing:
            for at, each in enumerate(names):
                if each == name:
                    return at
            fail Missing(name)

        def main(stdout: Stdout):
            names = ['ash', 'kit']
            found = find(names, 'kit') except why:
                stdout.print('no', why)
                -1
            stdout.print(found)
            lost = find(names, 'vex') except why:
                case Missing(name):
                    stdout.print('missing', name)
                    -1
            stdout.print(lost)
        ",
        "1\nmissing vex\n-1\n",
    );
}

#[test]
fn a_failure_out_of_main_stops_the_program_with_exit_code_1() {
    let (printed, ended) = ran("
        error Broken:
            reason: str

        def main(stdout: Stdout):
            stdout.print('before')
            fail Broken('on purpose')
        ");
    assert_eq!(printed, "before\n");
    let Ended::Stopped(said) = ended else { panic!("{ended:?}") };
    assert!(
        said.starts_with("error: `main` failed: Broken(reason=\"on purpose\")\n --> test.fen:6:5"),
        "{said}"
    );
}

#[test]
fn an_int_that_overflows_stops_the_program() {
    let (printed, ended) = ran("
        def factorial(n: int) -> int:
            if n <= 1: return 1
            return n * factorial(n - 1)

        def main(stdout: Stdout):
            stdout.print(factorial(20))
            stdout.print(factorial(21))
        ");
    assert_eq!(printed, "2432902008176640000\n");
    assert!(matches!(&ended, Ended::Stopped(said) if said.contains("overflows an int")), "{ended:?}");
}

#[test]
fn a_generator_and_its_reader_take_turns() {
    prints(
        "
        def countdown(out: Stdout, start: int) -> Iterator[int]:
            out.print('counting')
            mut n = start
            while n > 0:
                yield n
                n -= 1
            return out.print('liftoff')

        def naturals() -> Iterator[int]:
            mut n = 0
            while True:
                yield n
                n += 1

        def main(stdout: Stdout):
            for n in countdown(stdout, 2):
                stdout.print('got', n)
            stdout.print(any(n > 1 for n in naturals()), list(enumerate(n * 2 for n in range(2))))
        ",
        "counting\ngot 2\ngot 1\nliftoff\nTrue [(0, 0), (1, 2)]\n",
    );
}

#[test]
fn a_source_lends_to_the_block_and_finishes_after_it() {
    prints(
        "
        struct Ledger:
            lines = list[str]()

        def entry(ref ledger: Ledger, name: str) -> with str:
            ledger.lines.append(f'open {name}')
            lend name.upper() except:
                ledger.lines.append(f'undo {name}')
            ledger.lines.append(f'close {name}')

        def first(ref ledger: Ledger) -> int:
            with entry(ref ledger, 'bo') as label:
                return len(label)
            return 0

        def main(stdout: Stdout):
            mut ledger = Ledger()
            with entry(ref ledger, 'ana') as label:
                ledger.lines.append(label)
            stdout.print(first(ref ledger), ledger.lines)
        ",
        "2 [\"open ana\", \"ANA\", \"close ana\", \"open bo\", \"close bo\"]\n",
    );
}

#[test]
fn a_loop_and_a_block_keep_their_names_to_themselves() {
    prints(
        "
        def main(stdout: Stdout):
            at = 9
            for at in range(3):
                stdout.print(at)
            stdout.print(at)
        ",
        "0\n1\n2\n9\n",
    );
}

#[test]
fn an_else_ends_a_chain_at_a_missing_or_failed_step() {
    prints(
        "
        error Lost:
            why: str

        struct Part:
            name: str

        def find(n: int) -> Part fails Lost:
            if n > 1: fail Lost(why='no such part')
            return Part(name='gear')

        def main(stdout: Stdout):
            stdout.print(find(1).name else '?', find(2).name else '?')
            counts = {'a': 1}
            stdout.print(counts.get('b') else 0, counts.get('a') else 0)
        ",
        "gear ?\n0 1\n",
    );
}

#[test]
fn what_it_doesnt_model_is_refused_by_name() {
    let (printed, ended) = ran("
        from std import toml

        def main(stdout: Stdout):
            stdout.print('before')
            stdout.print(toml.dumps({'a': 1}))
        ");
    assert_eq!(printed, "");
    assert_eq!(ended, Ended::Unsupported("std.toml.dumps".to_owned()));
    let (_, ended) = ran("
        def main(stdout: Stdout, clock: Clock):
            stdout.print('hi')
        ");
    assert_eq!(ended, Ended::Unsupported("the capability Clock".to_owned()));
}

#[test]
fn a_run_that_goes_on_too_long_is_stopped() {
    let source = "def main(stdout: Stdout):\n    mut n = 0\n    while True:\n        n = (n + 1) % 7\n";
    let outcome = run_source_within(source, "test.fen", 100_000, Workdir::current());
    assert_eq!(outcome.ended, Ended::Unsupported("ran too long".to_owned()));
}

#[test]
fn a_buffer_is_held_as_the_list_it_prints_as() {
    prints(
        "
        def main(stdout: Stdout):
            mut buffer = Buffer[int](capacity=2)
            buffer.push(3)
            buffer.push(4)
            buffer.insert(0, 9)
            stdout.print(len(buffer), buffer.remove(1), buffer.pop(), buffer)
        ",
        "3 3 4 [9]\n",
    );
}

#[test]
fn a_value_of_a_generic_type_prints_with_no_type_arguments() {
    prints(
        "
        struct Holder[T]:
            item: T

        struct Pair[A, B]:
            first: A
            second: B

        def main(stdout: Stdout):
            stdout.print(Holder(item=3), Holder(item='x'), Holder(item=2.5), [Holder(item=1)])
            stdout.print(f'{Holder(item=[1, 2])} {str(Pair(first=4, second=Holder(item=True)))}')
        ",
        "Holder(item=3) Holder(item=\"x\") Holder(item=2.5) [Holder(item=1)]\nHolder(item=[1, 2]) Pair(first=4, second=Holder(item=True))\n",
    );
}

#[test]
fn a_table_gives_each_item_an_id_that_finds_it_or_nothing() {
    let (printed, ended) = ran("
        from std.collections import Table

        struct Unit:
            health = 1

        def main(stdout: Stdout):
            mut names = Table[str]()
            first = names.add('a')
            _ = names.add('b')
            third = names.add('c')
            _ = names.remove(first)
            _ = names.remove(third)
            fourth = names.add('d')
            fifth = names.add('e')
            stdout.print(names, len(names), fourth, fifth, fourth == first, first in names, names.get(first))
            mut copied = names
            _ = copied.add('f')
            stdout.print(copied == names, names.items())
            mut units = Table[Unit]()
            unit_id = units.add(Unit())
            units[unit_id].health += 5
            for ref unit in units:
                unit.health *= 2
            stdout.print(units, units.update(unit_id, Unit(health=9)), units[unit_id].health)
            _ = units.remove(unit_id)
            stdout.print(units[unit_id])
        ");
    assert_eq!(
        printed,
        "Table([\"d\", \"b\", \"e\"]) 3 Id(0) Id(2) False False None\n\
         False [(Id(0), \"d\"), (Id(1), \"b\"), (Id(2), \"e\")]\n\
         Table([Unit(health=12)]) True 9\n"
    );
    let Ended::Stopped(said) = ended else { panic!("it ended {ended:?}") };
    assert!(said.starts_with("error: this Id refers to something that's been removed\n"), "{said}");
}

/// Writes a kit's files into a folder of its own, and runs it.
fn kit_ran(name: &str, files: &[(&str, &str)]) -> std::io::Result<(String, Ended)> {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("oracle-kits").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap_or(&dir))?;
        std::fs::write(&path, text.replace('\'', "\""))?;
    }
    let outcome = fennec_oracle::run_path(&dir, name);
    Ok((String::from_utf8_lossy(&outcome.stdout).into_owned(), outcome.ended))
}

const KIT_TOML: &str = "name = 'shapes'\nversion = '0.1.0'\nfennec = '0.0.1'\n\n[dependencies]\n";

#[test]
fn a_kit_is_its_modules_merged_as_the_compiler_merges_them() {
    let main = "\
from shapes import Shape, area as size_of
from world import physics
import world.weather as sky

def main(stdout: Stdout):
    shapes = [Shape.Circle(2), physics.square(3)]
    for shape in shapes:
        stdout.print(size_of(shape), physics.fall(shape))
    stdout.print(sky.today(), sky.Kind.Rain, physics.GRAVITY)
";
    let shapes = "\
enum Shape:
    Circle(radius: int)
    Square(side: int)

def area(shape: Shape) -> int:
    match shape:
        case .Circle(radius):
            return 3 * radius * radius
        case .Square(side):
            return side * side
";
    let physics = "\
from shapes import Shape, area

GRAVITY = 10

def square(side: int) -> Shape:
    return .Square(side)

def fall(shape: Shape) -> int:
    return area(shape) * GRAVITY
";
    // `Shape` has a `Square` too, but this file can't see it, so `.Square` is a `Kind`.
    let weather = "\
enum Kind:
    Sun
    Rain
    Square

def today() -> Kind:
    kind = .Square
    return kind
";
    let files = [
        ("kit.toml", KIT_TOML),
        ("src/main.fen", main),
        ("src/shapes.fen", shapes),
        ("src/world/physics.fen", physics),
        ("src/world/weather.fen", weather),
    ];
    let (printed, ended) = kit_ran("merged", &files).unwrap();
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, "12 120\n9 90\nKind.Square Kind.Rain 10\n");
    let depends =
        KIT_TOML.replace("[dependencies]\n", "[dependencies]\nphysics = { folder = '../physics' }\n");
    let (_, ended) = kit_ran("depends", &[("kit.toml", &depends), ("src/main.fen", main)]).unwrap();
    assert_eq!(ended, Ended::Unsupported("a kit with dependencies".to_owned()));
}

#[test]
fn json_writes_a_value_as_its_type_saves_it_and_reads_it_back() {
    prints(
        "
        from std import json

        enum Tile:
            Floor
            Door(locked: bool)

        struct Room:
            name: str
            tiles: list[Tile]
            doors: dict[str, int]
            corners: dict[(int, int), str]
            pet: str? = None
            @serialize(name='Size')
            size = (3, 4)
            @skip
            seen = 0

        def main(stdout: Stdout):
            room = Room(
                name='hall',
                tiles=[Tile.Floor, Tile.Door(locked=True)],
                doors={'north': 1},
                corners={(0, 0): 'start'},
            )
            text = json.dumps(room)
            stdout.print(text)
            stdout.print(json.dumps(room.doors, indent=2))
            back = json.loads[Room](text)?
            stdout.print(back == room, back.pet is None, back.seen)
            stdout.print(json.loads[list[int?]]('[1, null, 3]')?)
        ",
        "{\"name\": \"hall\", \"tiles\": [\"Floor\", {\"Door\": {\"locked\": true}}], \"doors\": {\"north\": 1}, \
         \"corners\": [[[0, 0], \"start\"]], \"pet\": null, \"Size\": [3, 4]}\n\
         {\n  \"north\": 1\n}\n\
         True True 0\n\
         [1, None, 3]\n",
    );
}

#[test]
fn json_writes_floats_to_read_back_the_same_and_escapes_only_what_it_must() {
    prints(
        "
        from std import json

        def main(stdout: Stdout):
            floats = [1e20, 1e-7, -0.0, 0.1, 100.0]
            stdout.print(json.dumps(floats))
            stdout.print(json.loads[list[float]](json.dumps(floats))? == floats)
            stdout.print(json.dumps('tab\\there \\'quoted\\' \\\\ \\u0001 \\u0085 é'))
            stdout.print(json.loads[str]('\\'\\\\u00e9t\\\\u00e9 \\\\ud83d\\\\ude00\\'')?)
        ",
        "[1e20, 1e-7, -0.0, 0.1, 100.0]\nTrue\n\"tab\\there \\\"quoted\\\" \\\\ \\u0001 \\u0085 é\"\nété 😀\n",
    );
}

#[test]
fn a_json_load_that_fails_says_what_and_where_as_std_json_does() {
    prints(
        "
        from std import json

        struct Point:
            x: int
            y: int

        def why[T](run: () -> T fails ParseError) -> str:
            _ = run() except problem:
                return f'{problem.message} ({problem.line}:{problem.column})'
            return 'read'

        def main(stdout: Stdout):
            stdout.print(why(() => json.loads[list[Point]]('[\\n  {\\'x\\': 1,}\\n]')?))
            stdout.print(why(() => json.loads[list[Point]]('[{\\'x\\': 1, \\'y\\': \\'2\\'}]')?))
            stdout.print(why(() => json.loads[Point]('{\\'x\\': 1}')?))
            stdout.print(why(() => json.loads[Point]('{\\'x\\': 1, \\'y\\': 2, \\'z\\': 3}')?))
            stdout.print(why(() => json.loads[Point]('{\\'x\\': 1, \\'x\\': 2, \\'y\\': 3}')?))
            stdout.print(why(() => json.loads[dict[int, int]]('{\\'01\\': 1}')?))
        ",
        "expected a key in double quotes, but found `}`, at line 2, column 11 (2:11)\n\
         `[0].y` should be an int, but it's a str (0:0)\n\
         `y` is missing (0:0)\n\
         `Point` has no field \"z\", but the text has one (0:0)\n\
         the text has `x` twice (0:0)\n\
         the text has the key \"01\", which isn't how that int is written (0:0)\n",
    );
}

#[test]
fn a_json_enum_with_a_key_after_its_case_fails_once_its_fields_have_read() {
    prints(
        "
        from std import json

        enum Shape:
            Box(width: int, height: int)
            Dot

        struct Holder:
            shape: Shape

        def why[T](run: () -> T fails ParseError) -> str:
            _ = run() except problem:
                return problem.message
            return 'read'

        def main(stdout: Stdout):
            stdout.print(why(() => json.loads[Shape]('{\\'Box\\': {\\'width\\': 1, \\'height\\': 2}, \\'extra\\': 1}')?))
            stdout.print(why(() => json.loads[Shape]('{\\'Box\\': {\\'width\\': \\'no\\', \\'height\\': 2}, \\'extra\\': 1}')?))
            stdout.print(why(() => json.loads[Shape]('{\\'Dot\\': {}, \\'Box\\': 1}')?))
            stdout.print(why(() => json.loads[Holder]('{\\'shape\\': {\\'Dot\\': {}, \\'a\\\\nb\\': [1]}}')?))
        ",
        "the text has \"extra\" after its case, but an enum is written as an object with its case as the only key\n\
         `width` should be an int, but it's a str\n\
         the text has \"Box\" after its case, but an enum is written as an object with its case as the only key\n\
         `shape` has \"a\\nb\" after its case, but an enum is written as an object with its case as the only key\n",
    );
}

#[test]
fn json_refuses_a_failing_load_it_cant_be_sure_of_and_stops_on_nan() {
    let (printed, ended) = ran("
        from std import json

        struct Note:
            text: str
            @skip
            seen = 0

        def main(stdout: Stdout):
            stdout.print('before')
            _ = json.loads[Note]('{\\'text\\': \\'a\\', \\'\\': 1}') except ParseError():
                Note(text='')
        ");
    assert_eq!(printed, "");
    assert_eq!(
        ended,
        Ended::Unsupported("json.loads failing (a key that names a field `@skip` leaves out)".to_owned())
    );
    let (printed, ended) = ran("
        from std import json, math

        struct Stats:
            values: list[float]

        def main(stdout: Stdout):
            stdout.print('before')
            stdout.print(json.dumps([Stats(values=[1.0]), Stats(values=[2.0, math.NAN])]))
        ");
    assert_eq!(printed, "before\n");
    assert!(
        matches!(&ended, Ended::Stopped(said) if said.contains("`[1].values[1]` is nan, which JSON can't write")),
        "{ended:?}"
    );
}

#[test]
fn a_file_written_is_read_back_and_writing_again_replaces_it() {
    prints_in_work(
        "
        def main(stdout: Stdout, fs: Fs) fails IoError:
            stdout.print(fs.exists('notes.txt')?)
            fs.write_text('notes.txt', 'a longer first note')?
            fs.write_text('notes.txt', 'short')?
            stdout.print(fs.read_text('notes.txt')?, fs.metadata('notes.txt')?.size, fs.exists('notes.txt')?)
            stdout.print(fs.read_text_if_present('none.txt')? else 'nothing yet')
            fs.remove('notes.txt')?
            stdout.print(fs.exists('notes.txt')?)
        ",
        "False\nshort 5 True\nnothing yet\nFalse\n",
    );
}

#[test]
fn a_file_made_in_a_with_block_is_written_in_pieces_and_takes_its_place_when_the_block_ends() {
    prints_in_work(
        "
        error Stop:
            why: str

        def save(fs: Fs, lines: list[str]) -> bool fails IoError | Stop:
            with fs.create('save.txt')? as file:
                for line in lines:
                    if line == '': fail Stop('an empty line')
                    file.write(line)?
                    file.write_line('!')?
                return fs.exists('save.txt')?
            return False

        def main(stdout: Stdout, fs: Fs) fails IoError | Stop:
            stdout.print(save(fs, ['one', 'two'])?)
            stdout.print(fs.read_text('save.txt')?)
            _ = save(fs, ['three', '']) except why:
                stdout.print(why)
                False
            mut found = list[str]()
            with fs.open('save.txt')? as file:
                found.append(file.read_line()? else 'none')
                for line in file.lines()?:
                    found.append(line)
                found.append(str(file.read_line()? is None))
            stdout.print(found)
        ",
        "False\none!\ntwo!\n\nStop(why=\"an empty line\")\n[\"one!\", \"two!\", \"True\"]\n",
    );
}

#[test]
fn a_folder_is_listed_in_order_and_walked_a_folder_before_what_is_in_it() {
    prints_in_work(
        "
        from std.path import Path

        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.make_dir('game/world')?
            fs.write_text('game/world/map.fen', '')?
            fs.write_text('game/main.fen', '')?
            fs.write_text('game/Notes.txt', '')?
            stdout.print([path.text for path in fs.list_dir('game')?])
            stdout.print([path.text for path in fs.walk(Path('game'))?])
            first = fs.list_dir('./game/')?
            stdout.print(first[0], fs.metadata('game/world')?.is_dir)
        ",
        "[\"game/Notes.txt\", \"game/main.fen\", \"game/world\"]\n\
         [\"game/Notes.txt\", \"game/main.fen\", \"game/world\", \"game/world/map.fen\"]\n\
         Path(text=\"./game/Notes.txt\") True\n",
    );
}

/// What makes each of these fail, as `why` shows it, indented as the programs that use it are.
const WHY: &str = "
        def why[T](run: () -> T fails IoError) -> str:
            _ = run() except failed:
                return f'{failed}'
            return 'fine'
";

#[test]
fn the_file_system_fails_with_the_case_and_the_words_the_runtime_gives() {
    prints_in_work(
        &format!(
            "{WHY}
        def read(fs: Fs, path: str) -> str fails IoError:
            with fs.open(path)? as file:
                return file.read_text()?
            return ''

        def removed(fs: Fs, path: str) -> str fails IoError:
            fs.remove(path)?
            return 'removed'

        def made(fs: Fs, path: str) -> str fails IoError:
            fs.make_dir(path)?
            return 'made'

        def written(fs: Fs) -> str fails IoError:
            with fs.open('note.txt')? as file:
                file.write_line('more')?
            return 'written'

        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.make_dir('folder/inner')?
            fs.write_text('note.txt', 'x')?
            stdout.print(why(() => fs.read_text('missing.txt')?))
            stdout.print(why(() => fs.read_text('folder')?))
            stdout.print(why(() => read(fs, 'folder')?))
            stdout.print(why(() => fs.list_dir('missing')?))
            stdout.print(why(() => removed(fs, 'folder')?))
            stdout.print(why(() => made(fs, 'note.txt')?))
            stdout.print(why(() => written(fs)?))
            stdout.print(why(() => fs.read_text('a\\x00b')?))
        "
        ),
        "IoError.NotFound(reason=\"nothing is there\")\n\
         IoError.Other(reason=\"it's a folder, not a file\")\n\
         IoError.Other(reason=\"it's a folder, not a file\")\n\
         IoError.NotFound(reason=\"nothing is there\")\n\
         IoError.Other(reason=\"the folder isn't empty\")\n\
         IoError.AlreadyExists(reason=\"something is there already\")\n\
         IoError.Other(reason=\"this file was opened to read, not to write\")\n\
         IoError.Other(reason=\"`a\\\\0b` can't be a path: it holds a NUL\")\n",
    );
}

/// Windows says a path through a file isn't there, so the oracle refuses one there.
#[cfg(unix)]
#[test]
fn a_path_through_a_file_fails_as_not_a_folder() {
    prints_in_work(
        &format!(
            "{WHY}
        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.write_text('note.txt', 'x')?
            stdout.print(why(() => fs.read_text('note.txt/x')?))
            stdout.print(why(() => fs.exists('note.txt/x')?))
            stdout.print(why(() => fs.list_dir('note.txt')?))
        "
        ),
        &"IoError.Other(reason=\"part of the path is a file, not a folder\")\n".repeat(3),
    );
}

#[test]
fn a_narrowed_fs_reaches_only_its_folder_and_a_read_only_one_changes_nothing() {
    prints_in_work(
        "
        def why[T](run: () -> T fails IoError) -> str:
            _ = run() except failed:
                return failed.message()
            return 'fine'

        def wrote(fs: Fs, path: str) -> str fails IoError:
            fs.write_text(path, 'x')?
            return 'written'

        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.make_dir('saves/slot1')?
            fs.write_text('secret.txt', 'hunter2')?
            saves = fs.only('saves')
            saves.write_text('saves/slot1/game.json', '{}')?
            stdout.print(saves.read_text('saves/./slot1/../slot1/game.json')?, saves.exists('saves')?)
            stdout.print(why(() => saves.read_text('secret.txt')?))
            stdout.print(why(() => saves.read_text('saves/../secret.txt')?))
            stdout.print(why(() => saves.only('other').exists('other')?))
            stdout.print(why(() => fs.only('missing').exists('missing/x')?))
            reader = saves.read_only()
            stdout.print(reader.read_text('saves/slot1/game.json')?, why(() => wrote(reader, 'saves/new.txt')?))
            stdout.print(why(() => saves.read_text('saves/a\\x00b')?))
        ",
        "{} True\n\
         `secret.txt` is outside what this Fs can reach: `/work/saves`\n\
         `saves/../secret.txt` is outside what this Fs can reach: `/work/saves`\n\
         `other` is outside what this Fs can reach: nothing\n\
         nothing is there\n\
         {} this Fs only reads, so it can't change `saves/new.txt`\n\
         `saves/a\\0b` can't be a path: it holds a NUL\n",
    );
}

#[test]
fn a_path_is_its_text_and_its_methods_only_look_at_it() {
    prints_in_work(
        "
        from std.path import Path

        def main(stdout: Stdout, fs: Fs) fails IoError:
            slot = Path('saves') / 'slot1.json'
            stdout.print(slot, slot.name(), slot.stem(), slot.extension(), slot.parent().text, slot.is_absolute())
            stdout.print(Path('a/./b/../c').normalize().text, Path('/a/../../b').normalize().text, Path('x').join('/etc').text)
            inside = Path('www').inside('/css/site.css') except why:
                Path(why.message)
            outside = Path('www').inside('../secret') except why:
                Path(why.message)
            stdout.print(inside.text, outside.text)
            fs.write_text(slot.name(), 'kept')?
            stdout.print(fs.read_text(Path('slot1.json'))?)
        ",
        "Path(text=\"saves/slot1.json\") slot1.json slot1 json saves False\n\
         a/c /b /etc\n\
         www/css/site.css `../secret` would leave `www`\n\
         kept\n",
    );
}

#[test]
fn a_program_makes_an_io_error_of_its_own() {
    prints_in_work(
        "
        def checked(fs: Fs, path: str) -> str fails IoError:
            text = fs.read_text(path) except why:
                case .NotFound():
                    fail .Other(reason=f'{path} is missing: {why.message()}')
                case _:
                    fail why
            return text

        def main(stdout: Stdout, fs: Fs):
            _ = checked(fs, 'missing.txt') except why:
                stdout.print(why, why.message())
                ''
        ",
        "IoError.Other(reason=\"missing.txt is missing: nothing is there\") missing.txt is missing: nothing is there\n",
    );
}

#[test]
fn what_differs_from_one_system_to_another_is_refused() {
    let refused = |body: &str, why: &str| {
        let source = format!("def main(stdout: Stdout, fs: Fs) fails IoError:\n{body}");
        let (printed, ended) = ran_in_work(&source);
        assert_eq!((printed.as_str(), ended), ("", Ended::Unsupported(why.to_owned())), "{body}");
    };
    refused(
        "    with fs.create('made.txt')? as file:\n        stdout.print(fs.list_dir('.')?)\n",
        "what's in a folder while a file is made in it, under a name holding the process's number",
    );
    refused(
        "    fs.write_text('Notes.txt', 'a')?\n    stdout.print(fs.exists('notes.txt')?)\n",
        "the name `notes.txt` beside `Notes.txt`, which some systems take as the same name",
    );
    refused("    stdout.print(fs.exists('/etc/hosts')?)\n", "a path outside the folder the program runs in");
    refused(
        "    stdout.print(fs.metadata('.')?.is_dir)\n    stdout.print(fs.metadata('.')?)\n",
        "a folder's size, which differs from one system to another",
    );
    refused("    _ = fs.read_bytes('a.bin')?\n", "bytes");
}

/// In the folder the test runs in, the crate's own, the oracle sees what's there, reads none of
/// it, and writes nothing to the disk.
#[test]
fn the_disk_is_looked_at_and_never_written() {
    let (printed, ended) = ran("
        def main(stdout: Stdout, fs: Fs) fails IoError:
            fs.write_text('written-by-the-oracle.txt', 'x')?
            fs.make_dir('made-by-the-oracle/inside')?
            stdout.print(fs.exists('Cargo.toml')?, fs.exists('src')?, fs.exists('no-such-file.txt')?)
            stdout.print(fs.read_text('written-by-the-oracle.txt')?, fs.walk('made-by-the-oracle')?)
        ");
    assert_eq!(ended, Ended::Exited(0), "it printed:\n{printed}");
    assert_eq!(printed, "True True False\nx [Path(text=\"made-by-the-oracle/inside\")]\n");
    assert!(!std::path::Path::new("written-by-the-oracle.txt").exists());
    assert!(!std::path::Path::new("made-by-the-oracle").exists());
    let (_, ended) = ran("
        def main(stdout: Stdout, fs: Fs) fails IoError:
            stdout.print(fs.read_text('Cargo.toml')?)
        ");
    assert_eq!(
        ended,
        Ended::Unsupported("reading a file that was there before the program started".to_owned())
    );
}
