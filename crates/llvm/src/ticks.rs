//! Fairness without interrupts: code that can run as a parked task, sharing its thread with other
//! tasks, counts its loops' turns, and every [`TICKS`] turns asks the runtime with
//! `fennec_task_tick` whether to let the others run. A task that's only computing then still lets
//! a server's other rounds answer.
//!
//! Which code that is: in a program with a server's rounds (`parallel.serve`) or launched tasks
//! (`intrinsics.launch`), which share the runtime's threads, every closure, every generator, and
//! what they reach. A handler reaches a round as a value, given to `serve` or kept in a `Router`'s
//! list, and a round runs closures builtins are given, like a sort's key, with no call of its own;
//! so rather than follow values, every closure that could be one ticks. `main` isn't a task, as it
//! runs on the program's own thread, and neither are the ranges `parallel` splits a list into,
//! which run to their end. A program with neither keeps its code as it was.

use std::collections::{HashMap, HashSet};

use fennec_ir::{Function, InstKind, Operand, Program};

use crate::analysis::callees;
use crate::emit::Emit;

/// How many turns of a function's loops go between ticks.
pub(crate) const TICKS: i64 = 4096;

/// The builtins whose closures run as `parallel`'s ranges of a list, not as tasks.
const RANGES: [&str; 4] = ["parallel.for", "parallel.map", "parallel.sum", "parallel.stream"];

/// The builtins whose closures share the runtime's threads with each other, so need to make way:
/// a server's rounds and launched tasks.
const SHARING: [&str; 2] = ["parallel.serve", "intrinsics.launch"];

/// The functions that can run as a parked task: in a program with a server's rounds or launched
/// tasks, every closure but `parallel`'s ranges, every generator's resume, every `def str` the
/// runtime calls to print a value, and what they reach. A generator's abandon isn't one: it runs
/// from a queue its thread shares, which a task that made way there would leave to another.
/// Ticking more than runs as a task costs the count, and the call in each loop, which LLVM can't
/// see through; the runtime's tick does nothing outside a task.
pub(crate) fn tasks<'p>(
    program: &'p Program,
    functions: &HashMap<&'p str, &'p Function>,
) -> HashSet<&'p str> {
    let shares =
        functions.values().flat_map(|function| &function.blocks).flat_map(|block| &block.insts).any(
            |inst| matches!(&inst.kind, InstKind::Builtin { name, .. } if SHARING.contains(&name.as_str())),
        );
    if !shares {
        return HashSet::new();
    }
    let generators = program.generators.iter().map(|def| def.resume.as_str());
    let own_texts = program.own_texts.iter().map(|own| own.function.as_str());
    let mut waiting: Vec<&'p str> = functions
        .values()
        .flat_map(|function| closures_run_here(function))
        .chain(generators)
        .chain(own_texts)
        .collect();
    let mut found = HashSet::new();
    while let Some(name) = waiting.pop() {
        let Some((key, function)) = functions.get_key_value(name) else { continue };
        if !found.insert(*key) {
            continue;
        }
        waiting.extend(callees(function));
        waiting.extend(closures_run_here(function));
    }
    found
}

/// The function each local a closure is made into is a closure of.
fn made_closures(function: &Function) -> HashMap<usize, &str> {
    function
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match &inst.kind {
            InstKind::MakeClosure { dest, function, .. } => Some((dest.0, function.as_str())),
            _ => None,
        })
        .collect()
}

/// The closures a function makes that can run in the same task: all but `parallel`'s ranges.
fn closures_run_here(function: &Function) -> impl Iterator<Item = &str> {
    let made = made_closures(function);
    let mut ranges = HashSet::new();
    for inst in function.blocks.iter().flat_map(|block| &block.insts) {
        if let InstKind::Builtin { name, args, .. } = &inst.kind
            && RANGES.contains(&name.as_str())
        {
            ranges.extend(args.iter().filter_map(|arg| match arg {
                Operand::Local(local) => Some(local.0),
                _ => None,
            }));
        }
    }
    made.into_iter().filter(move |(local, _)| !ranges.contains(local)).map(|(_, name)| name)
}

impl Emit<'_, '_, '_> {
    /// Whether this function counts its loops' turns.
    pub(crate) fn ticks(&self) -> bool {
        self.module.reads.task(self.function.name.as_str())
    }

    /// The counter of turns until the next tick, a word in the frame that LLVM keeps in a register.
    pub(crate) fn start_ticks(&mut self) {
        self.line("%ticks = alloca i64");
        self.line(&format!("store i64 {TICKS}, ptr %ticks"));
    }

    /// One more turn of a loop: every `TICKS` turns, the runtime may let other tasks on this
    /// thread run. If this one parked, others have run on its thread, so its stack limit is read
    /// again.
    pub(crate) fn tick(&mut self) {
        self.declare("declare i64 @fennec_task_tick() nounwind");
        let (count, fewer, due) = (self.temp(), self.temp(), self.temp());
        let (ticking, ticked) = (self.label("tick"), self.label("ticked"));
        self.line(&format!("{count} = load i64, ptr %ticks"));
        self.line(&format!("{fewer} = add i64 {count}, -1"));
        self.line(&format!("store i64 {fewer}, ptr %ticks"));
        self.line(&format!("{due} = icmp eq i64 {fewer}, 0"));
        self.line(&format!("br i1 {due}, label %{ticking}, label %{ticked}, !prof !0"));
        self.start_block(&ticking);
        self.line(&format!("store i64 {TICKS}, ptr %ticks"));
        let parked = self.temp();
        self.line(&format!("{parked} = call i64 @fennec_task_tick()"));
        self.line(&format!("br label %{ticked}"));
        self.start_block(&ticked);
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    /// Each function's name and text, lowered with `parallel` lifted, as a backend that runs it asks.
    fn texts(source: &str) -> Result<Vec<(String, String)>, String> {
        let mut module = fennec_syntax::parse(source).map_err(|err| format!("{err:?}"))?;
        let checked = fennec_check::check_and_resolve(&mut module).map_err(|err| format!("{err:?}"))?;
        let options = fennec_ir::Options { outline_parallel: true, ..fennec_ir::Options::default() };
        let (units, _, _) = crate::emit::emit(&fennec_ir::lower_with(&module, &checked, options))?;
        Ok(units.into_iter().map(|unit| (unit.function, unit.text)).collect())
    }

    /// A server's round counts its loops' turns, and so does what it calls. `main`, a function
    /// only `main` calls, and a `parallel` comprehension's range don't.
    #[test]
    fn only_what_a_round_reaches_ticks() -> Result<(), String> {
        let source = "\
from std.net import NetError

def spin(limit: int) -> int:
    mut turns = 0
    while turns < limit:
        turns += 1
    return turns

def counted(limit: int) -> int:
    mut turns = 0
    for n in range(limit):
        turns += n
    return turns

def main(net: Net, stdout: Stdout) fails NetError:
    stdout.print(counted(3), [x * 2 for x in parallel([1, 2])])
    with net.listen(0)? as server:
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            conn.write(f\"{spin(len(line))}\\n\")?
";
        let texts = texts(source)?;
        let ticks = |name: &str| ticking(&texts, name);
        let names: Vec<&str> = texts.iter().map(|(function, _)| function.as_str()).collect();
        assert!(ticks("spin"), "{names:?}");
        assert!(!ticks("counted") && !ticks("main.parallel#"), "{names:?}");
        Ok(())
    }

    /// Whether the function `name`, or one whose name starts with it, counts its loops' turns.
    fn ticking(texts: &[(String, String)], name: &str) -> bool {
        texts.iter().filter(|(function, _)| function == name || function.starts_with(name)).any(
            |(_, text)| {
                text.split("\ndefine ")
                    .filter(|defined| defined.contains(&format!("@\"f_{name}")))
                    .any(|defined| defined.contains("@fennec_task_tick()"))
            },
        )
    }

    /// A handler reaches its round as a value, so in a program with a server every closure ticks,
    /// and what it reaches: a handler given to `serve` by name, one a `Router` keeps in its list,
    /// a sort's key, which a builtin calls with no call of its own, a generator a handler walks, and
    /// a `def str` the runtime calls to print a list's items. A function only `main` calls still
    /// doesn't. The generator's own loop ticks; a turn that yields leaves its resume, and the loop
    /// walking it ticks for that turn.
    #[test]
    fn a_handler_given_to_serve_or_kept_by_a_router_ticks() -> Result<(), String> {
        let source = "\
from std.net import Captured, NetError, Request, Response, Router

struct Spot:
    x: int

    def str(self, ref out: StrWriter):
        mut at = 0
        while at < self.x:
            out.write(\"*\")
            at += 1

struct Ring:
    slots: list[int]

    def iter(self) -> Iterator[int]:
        mut at = 0
        while at < len(self.slots):
            mut skipped = 0
            while skipped < at:
                skipped += 1
            yield self.slots[at] + skipped
            at += 1

def count(limit: int) -> int:
    mut turns = 0
    while turns < limit:
        turns += 1
    return turns

def tally(limit: int) -> int:
    mut turns = 0
    while turns < limit:
        turns += 2
    return turns

def rank(word: str) -> int:
    mut score = 0
    for at in range(len(word)):
        score += at
    return score

def alone(limit: int) -> int:
    mut turns = 0
    for n in range(limit):
        turns += n
    return turns

def respond(request: Request) -> Response:
    ring = Ring(slots=[1, 2])
    mut total = count(len(request.path))
    for slot in ring:
        total += slot
    return Response(status=200, body=f\"{total}\".encode())

def routed(found: Captured) -> Response:
    return Response(status=200, body=f\"{tally(len(found.text_at(0)))}\".encode())

def main(net: Net, stdout: Stdout) fails NetError:
    stdout.print(alone(3), sorted([\"b\", \"a\"], key=(word) => rank(word)), [Spot(x=2)])
    mut router = Router()
    router.handle(\"GET /{name}\", (request, found) => routed(found))
    with net.listen(0)? as first:
        first.serve(respond)
    with net.listen(0)? as second:
        second.serve(router)
";
        let texts = texts(source)?;
        let ticks = |name: &str| ticking(&texts, name);
        let names: Vec<&str> = texts.iter().map(|(function, _)| function.as_str()).collect();
        assert!(ticks("respond") && ticks("count"), "a handler given by name: {names:?}");
        assert!(ticks("tally"), "a router's handler: {names:?}");
        assert!(ticks("rank"), "a sort's key: {names:?}");
        assert!(ticks("Ring.iter"), "a generator: {names:?}");
        assert!(ticks("Spot.str"), "a `def str` the runtime calls: {names:?}");
        assert!(!ticks("alone"), "{names:?}");
        Ok(())
    }

    /// A launched task shares the runtime's threads with others, so it ticks with no server in the
    /// program. In a program with neither, nothing does, closures included.
    #[test]
    fn a_launched_task_ticks_and_a_program_without_tasks_doesnt() -> Result<(), String> {
        let looping = "\
def count(limit: int) -> int:
    mut turns = 0
    while turns < limit:
        turns += 1
    return turns

def rank(word: str) -> int:
    mut score = 0
    for at in range(len(word)):
        score += at
    return score

";
        let launched = format!(
            "from std.concurrent import Crashed\n\n{looping}def main(stdout: Stdout) fails Crashed:\n    stdout.print(rank(\"ab\"))\n    with launch(() => stdout.print(count(3)))?:\n        stdout.print(\"launched\")\n"
        );
        let texts_launched = texts(&launched)?;
        let names: Vec<&str> = texts_launched.iter().map(|(function, _)| function.as_str()).collect();
        assert!(ticking(&texts_launched, "count"), "{names:?}");
        assert!(!ticking(&texts_launched, "rank"), "{names:?}");
        let alone = format!(
            "{looping}def main(stdout: Stdout):\n    stdout.print(count(3), sorted([\"b\", \"a\"], key=(word) => rank(word)))\n"
        );
        let texts_alone = texts(&alone)?;
        let names: Vec<&str> = texts_alone.iter().map(|(function, _)| function.as_str()).collect();
        assert!(!ticking(&texts_alone, "count") && !ticking(&texts_alone, "rank"), "{names:?}");
        Ok(())
    }

    /// A turn of a loop ticks once, on its jump back to its start, however many blocks it goes
    /// through: a `for` loop whose body tests a byte and goes on to its step, and a `while` loop's
    /// `continue`, which is a second way back. Each block numbered before the one jumping to it
    /// ticked once, and a byte loop in a server's round ticked three times a turn.
    #[test]
    fn a_loops_turn_ticks_once() -> Result<(), String> {
        use std::collections::BTreeSet;
        let source = "\
from std.net import NetError

def first_space(words: list[int]) -> int:
    for at in range(len(words)):
        if words[at] == 32:
            return at
    return -1

def odd_total(limit: int) -> int:
    mut turns = 0
    mut total = 0
    while turns < limit:
        turns += 1
        if turns % 2 == 0:
            continue
        total += turns
    return total

def main(net: Net, stdout: Stdout) fails NetError:
    with net.listen(0)? as server:
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            conn.write(f\"{first_space([len(line)])} {odd_total(len(line))}\\n\")?
";
        let texts = texts(source)?;
        // Each unit that calls a function has its own copy of it, so each copy is counted alone.
        let ticks = |name: &str| -> BTreeSet<usize> {
            texts
                .iter()
                .flat_map(|(_, text)| text.split("\ndefine "))
                .filter(|defined| {
                    defined.lines().next().is_some_and(|line| line.contains(&format!("@\"f_{name}\"(")))
                })
                .map(|defined| defined.matches("call i64 @fennec_task_tick()").count())
                .collect()
        };
        assert_eq!(ticks("first_space"), BTreeSet::from([1]), "the `for` loop's step is its one way back");
        assert_eq!(ticks("odd_total"), BTreeSet::from([2]), "a `while` loop's end and its `continue`");
        Ok(())
    }
}
