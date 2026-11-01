//! The standard library modules whose functions exist only once imported: `std.json`,
//! `std.toml` and `std.path`.

use crate::problems;

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
    let declarations = "from std.json import loads\nfrom std.game import Id\n\nstruct Player:\n    name: str\n    friends: list[Player]\n";
    assert_eq!(
        with_imports(declarations, program),
        [
            "`json.loads` needs the type to read @ loads('1')".replace('\'', "\"").as_str(),
            "JSON can't hold a set @ set[int]",
            "JSON can't hold a dict @ dict[int, str]",
            "JSON can't hold an Id @ Id[Player]",
            "write the type to read in brackets, like `json.loads[Settings]` @ 3",
        ]
    );
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
