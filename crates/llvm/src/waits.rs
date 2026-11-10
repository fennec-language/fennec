//! Which functions may wait, over a whole program: the first step towards compiling tasks into
//! state machines, where every call that may wait becomes a place a task can pause. Only a
//! report for now, to see how much code it would touch.
//!
//! Waiting starts at what a function was handed: a connection, a server or a socket, an HTTP
//! request, a clock's sleep, a `shared` value's lock, or `parallel`, which waits for its
//! workers. Reading a file, standard input or a process waits too, on a pool of threads of its
//! own; those are counted apart. From there it spreads to every caller. A call through a
//! function value may wait when any closure made in the program may, and running a generator
//! on when its resume may.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use fennec_ir::{InstKind, Program};

/// Why a function may wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// It does something that waits itself, named here.
    Itself(String),
    /// It calls this function, which may wait.
    Calls(String),
    /// It calls a function value or runs a generator, and some closure or generator may wait.
    Indirectly,
}

/// What may wait in a program, by function.
#[derive(Debug, Default)]
pub struct Waits {
    /// Every function of the program, by name.
    pub functions: BTreeSet<String>,
    /// The ones that may wait, and why.
    pub marked: BTreeMap<String, Why>,
    /// What each function calls directly, a generator it runs included.
    pub callees: BTreeMap<String, BTreeSet<String>>,
}

impl Waits {
    /// The longest way from a function down to something that waits itself, through calls that
    /// may wait, as the functions on the way. A call back to one already on the way ends it.
    pub fn chain(&self, from: &str) -> Vec<String> {
        let mut memo = HashMap::new();
        self.chain_from(from, &mut BTreeSet::new(), &mut memo)
    }

    fn chain_from(
        &self,
        from: &str,
        on_the_way: &mut BTreeSet<String>,
        memo: &mut HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        if let Some(known) = memo.get(from) {
            return known.clone();
        }
        let mut longest = vec![from.to_owned()];
        if let Some(Why::Calls(_)) = self.marked.get(from) {
            on_the_way.insert(from.to_owned());
            for callee in self.callees.get(from).into_iter().flatten() {
                if on_the_way.contains(callee) || !self.marked.contains_key(callee) {
                    continue;
                }
                let below = self.chain_from(callee, on_the_way, memo);
                if below.len() + 1 > longest.len() {
                    longest = std::iter::once(from.to_owned()).chain(below).collect();
                }
            }
            on_the_way.remove(from);
        }
        memo.insert(from.to_owned(), longest.clone());
        longest
    }
}

/// Whether a builtin waits for something outside the program, and whether only on the pool of
/// threads for blocking reads.
fn waits(name: &str) -> Option<bool> {
    // Closing a socket, asking who's at the other end, listening and binding don't wait.
    let network = [
        "Connection.read",
        "Connection.write",
        "Server._accept",
        "UdpSocket.receive",
        "UdpSocket.send_to",
        "Net._connect",
        "Net.get",
        "Net.post",
        "Net.request",
        "Net.script",
    ];
    // A `parallel` call waits for its ranges, or for connections; what helps a range put its
    // items back or take what it owns doesn't.
    let runs = ["parallel.for", "parallel.map", "parallel.sum", "parallel.serve"];
    // Sending on a channel waits for room, taking from one waits for a message, and the end of a
    // `with launch` waits for its task.
    let tasks = ["intrinsics.channel_send", "intrinsics.channel_take", "intrinsics.launch_take"];
    if network.iter().any(|prefix| name.starts_with(prefix))
        || name == "Clock.sleep"
        || name == "Signals.wait"
        || runs.contains(&name)
        || tasks.contains(&name)
    {
        return Some(false);
    }
    let blocking = ["Fs.", "Stdin.", "Process."];
    blocking.iter().any(|prefix| name.starts_with(prefix)).then_some(true)
}

/// Which functions of `program` may wait, counting blocking reads when `blocking` is set.
pub fn may_wait(program: &Program, blocking: bool) -> Waits {
    let mut waits = Waits::default();
    let mut callees: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indirect: BTreeSet<String> = BTreeSet::new();
    let mut closures: BTreeSet<String> = BTreeSet::new();
    for function in program.functions.iter().chain(&program.constants) {
        waits.functions.insert(function.name.clone());
        for inst in function.blocks.iter().flat_map(|block| &block.insts) {
            match &inst.kind {
                InstKind::Builtin { name, .. } => {
                    if waits_here(name, blocking) && !waits.marked.contains_key(&function.name) {
                        waits.marked.insert(function.name.clone(), Why::Itself(name.clone()));
                    }
                }
                InstKind::Lock { .. } | InstKind::LockAll { .. } => {
                    waits
                        .marked
                        .entry(function.name.clone())
                        .or_insert_with(|| Why::Itself("a lock".to_owned()));
                }
                InstKind::Call { function: callee, .. } | InstKind::Resume { known: Some(callee), .. } => {
                    callees.entry(function.name.clone()).or_default().insert(callee.clone());
                }
                InstKind::CallClosure { .. } | InstKind::Resume { known: None, .. } => {
                    indirect.insert(function.name.clone());
                }
                InstKind::MakeClosure { function: lambda, .. } => {
                    closures.insert(lambda.clone());
                }
                _ => {}
            }
        }
    }
    let resumes: BTreeSet<String> =
        program.generators.iter().map(|generator| generator.resume.clone()).collect();
    // Spread to callers until nothing changes.
    loop {
        let before = waits.marked.len();
        let any_indirect = closures.iter().chain(&resumes).any(|callee| waits.marked.contains_key(callee));
        for name in &waits.functions {
            if waits.marked.contains_key(name) {
                continue;
            }
            let called =
                callees.get(name).into_iter().flatten().find(|callee| waits.marked.contains_key(*callee));
            if let Some(callee) = called {
                waits.marked.insert(name.clone(), Why::Calls(callee.clone()));
            } else if any_indirect && indirect.contains(name) {
                waits.marked.insert(name.clone(), Why::Indirectly);
            }
        }
        if waits.marked.len() == before {
            break;
        }
    }
    waits.callees = callees;
    waits
}

/// Whether a builtin waits, as `blocking` counts it.
fn waits_here(name: &str, blocking: bool) -> bool {
    waits(name).is_some_and(|only_blocking| blocking || !only_blocking)
}
