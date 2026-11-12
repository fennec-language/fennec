//! Fairness without interrupts: code that can run as a parked task, sharing its thread with other
//! tasks, counts its loops' turns, and every [`TICKS`] turns asks the runtime with
//! `fennec_task_tick` whether to let the others run. A task that's only computing then still lets
//! a server's other rounds answer.
//!
//! Which code that is: what a `serve` handler's round reaches. `main` isn't a task, as it runs on
//! the program's own thread, and neither are the ranges `parallel` splits a list into, which run
//! to their end.

use std::collections::{HashMap, HashSet};

use fennec_ir::{Function, InstKind, Operand};

use crate::analysis::callees;
use crate::emit::Emit;

/// How many turns of a function's loops go between ticks.
pub(crate) const TICKS: i64 = 4096;

/// The builtins whose closures run as `parallel`'s ranges of a list, not as tasks.
const RANGES: [&str; 4] = ["parallel.for", "parallel.map", "parallel.sum", "parallel.stream"];

/// The functions that can run as a parked task: each round of a server, and what it reaches.
pub(crate) fn tasks<'p>(functions: &HashMap<&'p str, &'p Function>) -> HashSet<&'p str> {
    let mut waiting: Vec<&'p str> = Vec::new();
    #[expect(clippy::iter_over_hash_type, reason = "it starts a walk whose result is a set")]
    for function in functions.values() {
        let made = made_closures(function);
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            if let InstKind::Builtin { name, args, .. } = &inst.kind
                && name == "parallel.serve"
                && let Some(Operand::Local(task)) = args.get(1)
                && let Some(round) = made.get(&task.0)
            {
                waiting.push(round);
            }
        }
    }
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
        self.module.whole.tasks.contains(self.function.name.as_str())
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
    turns

def counted(limit: int) -> int:
    mut turns = 0
    for n in range(limit):
        turns += n
    turns

def main(net: Net, stdout: Stdout) fails NetError:
    stdout.print(counted(3), [x * 2 for x in parallel([1, 2])])
    with net.listen(0)? as server:
        for conn in parallel(server):
            line = conn.read_line()? else \"\"
            conn.write(f\"{spin(line.len())}\\n\")?
";
        let texts = texts(source)?;
        let ticks = |name: &str| {
            texts.iter().filter(|(function, _)| function == name || function.starts_with(name)).any(
                |(_, text)| {
                    text.split("\ndefine ")
                        .filter(|defined| defined.contains(&format!("@\"f_{name}")))
                        .any(|defined| defined.contains("@fennec_task_tick()"))
                },
            )
        };
        let names: Vec<&str> = texts.iter().map(|(function, _)| function.as_str()).collect();
        assert!(ticks("spin"), "{names:?}");
        assert!(!ticks("counted") && !ticks("main.parallel@"), "{names:?}");
        Ok(())
    }
}
