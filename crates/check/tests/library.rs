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
        crate::helps(
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

/// `std.concurrent`'s channels check, and `with channel[Job](size=4) as jobs:`, a generic source,
/// is written in where it's used with `Job` for its `T`, so what runs is the source's own body. A
/// `Channel` is only made that way: its constructor's private.
#[test]
fn a_channel_opens_through_its_generic_source() {
    let source = "from std.concurrent import Closed, Inbox, Sender, Timeout, channel\n\n\
        enum Job:\n    Build(name: str)\n    Stop\n\n\
        def drain(inbox: Inbox[Job]) -> int:\n    mut count = 0\n    for job in inbox:\n        match job:\n            \
        case .Build(name):\n                count += name.len()\n            case .Stop:\n                return count\n    count\n\n\
        def main(stdout: Stdout) fails Closed | Timeout:\n    with channel[Job](size=4) as jobs:\n        \
        sender: Sender[Job] = jobs.sender()\n        sender.send(.Build(name=\"core\"))?\n        jobs.send(.Stop, timeout=1s)?\n        \
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

/// A static method called through a generic type with its type written: the type's parameters
/// come from the brackets, and a wrong count says so.
#[test]
fn a_static_method_takes_its_types_from_the_type_written() {
    let boxes = "struct Boxed[T]:\n    items: list[T]\n\n    def empty() -> Boxed[T]:\n        Boxed[T](items=[])\n\n";
    let empty: [&str; 0] = [];
    assert_eq!(
        problems(&format!(
            "{boxes}def main(stdout: Stdout):\n    held = Boxed[int].empty()\n    stdout.print(held.items.len())\n"
        )),
        empty
    );
    assert_eq!(
        problems(&format!(
            "{boxes}def main(stdout: Stdout):\n    held = Boxed[int, str].empty()\n    stdout.print(held.items.len())\n"
        )),
        ["`Boxed` takes 1 type in brackets, but this gives 2 @ int, str"]
    );
}

/// `with launch(...)` checks, and is written in as a source around `std.intrinsics`' `launch` and
/// `launch_end`, with and without an inbox: what's written in checks as it stands, as a build
/// checks it again once trimmed.
#[test]
fn with_launch_is_written_in_around_the_launch_intrinsics() {
    let source = "from std.concurrent import Closed, Crashed, Inbox, Timeout\n\nenum Sound:\n    Play(name: str, volume: float)\n    StopAll\n\nerror Muted:\n    why: str\n\ndef audio(inbox: Inbox[Sound], stdout: Stdout) fails Muted:\n    for sound in inbox:\n        match sound:\n            case .Play(name, volume):\n                if volume > 1.0: fail Muted(why=\"too loud\")\n                stdout.print(f\"play {name}\")\n            case .StopAll:\n                stdout.print(\"stop\")\n\ndef tick(stdout: Stdout):\n    stdout.print(\"tick\")\n\ndef main(stdout: Stdout) fails Closed | Crashed | Muted | Timeout:\n    with launch((inbox) => audio(inbox, stdout), stop_after=5s, inbox_size=4)? as sounds:\n        sounds.send(.Play(name=\"jump\", volume=0.8))?\n        sounds.send(.StopAll)?\n    with launch(() => tick(stdout))?:\n        stdout.print(\"ticking\")\n";
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
         self.squads.append(at[0])\n        self.squads.len()\n\n\
         def main(stdout: Stdout):\n    mut world = World()\n    _ = world.form((1, 2))\n    _ = world.form((3, 4))\n    \
         shared count = World()\n    lock count as held:\n        stdout.print(held.squads, world.squads)\n",
        "error DbError:\n    why: str\n\nstruct Log:\n    lines = list[str]()\n\n\
         def transaction(ref log: Log) -> with int fails DbError:\n    lend 1 else:\n        \
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
