//! Which types can cross to another task, as the backend reads it from the signatures: a
//! channel's item, what a `launch` captures, gives back and fails with, what `parallel`'s workers
//! read, and what a `shared` value holds, with whatever each holds inside. Each is worked out
//! the same from the program as written and from the program lowering rewrote.

use fennec_check::{Crossing, Signatures, Type};

/// What crosses in a program, as a run checks it and as a build checks it once resolved and
/// trimmed, or the problems either found. Both have to agree on every type named here.
fn crossing(source: &str) -> Result<(Crossing, Crossing), String> {
    let mut module = fennec_syntax::parse(source).map_err(|problem| problem.message)?;
    let shown = |problems: Vec<fennec_syntax::Diagnostic>| format!("{problems:?}");
    let ran = fennec_check::check_to_run(&mut module.clone()).map_err(shown)?.crossing().clone();
    fennec_check::check_and_resolve(&mut module).map_err(shown)?;
    let built = fennec_check::check_to_emit(&module).map_err(shown)?.crossing().clone();
    Ok((ran, built))
}

/// Says each of `crossing_names` crosses and each of `staying` doesn't, in both checks.
fn crosses(source: &str, crossing_names: &[&str], staying: &[&str]) -> Result<(), String> {
    let (ran, built) = crossing(source)?;
    for (which, found) in [("run", &ran), ("build", &built)] {
        assert!(found.worked_out(), "{which}");
        let listed = || found.types().map(ToString::to_string).collect::<Vec<_>>();
        for name in crossing_names {
            assert!(
                found.crosses(&Type::Named((*name).to_owned())),
                "{which}: `{name}` should cross: {:?}",
                listed()
            );
        }
        for name in staying {
            assert!(
                !found.crosses(&Type::Named((*name).to_owned())),
                "{which}: `{name}` shouldn't cross: {:?}",
                listed()
            );
        }
    }
    Ok(())
}

/// A channel's item crosses, and so does what it holds, here in an enum's case; a type the
/// program only keeps to itself doesn't.
#[test]
fn a_channels_item_crosses_with_what_it_holds() {
    let source = "from std.concurrent import Closed, Inbox, Sender, Timeout, channel\n\n\
        struct Part:\n    name: str\n\n\
        struct Tally:\n    count: int\n\n\
        enum Job:\n    Build(part: Part)\n    Stop\n\n\
        def drain(inbox: Inbox[Job]) -> int:\n    mut tally = Tally(count=0)\n    for job in inbox:\n        match job:\n            \
        case .Build(part):\n                tally.count += len(part.name)\n            case .Stop:\n                return tally.count\n    return tally.count\n\n\
        def main(stdout: Stdout) fails Closed | Timeout:\n    with channel[Job](size=4) as jobs:\n        \
        sender: Sender[Job] = jobs.sender()\n        sender.send(.Build(part=Part(name=\"core\")))?\n        jobs.send_within(.Stop, 1s)?\n        \
        stdout.print(drain(jobs.inbox()))\n";
    crosses(source, &["Job", "Part"], &["Tally"]).unwrap();
}

/// What a `launch`'s arrow captures crosses to the task, and what the task fails with crosses
/// back; a value the caller only uses itself stays.
#[test]
fn what_a_launch_captures_and_fails_with_crosses() {
    let source = "from std.concurrent import Crashed\n\n\
        struct Config:\n    name: str\n    limits: list[Limit]\n\n\
        struct Limit:\n    most: int\n\n\
        struct Local:\n    n: int\n\n\
        error Refused:\n    why: Reason\n\n\
        struct Reason:\n    text: str\n\n\
        def work(config: Config) fails Refused:\n    if len(config.limits) > 3: fail Refused(why=Reason(text=\"too many\"))\n\n\
        def main(stdout: Stdout) fails Crashed | Refused:\n    config = Config(name=\"x\", limits=[Limit(most=2)])\n    local = Local(n=1)\n    \
        with launch(() => work(config)?)?:\n        stdout.print(local.n)\n";
    crosses(source, &["Config", "Limit", "Refused", "Reason"], &["Local"]).unwrap();
}

/// A `parallel` loop's items cross to its workers, and so does each name its body reads from
/// around it; what's made after the loop doesn't.
#[test]
fn what_a_parallel_loop_reads_crosses() {
    let source = "struct Unit:\n    hp: int\n\n\
        struct Bonus:\n    amount: int\n\n\
        struct Summary:\n    total: int\n\n\
        def main(stdout: Stdout):\n    bonus = Bonus(amount=2)\n    mut units = [Unit(hp=1), Unit(hp=2)]\n    \
        for ref unit in parallel(units):\n        unit.hp += bonus.amount\n    \
        summary = Summary(total=units[0].hp)\n    stdout.print(summary.total)\n";
    crosses(source, &["Unit", "Bonus"], &["Summary"]).unwrap();
}

/// A `parallel` comprehension's items cross to its workers, with what its element reads from
/// around it, and what it makes comes back from them; a type only the caller keeps doesn't.
#[test]
fn what_a_parallel_comprehension_reads_and_makes_crosses() {
    let source = "struct Cell:\n    hp: int\n\n\
        struct Hit:\n    damage: int\n\n\
        struct Rule:\n    scale: int\n\n\
        struct Note:\n    text: str\n\n\
        def hit(cell: Cell, rule: Rule) -> Hit:\n    return Hit(damage=cell.hp * rule.scale)\n\n\
        def main(stdout: Stdout):\n    rule = Rule(scale=2)\n    cells = [Cell(hp=1), Cell(hp=2)]\n    note = Note(text=\"done\")\n    \
        hits = [hit(cell, rule) for cell in parallel(cells)]\n    stdout.print(len(hits), note.text)\n";
    crosses(source, &["Cell", "Hit", "Rule"], &["Note"]).unwrap();
}

/// What a `shared` value holds crosses: a lock hands it from task to task, with what's inside it.
#[test]
fn what_a_shared_value_holds_crosses() {
    let source = "struct Entry:\n    text: str\n\n\
        struct Book:\n    entries: list[Entry]\n\n\
        def add(book: shared Book, text: str):\n    lock book as ref held:\n        held.entries.append(Entry(text=text))\n\n\
        struct Note:\n    text: str\n\n\
        def main(stdout: Stdout):\n    shared book = Book(entries=[])\n    add(book, \"one\")\n    note = Note(text=\"kept\")\n    \
        lock book as seen:\n        stdout.print(len(seen.entries), note.text)\n";
    crosses(source, &["Book", "Entry"], &["Note"]).unwrap();
}

/// A generic function that sends what it's given makes each type it's called with cross, in the
/// copy for that type, though nothing names the type beside the channel.
#[test]
fn a_type_sent_through_a_generic_function_crosses_in_its_copy() {
    let source = "from std.concurrent import Closed, Sender, Timeout, channel\n\n\
        struct Point:\n    x: int\n\n\
        struct Size:\n    w: int\n\n\
        def send_one[T](to: Sender[T], item: T) fails Closed:\n    to.send(item)?\n\n\
        def main(stdout: Stdout) fails Closed | Timeout:\n    size = Size(w=3)\n    \
        with channel[Point](size=2) as points:\n        send_one(points.sender(), Point(x=size.w))?\n        stdout.print(size.w)\n";
    crosses(source, &["Point"], &["Size"]).unwrap();
}

/// A pool hands its values between its keeper tasks and the blocks that take them, through
/// channels of its own: its `T` crosses, though the program has no channel of its own.
#[test]
fn a_pools_values_cross() {
    let source = "from std.concurrent import Crashed, PoolError, pool\n\n\
        struct Link:\n    number: int\n\n    def reset(self) -> bool:\n        return True\n\n    def close(self):\n        pass\n\n\
        error Refused:\n    why: str\n\n    def message(self) -> str:\n        return self.why\n\n\
        struct Count:\n    n: int\n\n\
        def opened() -> Link fails Refused:\n    return Link(number=1)\n\n\
        def main(stdout: Stdout) fails PoolError | Crashed | Refused:\n    count = Count(n=0)\n    \
        with pool[Link, Refused](open=() => opened()?, size=1)? as links:\n        with links.take()? as link:\n            \
        stdout.print(link.number + count.n)\n";
    crosses(source, &["Link"], &["Count"]).unwrap();
}

/// A function handed to a task crosses with what it captures. One whose making can't be seen
/// where it's launched, a parameter, captures what the arrow functions of its type capture.
#[test]
fn a_function_a_task_runs_takes_what_it_captures_across() {
    let source = "from std.concurrent import Crashed\n\n\
        struct Settings:\n    level: int\n\n\
        struct Other:\n    n: int\n\n\
        def run(step: () -> int) fails Crashed:\n    with launch(() => step())?:\n        pass\n\n\
        def main(stdout: Stdout) fails Crashed:\n    settings = Settings(level=3)\n    other = Other(n=1)\n    \
        run(() => settings.level)?\n    stdout.print(other.n)\n";
    crosses(source, &["Settings"], &["Other"]).unwrap();
}

/// A server runs its handler on worker threads, a task for each connection: what the handler
/// captures crosses, and so do the requests it's handed and the responses it gives back, with
/// what they hold. What the program keeps to itself doesn't.
#[test]
fn a_servers_handler_takes_what_it_captures_and_answers_across() {
    let source = "from std.net import NetError, Request, Response\n\n\
        struct Greeting:\n    text: str\n\n\
        struct Startup:\n    port: int\n\n\
        def main(net: Net, stdout: Stdout) fails NetError:\n    greeting = Greeting(text=\"hello\")\n    startup = Startup(port=0)\n    \
        def respond(request: Request) -> Response:\n        return Response(status=200, body=f\"{greeting.text} {request.path}\".encode())\n    \
        with net.listen(startup.port)? as server:\n        server.serve(respond)\n    stdout.print(startup.port)\n";
    crosses(source, &["Greeting", "Request", "Response", "Server"], &["Startup"]).unwrap();
}

/// What a `parallel` loop's body can fail with crosses: the runtime hands the failure from the
/// worker that met it back to the caller. So does what a task fails with, and a function that runs
/// on another task takes what it fails with across.
#[test]
fn what_a_worker_or_task_fails_with_crosses() {
    let source = "from std.concurrent import Crashed\n\n\
        struct Unit:\n    hp: int\n\n\
        error Dead:\n    why: Cause\n\n\
        struct Cause:\n    text: str\n\n\
        error Late:\n    by: int\n\n\
        error Unused:\n    n: int\n\n\
        def check(unit: Unit) fails Dead:\n    if unit.hp < 0: fail Dead(why=Cause(text=\"hp\"))\n\n\
        def slow() fails Late:\n    fail Late(by=1)\n\n\
        def main(stdout: Stdout) fails Dead | Late | Crashed:\n    mut units = [Unit(hp=1)]\n    \
        for ref unit in parallel(units):\n        check(unit)?\n    \
        with launch(() => slow()?)?:\n        stdout.print(len(units))\n";
    crosses(source, &["Dead", "Cause", "Late"], &["Unused"]).unwrap();
}

/// A program that hands nothing to another task has nothing that crosses, and signatures made
/// without the check that works it out say every type does.
#[test]
fn nothing_crosses_without_tasks_and_everything_without_the_check() {
    let (ran, built) =
        crossing("struct Point:\n    x: int\n\ndef main(stdout: Stdout):\n    stdout.print(Point(x=1).x)\n")
            .unwrap();
    for found in [ran, built] {
        assert!(found.worked_out());
        assert_eq!(found.types().count(), 0, "{:?}", found.types().collect::<Vec<_>>());
    }
    let unknown = Signatures::default();
    assert!(!unknown.crossing().worked_out());
    assert!(unknown.crossing().crosses(&Type::Named("Point".to_owned())));
}
