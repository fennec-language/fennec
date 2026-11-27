//! The standard library modules whose functions exist only once imported: `std.json`,
//! `std.toml` and `std.path`.

use crate::{helps, problems};

/// The problems in statements, in a `main` that can fail, after some imports.
fn with_imports(imports: &str, body: &str) -> Vec<String> {
    let body: String = crate::dedent(body).lines().map(|line| ["    ", line, "\n"].concat()).collect();
    problems(&format!("{imports}\ndef main(stdout: Stdout):\n{body}"))
}

#[test]
fn module_functions_need_importing() {
    let program = "
        stdout.print(loads('1')?)
    ";
    assert_eq!(with_imports("", program), ["`loads` doesn't exist @ loads"]);
    let program = "
        stdout.print(loads[int]('1')?)
    ";
    assert_eq!(with_imports("from std.json import loads", program), Vec::<String>::new());
}

/// The standard library's own helpers and constants are its own: a program reaches an exported
/// constant by importing it, as it does a function, and can't name what std keeps to itself, as
/// str.fen's `padding` or std.net's `MOST_ROUTE_PARAMETERS`. A program's own of either name is
/// its own, and std's code still means std's.
#[test]
fn std_keeps_its_own_helpers_and_constants() {
    let reached = "
        stdout.print(padding('x', count=2), MOST_ROUTE_PARAMETERS)
    "
    .replace('\'', "\"");
    assert_eq!(
        with_imports("", &reached),
        [
            "`padding` is private to the standard library @ padding",
            "`MOST_ROUTE_PARAMETERS` is private to `std.net` @ MOST_ROUTE_PARAMETERS"
        ]
    );
    let imported = "
        stdout.print(INFINITY > 1.0, math.NAN.is_nan(), DIGITS)
    ";
    let imports = "from std import math\nfrom std.math import INFINITY\nfrom std.string import DIGITS";
    assert_eq!(with_imports(imports, imported), Vec::<String>::new());
    let own = "
        stdout.print(padding(2), MOST_ROUTE_PARAMETERS, 'ab'.ljust(4))
    "
    .replace('\'', "\"");
    let defined = "def padding(count: int) -> int:\n    return count\n\nMOST_ROUTE_PARAMETERS = 1\n";
    assert_eq!(with_imports(defined, &own), Vec::<String>::new());
}

/// Every top-level function and constant the standard library's own files have is out of a
/// program's reach by its bare name: what std keeps to itself is said to be private, and what a
/// module exports needs importing. Only what the language gives everywhere, like `failure_of`, is
/// there without an import.
#[test]
fn no_name_std_keeps_is_reached_from_a_program() {
    let mut names = std::collections::BTreeSet::new();
    for (file, source) in fennec_syntax::prelude_files() {
        let module =
            fennec_syntax::parse_prelude(source).unwrap_or_else(|problem| panic!("{file}: {problem:?}"));
        for item in module.items {
            match item {
                fennec_syntax::Item::Function(def) => names.insert(def.name),
                fennec_syntax::Item::Const { name, .. } => names.insert(name),
                _ => false,
            };
        }
    }
    let everywhere = |name: &String| {
        fennec_check::provided_everywhere(name) || fennec_check::reserved_name(name) || name == "failure_of"
    };
    let names: Vec<String> =
        names.into_iter().filter(|name| !name.starts_with('_') && !everywhere(name)).collect();
    assert!(
        names.iter().any(|name| name == "padding")
            && names.iter().any(|name| name == "MOST_ROUTE_PARAMETERS")
    );
    let body: String = names.iter().flat_map(|name| ["    _ = ", name.as_str(), "\n"]).collect();
    let problems = crate::problems_as_written(&format!("def main():\n{body}"));
    let helps = helps(&format!("def main():\n{body}"));
    assert_eq!(problems.len(), names.len(), "{problems:#?}");
    for ((name, problem), help) in names.iter().zip(&problems).zip(&helps) {
        let private = problem.starts_with(&format!("`{name}` is private to "));
        // `open` says first what Python's `open(path)` is in Fennec, and then how to import std's.
        let imported = problem.starts_with(&format!("`{name}` doesn't exist"))
            && (help.starts_with("import it") || help.contains(&format!(" import {name}`")));
        assert!(private || imported, "{name}: {problem} / {help}");
    }
}

/// A std module's function is keyed `module.name`, and a core file's own `core.file.name`, which
/// a program's method or a function written inside another can spell: a type `path` with a method
/// `is_device`, or a function `path` with one inside it. Std's code would then call the program's,
/// so each is refused, naming it.
#[test]
fn no_method_or_inner_function_takes_a_name_std_has() {
    let said = |key: &str, rest: &str| format!("`{key}` is the standard library's own name, so {rest}");
    for (source, problem) in [
        (
            "struct path:\n    name: str\n\n    def is_device(name: str) -> bool:\n        return False\n",
            said("path.is_device", "a type called `path` can't have a method called `is_device`"),
        ),
        (
            "struct json:\n    name: str\n\n    def quoted(text: str) -> str:\n        return text\n",
            said("json.quoted", "a type called `json` can't have a method called `quoted`"),
        ),
        (
            "def path(name: str) -> bool:\n    def is_device(name: str) -> bool:\n        return False\n    return is_device(name)\n\n\
             def main(stdout: Stdout):\n    stdout.print(path(\"a\"))\n",
            said("path.is_device", "a function inside `path` can't be called `is_device`"),
        ),
        (
            "struct core:\n    name: str\n\n    def str(self, ref out: StrWriter):\n        \
             def padding(fill: str, count: int) -> str:\n            return fill\n        out.write(padding(\"x\", count=1))\n\n\
             def main(stdout: Stdout):\n    stdout.print(core(name=\"n\"))\n",
            said("core.str.padding", "a function inside `core.str` can't be called `padding`"),
        ),
    ] {
        let found = crate::problems_as_written(source);
        assert!(found.iter().any(|found| found.starts_with(&problem)), "{source}: {found:?}");
    }
    let apart = "from std.path import Path\n\nstruct path:\n    name: str\n\n    def shown(self) -> str:\n        return self.name\n\n\
        def main(stdout: Stdout):\n    stdout.print(path(name=\"p\").shown(), Path(\"a\"))\n";
    assert_eq!(crate::problems_as_written(apart), Vec::<String>::new());
}

#[test]
fn reading_gives_back_a_result_of_the_type_asked_for() {
    let program = "
        stdout.print(loads[int]('1') + 1)
        stdout.print(loads[dict[str, int]]('a = 1')? + 1)
        stdout.print(loads[int]('1') else 0)
    ";
    let unhandled = "this is the result of something that can fail @ loads[int]('1') + 1".replace('\'', "\"");
    assert_eq!(
        with_imports("from std.json import loads", program),
        [unhandled.as_str(), "can't use `+` on a dict and an int @ loads[dict[str, int]](\"a = 1\")? + 1"]
    );
    assert_eq!(
        with_imports("from std.toml import loads", program),
        [
            unhandled.as_str(),
            "a TOML document is a table, so it's read as a dict or a struct, not an int @ int",
            "can't use `+` on a dict and an int @ loads[dict[str, int]](\"a = 1\")? + 1",
            "a TOML document is a table, so it's read as a dict or a struct, not an int @ int",
        ]
    );
}

#[test]
fn reading_needs_a_type_the_format_can_hold() {
    let program = "
        a = loads('1')
        b = loads[set[int]]('[1]')
        c = loads[dict[int, str]]('{}')
        d = loads[Id[Player]]('1')
        e = loads[Player]('{}')
        f = loads[3]('1')
    ";
    let declarations = "from std.json import loads\nfrom std.collections import Id\n\nstruct Player:\n    name: str\n    friends: list[Player]\n";
    assert_eq!(
        with_imports(declarations, program),
        [
            "`json.loads` needs the type to read @ loads('1')".replace('\'', "\"").as_str(),
            "JSON can't hold a set @ set[int]",
            "write the type to read in brackets, like `json.loads[Settings]` @ 3",
        ]
    );
}

#[test]
fn a_field_the_format_cant_hold_is_named() {
    let program = "
        a = dumps(Hooks(name='a'))
        b = loads[Player]('{}')
        shared account = Account()
        c = dumps(account)
    ";
    let declarations = "from std.json import dumps, loads\n\nstruct Hooks:\n    name: str\n    \
                        tags = set[str]()\n\nstruct Account:\n    total = 0\n\nstruct Player:\n    \
                        name: str\n    account: shared Account\n";
    assert_eq!(
        with_imports(declarations, program),
        [
            "JSON can't hold `Hooks.tags`, which is a set @ dumps(Hooks(name=\"a\"))",
            "JSON can't hold `Player.account`, which is a `shared Account` @ Player",
            "JSON can't hold a `shared Account` @ dumps(account)",
        ]
    );
}

/// An optional of an optional can't be saved: `null` can't tell `None` from an optional holding
/// `None`. That's so as a value and as a field.
#[test]
fn an_optional_of_an_optional_cant_be_saved() {
    let declarations = "from std.json import dumps\n\nstruct Kept:\n    held: int??\n\n\
                        def shown(value: int??) -> str:\n    return dumps(value)\n";
    let program = "
        inner: int? = None
        a = dumps(Kept(held=inner))
        b = shown(inner)
    ";
    assert_eq!(
        with_imports(declarations, program),
        [
            "JSON can't hold an optional of an optional @ dumps(value)",
            "JSON can't hold `Kept.held`, which is an optional of an optional @ dumps(Kept(held=inner))",
        ]
    );
}

/// A type parameter bounded by `Serializable` is written as JSON in the generic body, and one
/// bounded by `Deserializable` read, as is a list of one: each call's own type is checked against the
/// format where the call is, a call made with the caller's own type parameter at the caller's calls.
/// One with no bound, or with the other bound, could be anything.
#[test]
fn a_type_parameter_bounded_to_be_saved_is_written_and_read() {
    let declared = "from std import json\n\nstruct Account:\n    name: str\n\n\
        def answer[T: Serializable](status: int, value: T) -> str:\n    return f\"{status} {json.dumps(value)}\"\n\n";
    let fine = "def many[T: Serializable](values: list[T]) -> str:\n    return json.dumps(values)\n\n\
        def back[T: Deserializable](text: str) -> T fails ParseError:\n    return json.loads[T](text)?\n\n\
        def main(stdout: Stdout) fails ParseError:\n    stdout.print(answer(201, value=Account(name=\"ash\")), many([1]))\n    \
        account = back[Account](\"{}\")?\n    stdout.print(account.name)\n    return\n";
    assert_eq!(problems(&format!("{declared}{fine}")), Vec::<String>::new());
    let refused = "def anything[T](value: T) -> str:\n    return json.dumps(value)\n\n\
        def other_way[T: Deserializable](value: T) -> str:\n    return json.dumps(value)\n\n\
        def read_any[T](text: str) -> T fails ParseError:\n    return json.loads[T](text)?\n\n\
        def passes[U: Serializable](value: U) -> str:\n    return answer(1, value=value)\n\n\
        def main(stdout: Stdout):\n    stdout.print(answer(200, value=set[int]()), passes(set[str]()))\n    return\n";
    assert_eq!(
        problems(&format!("{declared}{refused}")),
        [
            "JSON can't hold a T @ json.dumps(value)",
            "JSON can't hold a T @ json.dumps(value)",
            "JSON can't hold a T @ T",
            "JSON can't hold a set @ answer(200, value=set[int]())",
            "JSON can't hold a set @ passes(set[str]())",
        ]
    );
}

/// A page is made only by `html.render`, so neither it nor a field holding one is ever loaded,
/// which would make one from text it never saw.
#[test]
fn a_page_is_never_loaded() {
    let program = "
        a = loads[Html]('{}')
        b = loads[Article]('{}')
    ";
    let declarations =
        "from std.json import loads\nfrom std.html import Html\n\nstruct Article:\n    body: Html\n";
    assert_eq!(
        with_imports(declarations, program),
        ["JSON can't hold an `Html` @ Html", "JSON can't hold `Article.body`, which is an `Html` @ Article",]
    );
}

/// A capability is handed down from `main`, never made: loading one from text would make one nobody
/// handed over. Neither one nor anything holding one is saved or loaded, by JSON, TOML or pack, as a
/// `Secret` isn't.
#[test]
fn a_capability_is_never_saved_or_loaded() {
    let program = "
        a = json.loads[Fs]('{}')
        b = json.loads[Holder]('{}')
        c = toml.loads[Outputs]('')
        d = pack.loads[Reaches](bytes([]))
        e = json.dumps(stdout)
    ";
    let declarations = "from std import json, pack, toml\n\n\
                        struct Holder:\n    files: Fs\n\n\
                        struct Outputs:\n    out: list[Stdout]\n\n\
                        struct Reaches:\n    net: Net?\n";
    assert_eq!(
        with_imports(declarations, program),
        [
            "JSON can't hold an `Fs` @ Fs",
            "JSON can't hold `Holder.files`, which is an `Fs` @ Holder",
            "TOML can't hold `Outputs.out`, which is a `Stdout` @ Outputs",
            "`Reaches` isn't a `Deserializable`: `Reaches.net` is a `Net`, which can't be saved @ pack.loads[Reaches](bytes([]))",
            "JSON can't hold a `Stdout` @ json.dumps(stdout)",
        ]
    );
}

/// What the runtime keeps open for this run, a file, a channel's end, a database, is never saved or
/// loaded, nor anything holding one, through a field, a collection or a generic struct: std marks
/// each such field `@handle`, and the checker carries it to whatever holds it. Before, loading a
/// `File` from `{"_handle": 1}` checked, and only native code refused it, as not built yet.
#[test]
fn what_holds_a_handle_is_never_saved_or_loaded() {
    let program = "
        a = json.loads[File]('{}')
        b = json.loads[Feed[int]]('{}')
        c = toml.loads[Kept]('')
        d = pack.loads[Pooled](bytes([]))
    ";
    let declarations = "from std import json, pack, toml\nfrom std.concurrent import Borrowed, Sender\n\
                        from std.fs import File\nfrom std.sqlite import Database\n\n\
                        struct Feed[T]:\n    out: Sender[T]\n\n\
                        struct Kept:\n    open: list[Database]\n\n\
                        struct Pooled:\n    lent: Borrowed[int]?\n\n\
                        def shown(files: list[File]) -> str:\n    return json.dumps(files)\n";
    let held = "kept open by the runtime for this run";
    assert_eq!(
        with_imports(declarations, program),
        [
            format!("JSON can't hold a `File`, {held} @ json.dumps(files)"),
            format!("JSON can't hold a `File`, {held} @ File"),
            format!("JSON can't hold `Feed.out`, which is a `Sender`, {held} @ Feed[int]"),
            format!("TOML can't hold `Kept.open`, which is a `Database`, {held} @ Kept"),
            format!(
                "`Pooled` isn't a `Deserializable`: `Pooled.lent` is a `Borrowed`, {held}, which can't be saved @ \
                 pack.loads[Pooled](bytes([]))"
            ),
        ]
    );
    // Std's own types that hold one, unmarked themselves, are refused through what they hold: a
    // pool through its channel's end, Postgres's database through its connection.
    let nested = "
        a = json.loads[Pool[int]]('{}')
        b = json.loads[PgDatabase]('{}')
    ";
    let imports =
        "from std import json\nfrom std.concurrent import Pool\nfrom std.postgres import PgDatabase\n";
    assert_eq!(
        with_imports(imports, nested),
        [
            format!("JSON can't hold `Pool._notes`, which is a `Sender`, {held} @ Pool[int]"),
            format!("JSON can't hold `PgDatabase._conn`, which is a `Connection`, {held} @ PgDatabase"),
        ]
    );
    // A program's own field can't be marked: it's an annotation that doesn't exist there.
    let marked = "struct Ticket:\n    @handle\n    _number: int\n";
    let said = problems(marked);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("there's no annotation called `@handle`"), "{said:?}");
}

/// A database URL written in the source can't hold its password, which would be in a plain `str`
/// and in the source too. Without one, or as a `Secret`, it's fine.
#[test]
fn a_database_url_written_with_its_password_is_refused() {
    let program = "
        a = postgres.settings('postgres://ann:pw@db/shop')
        b = settings(url='postgres://ann:pw@db/shop')
        c = postgres.settings('postgres://ann@db/shop')
        d = postgres.settings(Secret.of('postgres://ann:pw@db/shop'))
    ";
    let imports =
        "from std import postgres\nfrom std.crypto import Secret\nfrom std.postgres import settings\n";
    let refused = "this database URL has its password written in the source @ \"postgres://ann:pw@db/shop\"";
    assert_eq!(with_imports(imports, program), [refused, refused]);
}

/// A plain struct reads through a Postgres query with only `std.postgres` imported, as it does
/// through `std.sqlite`'s: importing either turns on the saving and loading generated for it.
#[test]
fn a_struct_reads_through_a_postgres_query_with_nothing_else_imported() {
    let source = "
        from std import postgres
        from std.crypto import Secret

        struct Entry:
            path: str
            size: int

        def main(net: Net, entropy: Entropy, stdout: Stdout):
            url = Secret.of(\"postgres://ann:pw@db/shop\")
            with postgres.connect(net, entropy, url)? as db:
                with db.transaction()? as tx:
                    rows = tx.query[Entry](t\"select path, size from entries\")?
                    stdout.print(len(rows))
    ";
    assert_eq!(problems(&crate::dedent(source)), Vec::<String>::new());
}

/// Every case of every enum and error in the standard library is capitalized, as language.md says
/// a case is, so an option reads like any other case: `extra=.Ignore`, `names=.CamelCase`.
#[test]
fn std_cases_are_capitalized() {
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/src/stdlib");
    let mut lower = Vec::new();
    let mut read = 0;
    for entry in std::fs::read_dir(&folder).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "fen") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let module = fennec_syntax::parse_prelude(&source).unwrap();
        read += 1;
        for item in &module.items {
            if let fennec_syntax::Item::Enum(def) | fennec_syntax::Item::Error(def) = item {
                for case in &def.cases {
                    if !case.name.starts_with(|first: char| first.is_ascii_uppercase()) {
                        lower.push(format!("{}.{}", def.name, case.name));
                    }
                }
            }
        }
    }
    assert!(read > 20, "the standard library's files weren't found in {}", folder.display());
    assert_eq!(lower, Vec::<String>::new());
}

#[test]
fn json_and_toml_are_called_the_right_way() {
    let program = "
        a = loads[int](1)
        b = dumps()
        c = dumps([1], indent='2')
        d = dumps([1], width=2)
        e = dumps({1, 2})
        f = dumps([1], indent=2)
    ";
    assert_eq!(
        with_imports("from std.json import loads, dumps", program),
        [
            "the text to read should be a str, but this is an int @ 1",
            "`json.dumps` was called with the wrong arguments @ dumps()",
            "`indent` should be an int, but this is a str @ \"2\"",
            "`json.dumps` has no parameter called `width` @ 2",
            "JSON can't hold a set @ dumps({1, 2})",
        ]
    );
    let program = "
        a = dumps(1)
        b = dumps({'a': 1}, indent=2)
        c = dumps({'a': 1})
    ";
    assert_eq!(
        with_imports("from std.toml import dumps", program),
        [
            "a TOML document is a table, so `toml.dumps` needs a dict or a struct, not an int @ 1",
            "`toml.dumps` has no parameter called `indent` @ 2",
        ]
    );
}

#[test]
fn paths_have_their_methods() {
    let program = "
        p = Path('saves') / 'slot1.json'
        stdout.print(p.name() + 1, p.parent().text, p.extension(), p.stem(), p.normalize())
        stdout.print(p.join(1), Path(1), p.size())
    ";
    assert_eq!(
        with_imports("from std.path import Path", program),
        [
            "can't use `+` on a str and an int @ p.name() + 1",
            "`part` should be a str, but this is an int @ 1",
            "`text` should be a str, but this is an int @ 1",
            "`Path` has no method `size` @ p.size()",
        ]
    );
}

#[test]
fn modules_are_imported_whole() {
    let program = "
        stdout.print(json.dumps(json.loads[int]('1')?), toml.dumps({'a': 1}), path.Path('a').name())
        stdout.print(math.sqrt(2.0) + math.PI)
    ";
    let imports = "from std import json, toml, path, math";
    assert_eq!(with_imports(imports, program), Vec::<String>::new());
    let program = "
        stdout.print(json.parse('1'), json, json.dumps(1) + 1, path.Path(1))
    ";
    assert_eq!(
        with_imports("from std import json, path", program),
        [
            "`std.json` has nothing called `parse` @ json.parse",
            "`json` is a module, so use one of its names, like `json.loads` @ json",
            "can't use `+` on a str and an int @ json.dumps(1) + 1",
            "`text` should be a str, but this is an int @ 1",
        ]
    );
}

#[test]
fn imports_bind_names_the_way_python_does() {
    // `import std.json` binds `std`; `from std import toml` binds `toml`.
    let program = "stdout.print(std.json.dumps(1), toml.dumps({'a': 1}), loads[int]('2')?)";
    let imports = "import std.json\nfrom std import toml\nfrom std.json import loads";
    assert_eq!(with_imports(imports, program), Vec::<String>::new());
    let program = "stdout.print(json.dumps(1), std.toml.dumps(1), std.json, std)";
    assert_eq!(
        with_imports("import std.json", program),
        [
            "`json` doesn't exist @ json",
            "`std.toml` isn't imported here @ std.toml",
            "`std.json` is a module, so use one of its names, like `std.json.loads` @ std.json",
            "`std.json` is a module, so use one of its names, like `std.json.loads` @ std",
        ]
    );
}

#[test]
fn imports_name_modules_and_names_that_exist() {
    let main = "stdout.print(1)";
    assert_eq!(with_imports("import std", main), ["import a module from std, not std itself @ import std"]);
    assert_eq!(
        with_imports("from std import jsn", main),
        ["there's no module called `std.jsn` @ from std import jsn"]
    );
    assert_eq!(
        with_imports("from std.json import parse", main),
        ["`std.json` has nothing called `parse` @ from std.json import parse"]
    );
}

/// One unknown module among several on an import line is the one problem: the others are
/// imported, and the unknown name isn't reported again where it's used. A name a letter or two
/// off a module's is taken to mean it.
#[test]
fn an_unknown_module_in_an_import_is_reported_once() {
    let main = "stdout.print(json.dumps(1), compres.gzip('x'), json.dumps(2), time.Duration(nanos=1))";
    assert_eq!(
        with_imports("from std import compres, json, time", main),
        ["there's no module called `std.compres` @ from std import compres, json, time"]
    );
    assert_eq!(
        helps(
            "from std import compres, json\n\ndef main(stdout: Stdout):\n    stdout.print(json.dumps(1))\n"
        ),
        ["did you mean `std.compress`?"]
    );
}

#[test]
fn as_binds_an_import_under_another_name() {
    let program = "stdout.print(read_json[int]('1')?, read_toml[dict[str, int]]('a = 1')?, j.dumps(1), t.dumps({'a': 1}))";
    let imports = "from std.json import loads as read_json\nfrom std.toml import loads as read_toml\nimport std.json as j\nfrom std import toml as t";
    assert_eq!(with_imports(imports, program), Vec::<String>::new());
    let program = "stdout.print(loads, json.dumps(1), std.json.dumps(1))";
    assert_eq!(
        with_imports("from std.json import loads as read\nimport std.json as j", program),
        ["`loads` doesn't exist @ loads", "`json` doesn't exist @ json", "`std` doesn't exist @ std",]
    );
    assert_eq!(
        with_imports("from std.collections import Deque as Queue", "stdout.print(1)"),
        [
            "`as` can't rename a type from the standard library yet, like `Deque` @ from std.collections import Deque as Queue"
        ]
    );
}

#[test]
fn a_module_the_library_imports_isnt_a_name_in_programs() {
    // `std.math` imports `math` for itself, which a program doesn't see.
    let text = "def main(stdout: Stdout):\n    stdout.print(math.sqrt(4.0))\n";
    assert_eq!(problems(text), ["`math` doesn't exist @ math"]);
}

/// A program about to be lowered is checked as far as it reaches, and also through what
/// lowering calls where the program names nothing: a type's own `str` when it's printed, and
/// `StrWriter`'s methods, which that `str` is given.
#[test]
fn a_program_to_emit_is_checked_through_what_lowering_calls() {
    let source = "from std.collections import Deque\n\n\
        def main(stdout: Stdout):\n    queue = Deque.of([1, 2])\n    stdout.print(f\"{queue}\")\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_to_run(&mut module).unwrap();
    let signatures = fennec_check::check_to_emit(&module).unwrap();
    for implicit in ["Deque.str", "StrWriter.text"] {
        assert!(signatures.get(implicit).is_some(), "{implicit} isn't checked");
    }
    // What nothing reaches, and lowering never calls on its own, isn't.
    assert!(signatures.get("Deque.appendleft").is_none());
}

/// A program a source is written into, like `with launch(...)`, is checked again after it's
/// written in over just what it reaches, as the first round checked it, not all of the library.
#[test]
fn a_program_checked_again_after_a_launch_is_written_in_reaches_no_more_of_the_library() {
    let source = "from std.collections import Deque\nfrom std.concurrent import Crashed\n\n\
        def tick(stdout: Stdout):\n    stdout.print(\"tick\")\n\n\
        def main(stdout: Stdout) fails Crashed:\n    mut queue = Deque.of([1, 2])\n    stdout.print(queue.popleft())\n    \
        with launch(() => tick(stdout))?:\n        stdout.print(\"ticking\")\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    let signatures = fennec_check::check_to_run(&mut module).unwrap();
    assert!(signatures.get("intrinsics.launch").is_some(), "launch isn't written in");
    assert!(signatures.get("Deque.popleft").is_some());
    assert!(
        signatures.get("Deque.appendleft").is_none(),
        "the round after launch checked all of the library"
    );
}

/// `std.concurrent`'s channels check, and `with channel[Job](size=4) as jobs:`, a generic source,
/// is written in where it's used with `Job` for its `T`, so what runs is the source's own body. A
/// `Channel` is only made that way: its constructor's private.
/// A leading-dot case whose name more than one enum has is written in with its enum's name, which
/// the program needn't have imported: `.Error` for a `LogLevel`, beside an enum of the program's
/// own with an `Error` case, still checks once resolved, as a build checks it again.
#[test]
fn a_case_two_enums_share_is_named_by_an_enum_the_program_never_imported() {
    let source = "from std import log\n\nenum Mood:\n    Fine\n    Error\n\n\
        def main(stdout: Stdout):\n    lines = log.Log(out=stdout, level=.Debug)\n    \
        stdout.print(lines.enabled(.Error))\n    mood: Mood = .Error\n    stdout.print(mood)\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    assert_eq!(fennec_check::check_to_emit(&module).err(), None);
}

#[test]
fn a_channel_opens_through_its_generic_source() {
    let source = "from std.concurrent import Closed, Inbox, Sender, Timeout, channel\n\n\
        enum Job:\n    Build(name: str)\n    Stop\n\n\
        def drain(inbox: Inbox[Job]) -> int:\n    mut count = 0\n    for job in inbox:\n        match job:\n            \
        case .Build(name):\n                count += len(name)\n            case .Stop:\n                return count\n    return count\n\n\
        def main(stdout: Stdout) fails Closed | Timeout:\n    with channel[Job](size=4) as jobs:\n        \
        sender: Sender[Job] = jobs.sender()\n        sender.send(.Build(name=\"core\"))?\n        jobs.send_within(.Stop, 1s)?\n        \
        stdout.print(drain(jobs.inbox()))\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    // What's written in checks as it stands, as a build checks it again once trimmed.
    assert_eq!(fennec_check::check(&module).err(), None);
    let mut made = Vec::new();
    fennec_syntax::visit::exprs_mut(&mut module, &mut |expr| {
        if let fennec_syntax::ExprKind::Name(name) = &expr.kind
            && name.contains("channel_")
        {
            made.push(name.clone());
        }
    });
    for written in ["channel_new", "channel_close", "channel_free"] {
        assert!(made.iter().any(|name| name.ends_with(written)), "{made:?}");
    }
    assert_eq!(
        problems(
            "from std.concurrent import channel\n\ndef main():\n    with channel[int, str](size=1) as jobs:\n        pass\n"
        ),
        ["`channel` takes 1 type in brackets, but this gives 2 @ channel[int, str](size=1)"]
    );
    let made = problems(
        "from std.concurrent import Channel\n\ndef main():\n    jobs = Channel[int](_channel=1)\n    _ = jobs\n",
    );
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].contains("`_channel`"), "{made:?}");
}

/// A channel to `.Every` has no one next message to take, since each listener has a place of its
/// own in what's kept, so `receive` and `receive_within` on the name it's lent as are refused where
/// `to=.Every` is written out: in the block, in a function written inside it, and in an arrow.
/// Looping over it, listening, its inbox, sending, a work queue's receive, and a `to` that isn't
/// written out are all fine, and so is the channel handed to a function, which the run-time stop
/// catches.
#[test]
fn a_plain_receive_from_a_channel_to_every_listener_is_refused() {
    let imports = "from std import concurrent\nfrom std.concurrent import Channel, Closed, Delivery, Timeout, channel\n\n";
    let refused = "def main(stdout: Stdout) fails Closed | Timeout:\n    \
        with channel[int](size=8, to=.Every) as hits:\n        hits.send(1)?\n        \
        stdout.print(hits.receive() else -1, hits.receive_within(1ms)? else -1)\n        \
        def later() -> int:\n            return hits.receive() else -1\n        stdout.print(later())\n    \
        with concurrent.channel[int](size=8, to=.Every) as news, channel[int](size=8) as jobs:\n        \
        heard = () => news.receive() else -1\n        stdout.print(heard(), jobs.receive() else -1)\n";
    let said = |name: &str, at: &str| {
        format!("`{name}` is a channel to every listener, so it has no one next message to take @ {at}")
    };
    assert_eq!(
        problems(&format!("{imports}{refused}")),
        [
            said("hits", "hits.receive()"),
            said("hits", "hits.receive_within(1ms)"),
            said("hits", "hits.receive()"),
            said("news", "news.receive()"),
        ]
    );
    let help =
        "loop over it with `for message in hits`, or take an inbox with `hits.inbox()` and receive from that";
    assert_eq!(helps(&format!("{imports}{refused}")).first().map(String::as_str), Some(help));
    let fine = "def take(queue: Channel[int]) -> int:\n    return queue.receive() else -1\n\n\
        def main(stdout: Stdout) fails Closed | Timeout:\n    \
        with channel[int](size=8, to=.Every) as hits:\n        box = hits.inbox()\n        hits.send(1)?\n        \
        stdout.print(box.receive() else -1, box.receive_within(1ms)? else -1, take(hits))\n        \
        for hit in hits:\n            stdout.print(hit)\n        for hit in hits.listen():\n            stdout.print(hit)\n    \
        with channel[int](size=8) as jobs:\n        stdout.print(jobs.receive() else -1, jobs.receive_within(1ms)? else -1)\n    \
        kind: Delivery? = None\n    with channel[int](size=8, to=kind) as plain:\n        stdout.print(plain.receive() else -1)\n";
    assert_eq!(problems(&format!("{imports}{fine}")), Vec::<String>::new());
}

/// A static method called through a generic type with its type written: the type's parameters
/// come from the brackets, and a wrong count says so.
#[test]
fn a_static_method_takes_its_types_from_the_type_written() {
    let boxes = "struct Boxed[T]:\n    items: list[T]\n\n    def empty() -> Boxed[T]:\n        return Boxed[T](items=[])\n\n";
    let empty: [&str; 0] = [];
    assert_eq!(
        problems(&format!(
            "{boxes}def main(stdout: Stdout):\n    held = Boxed[int].empty()\n    stdout.print(len(held.items))\n"
        )),
        empty
    );
    assert_eq!(
        problems(&format!(
            "{boxes}def main(stdout: Stdout):\n    held = Boxed[int, str].empty()\n    stdout.print(len(held.items))\n"
        )),
        ["`Boxed` takes 1 type in brackets, but this gives 2 @ int, str"]
    );
}

/// `with launch(...)` checks, and is written in as a source around `std.intrinsics`' `launch` and
/// `launch_end`, with and without an inbox: what's written in checks as it stands, as a build
/// checks it again once trimmed.
#[test]
fn with_launch_is_written_in_around_the_launch_intrinsics() {
    let source = "from std.concurrent import Closed, Crashed, Inbox, Timeout\n\nenum Sound:\n    Play(name: str, volume: float)\n    StopAll\n\nerror Muted:\n    why: str\n\ndef audio(inbox: Inbox[Sound], stdout: Stdout) fails Muted:\n    for sound in inbox:\n        match sound:\n            case .Play(name, volume):\n                if volume > 1.0: fail Muted(why=\"too loud\")\n                stdout.print(f\"play {name}\")\n            case .StopAll:\n                stdout.print(\"stop\")\n\ndef tick(stdout: Stdout):\n    stdout.print(\"tick\")\n\ndef main(stdout: Stdout) fails Closed | Crashed | Muted | Timeout:\n    with launch((inbox) => audio(inbox, stdout)?, stop_after=5s, inbox_size=4)? as sounds:\n        sounds.send(.Play(name=\"jump\", volume=0.8))?\n        sounds.send(.StopAll)?\n    with launch(() => tick(stdout))?:\n        stdout.print(\"ticking\")\n";
    let mut module = fennec_syntax::parse(source).unwrap();
    fennec_check::check_and_resolve(&mut module).unwrap();
    assert_eq!(fennec_check::check(&module).err(), None);
    let mut made = Vec::new();
    fennec_syntax::visit::exprs_mut(&mut module, &mut |expr| {
        if let fennec_syntax::ExprKind::Name(name) = &expr.kind
            && name.starts_with("intrinsics.")
        {
            made.push(name.clone());
        }
    });
    for written in
        ["intrinsics.launch", "intrinsics.launch_end", "intrinsics.channel_new", "intrinsics.channel_free"]
    {
        assert!(made.iter().any(|name| name == written), "{made:?}");
    }
}

/// What `with launch` asks of what it's given.
#[test]
fn with_launch_takes_an_arrow_its_inbox_and_its_own_options() {
    let program = |body: &str| {
        problems(&format!(
            "from std.concurrent import Crashed, Inbox\n\ndef work(inbox: Inbox[int]):\n    pass\n\n\
             def plain():\n    pass\n\ndef main():\n{body}"
        ))
    };
    assert_eq!(
        program("    task = launch(() => plain())\n"),
        ["`launch` is only ever written as `with launch(...)` @ launch(() => plain())"]
    );
    assert_eq!(
        program("    with launch(plain)?:\n        pass\n"),
        ["`launch` takes an arrow: the call you'd write anyway @ plain"]
    );
    assert_eq!(
        program("    with launch(() => plain(), inbox_size=4)?:\n        pass\n"),
        ["this task takes no inbox, so it has no `inbox_size=` @ 4"]
    );
    assert_eq!(
        program("    with launch((inbox) => work(inbox), size=4)?:\n        pass\n"),
        ["`launch` takes the task, then `stop_after=` and `inbox_size=` @ 4"]
    );
}

/// A channel's message may be optional: `receive()` gives a `T??`, so a `None` that was sent is
/// kept apart from the channel closing, and a task's inbox may take one too.
#[test]
fn a_channel_message_may_be_optional() {
    let program = "from std.concurrent import Closed, Crashed, Inbox, channel\n\n\
                   def gather(inbox: Inbox[int?]):\n    for got in inbox:\n        _ = got\n\n\
                   def main() fails Crashed:\n    with channel[int?](size=4) as numbers:\n        \
                   held: int?? = numbers.receive()\n        _ = held\n    \
                   with launch((inbox) => gather(inbox))? as to:\n        _ = to\n";
    assert_eq!(problems(program), Vec::<String>::new());
}

/// `tls=` is a yes or no on `connect` and the `Certificate` a server shows on `listen`, and a plain
/// connection can start TLS later.
#[test]
fn tls_is_named_where_a_connection_is_made() {
    let imports = "from std.net import Certificate, Connection, NetError, Server\n\n";
    let good = format!(
        "{imports}def use(net: Net, cert: Certificate, conn: Connection) fails NetError:\n    \
         with net.connect(\"a\", 443, tls=True, timeout=1s)? as _:\n        pass\n    \
         conn.start_tls(\"a\", timeout=1s)?\n    \
         with net.listen(0, tls=None)? as _:\n        pass\n    \
         with net.listen(0, tls=cert)? as _:\n        pass\n"
    );
    let module = fennec_syntax::parse(&good).unwrap();
    assert_eq!(fennec_check::check(&module).err(), None);
    for wrong in ["net.connect(\"a\", 443, tls=cert)?", "net.listen(0, tls=True)?", "net.udp(tls=True)?"] {
        let bad = format!(
            "{imports}def use(net: Net, cert: Certificate) fails NetError:\n    _ = cert\n    with {wrong} as _:\n        pass\n"
        );
        let module = fennec_syntax::parse(&bad).unwrap();
        assert!(fennec_check::check(&module).is_err(), "{wrong}");
    }
}

/// Once lowering rewrites a program, it's checked again, and that check has to pass: the rules
/// about how a program is written were held to it as written. Two `_ =` in one function stay
/// apart, and a `with` source's `else:` changing a `ref` parameter isn't a lost change once it's
/// written into its caller.
#[test]
fn a_program_lowering_rewrote_checks_again() {
    for source in [
        "struct World:\n    squads = list[int]()\n\n    def form(ref self, at: (int, int)) -> int:\n        \
         self.squads.append(at[0])\n        return len(self.squads)\n\n\
         def main(stdout: Stdout):\n    mut world = World()\n    _ = world.form((1, 2))\n    _ = world.form((3, 4))\n    \
         shared count = World()\n    lock count as held:\n        stdout.print(held.squads, world.squads)\n",
        "error DbError:\n    why: str\n\nstruct Log:\n    lines = list[str]()\n\n\
         def transaction(ref log: Log) -> with int fails DbError:\n    lend 1 except:\n        \
         log.lines.append(\"rolled back\")\n        fail DbError(why=\"lost\")\n    pass\n\n\
         def insert(n: int) fails DbError:\n    if n > 1: fail DbError(why=\"duplicate\")\n\n\
         def main(stdout: Stdout) fails DbError:\n    mut log = Log()\n    \
         with transaction(ref log)? as tx:\n        insert(tx + 4)?\n    stdout.print(log.lines)\n",
    ] {
        let mut module = fennec_syntax::parse(source).unwrap();
        let resolved = fennec_check::check_and_resolve(&mut module);
        assert!(resolved.is_ok(), "{:?}", resolved.err());
    }
}

/// `read_line` takes `crlf=`, a yes or no, beside `timeout=`, and `write` waits on the network,
/// so it takes `timeout=` too.
#[test]
fn a_connection_reads_lines_ending_in_crlf_and_writes_with_a_timeout() {
    let good = "from std.net import Connection, NetError\n\n\
        def use(conn: Connection) fails NetError:\n    \
        _ = conn.read_line(crlf=True, timeout=1s)?\n    _ = conn.read_line(timeout=1s, crlf=False)?\n    \
        conn.write(\"hi\", timeout=1s)?\n";
    let module = fennec_syntax::parse(good).unwrap();
    assert_eq!(fennec_check::check(&module).err(), None);
    let bad = "from std.net import Connection, NetError\n\ndef use(conn: Connection) fails NetError:\n    _ = conn.read_line(crlf=1)?\n";
    let module = fennec_syntax::parse(bad).unwrap();
    assert!(fennec_check::check(&module).is_err());
}

/// The handlers the route tests give a router: by the `Request`, then a route's parameters by name.
const HANDLERS: &str = "from std.net import Router, Request, Response\n\nerror Nope:\n    why: str\n\n\
    def show(request: Request, id: int) -> Response:\n    return Response(status=200)\n\n\
    def pair(request: Request, name: str, id: u8) -> Response:\n    return Response(status=200)\n\n\
    def keyed(request: Request, key: int) -> Response:\n    return Response(status=200)\n\n\
    def floaty(request: Request, id: float) -> Response:\n    return Response(status=200)\n\n\
    def bare(id: int) -> Response:\n    return Response(status=200)\n\n\
    def text(request: Request, id: int) -> str:\n    return \"\"\n\n\
    def failing(request: Request, id: int) -> Response fails Nope:\n    \
    fail Nope(why=\"no\")\n\n";

/// The problems in a `main` adding routes to `routes`, after the handlers.
fn routed(body: &str) -> Vec<String> {
    let body: String = body.lines().map(|line| ["    ", line, "\n"].concat()).collect();
    problems(&format!("{HANDLERS}def main(stdout: Stdout):\n    mut routes = Router()\n{body}"))
}

/// `router.get(path, handler)` and the other methods are checked where they're written: the path
/// is written out, starts at `/` and names each parameter once in a whole segment, at most eight,
/// and the handler takes the `Request`, then those parameters, each as text or an integer, and
/// gives back a `Response`. Once it checks, it's written in as a call to `handle`, which checks
/// again.
#[test]
fn a_route_is_checked_against_its_handler_where_its_written() {
    let fine = "routes.get(\"/\", (request) => Response(status=200))\n\
        routes.get(\"/notes/{id}\", show)\n\
        routes.post(\"/pairs/{id}/{name}\", pair)\n\
        routes.put(\"/notes/{id}\", show)\n\
        routes.patch(\"/notes/{id}/{name}\", pair)\n\
        routes.delete(\"/tags/{tag}\", (request, tag) => Response(status=204, body=tag.encode()))\n\
        routes.get(\"/notes/new/{id}\", show)\n";
    assert_eq!(routed(fine), Vec::<String>::new());
    let refused = [
        (
            "routes.get(\"/notes/\" + \"{id}\", show)",
            "a route's path is written out, so it's checked against its handler here @ \"/notes/\" + \"{id}\"",
        ),
        ("routes.get(\"notes/{id}\", show)", "`notes/{id}` doesn't start with `/` @ \"notes/{id}\""),
        (
            "routes.get(\"/notes/x{id}\", show)",
            "`x{id}` in `/notes/x{id}` isn't a parameter @ \"/notes/x{id}\"",
        ),
        ("routes.get(\"/notes/{Id}\", show)", "`{Id}` in `/notes/{Id}` isn't a parameter @ \"/notes/{Id}\""),
        (
            "routes.get(\"/notes/{id}/{id}\", show)",
            "`/notes/{id}/{id}` has `{id}` twice @ \"/notes/{id}/{id}\"",
        ),
        (
            "routes.get(\"/{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}/{i}\", show)",
            "`/{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}/{i}` has 9 parameters, but a route has at most 8 @ \"/{a}/{b}/{c}/{d}/{e}/{f}/{g}/{h}/{i}\"",
        ),
        (
            "routes.get(\"/notes/{id}\", floaty)",
            "`floaty` takes `id` as a float, but a route's parameter is read as text or a number @ floaty",
        ),
        (
            "routes.get(\"/notes/{id}\", bare)",
            "`bare` takes `id` first, but a route's handler takes the `Request` first @ bare",
        ),
        (
            "routes.get(\"/notes/{id}\", text)",
            "`text` gives back a str, but a route's handler gives back a `Response` @ text",
        ),
        (
            "routes.get(\"/notes/{id}\", failing)",
            "`failing` can fail, but a route's handler answers every request it's given @ failing",
        ),
        (
            "routes.get(\"/notes/{id}\", (request, id) => 5)",
            "this arrow gives back an int, but a route's handler gives back a `Response` @ (request, id) => 5",
        ),
        ("routes.get(\"/notes/{id}\", 5)", "a route's handler is a function, but this is an int @ 5"),
        (
            "routes.get(\"/notes/{id}\")",
            "`router.get` takes a route's path and its handler @ routes.get(\"/notes/{id}\")",
        ),
    ];
    for (line, said) in refused {
        assert_eq!(routed(line), [said], "{line}");
    }
    let source = format!(
        "{HANDLERS}def main(stdout: Stdout):\n    mut routes = Router()\n{}",
        fine.lines().map(|line| ["    ", line, "\n"].concat()).collect::<String>()
    );
    let mut module = fennec_syntax::parse(&source).unwrap();
    let resolved = fennec_check::check_and_resolve(&mut module);
    assert!(resolved.is_ok(), "{:?}", resolved.err());
}

/// A route's handler takes its parameters by their names in the path, and two routes on one
/// router in one function don't clash: the same route twice, or a parameter in a place they share
/// under two names. The router is held in a `mut` name, as adding a route changes it.
#[test]
fn a_route_names_its_handlers_parameters_and_clashes_with_none_before_it() {
    assert_eq!(
        routed("routes.get(\"/notes/{id}\", keyed)"),
        [
            "`/notes/{id}` has `{id}`, but `keyed` has no parameter called `id` @ keyed",
            "`keyed` takes `key`, but `/notes/{id}` has no `{key}` @ keyed",
        ]
    );
    assert_eq!(
        routed("routes.get(\"/notes/{id}\", (request) => Response(status=200))"),
        [
            "`/notes/{id}` has `{id}`, but this arrow has no parameter called `id` @ (request) => Response(status=200)"
        ]
    );
    let twice = "routes.get(\"/notes/{id}\", show)\nroutes.get(\"/notes/{key}\", keyed)\n";
    assert_eq!(
        routed(twice),
        ["`GET /notes/{key}` is routed twice: `GET /notes/{id}` is the same route @ \"/notes/{key}\""]
    );
    let renamed = "routes.get(\"/notes/{id}\", show)\nroutes.put(\"/notes/{key}/edit\", keyed)\n";
    assert_eq!(
        routed(renamed),
        [
            "`/notes/{key}/edit` calls the parameter in this place `{key}`, but `/notes/{id}` calls it `{id}` @ \"/notes/{key}/edit\""
        ]
    );
    let apart = "def main(stdout: Stdout):\n    routes = Router()\n    routes.get(\"/notes/{id}\", show)\n";
    assert_eq!(problems(&format!("{HANDLERS}{apart}")), ["`routes` isn't mut @ routes"]);
}
