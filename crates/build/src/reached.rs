//! What a program can reach from `main`, so that running one whose every definition has
//! already passed the checker needn't check, lower and load the rest of it again.
//!
//! This works on whole items, using the same names as `reach`: a type comes with all of its
//! methods and extensions. Reaching too much only costs time. A name it misses makes the
//! program fail to check, and the caller then goes back to the whole program.

use std::collections::{BTreeSet, HashMap, HashSet};

use fennec_syntax::{Item, Module};

use crate::reach::{Uses, is_implicit};

/// What an item defines, and whether it's kept whatever reaches it. What it uses is worked out
/// only once it's reached: most of a big program's items aren't.
#[derive(Default)]
struct Node {
    names: Vec<String>,
    members: Vec<String>,
    root: bool,
}

/// Removes everything `main` can't reach from `module`, tests included.
pub fn keep_what_main_reaches(module: &mut Module) {
    keep_what_functions_reach(module, &|name| name == "main");
}

/// What `main` reaches in `module`, tests left out, as a module of its own: what
/// `keep_what_main_reaches` leaves, without copying what it would let go of.
#[must_use]
pub fn what_main_reaches(module: &Module) -> Module {
    let kept = reached(module, &|name| name == "main");
    let items =
        module.items.iter().enumerate().filter(|(at, _)| kept.contains(at)).map(|(_, item)| item.clone());
    Module { items: items.collect(), std_rewritten: module.std_rewritten.clone() }
}

/// Removes everything that the functions `is_start` names can't reach from `module`, tests
/// included.
pub fn keep_what_functions_reach(module: &mut Module, is_start: &dyn Fn(&str) -> bool) {
    let kept = reached(module, is_start);
    let (reached, unreached): (Vec<_>, Vec<_>) =
        std::mem::take(&mut module.items).into_iter().enumerate().partition(|(at, _)| kept.contains(at));
    module.items = reached.into_iter().map(|(_, item)| item).collect();
    // What isn't reached is most of a big program, and letting it go takes as long as finding
    // what is: it's let go of beside the run rather than before it.
    std::thread::spawn(move || drop(unreached));
}

/// Which of `module`'s items `main` reaches, by where they are in it.
#[must_use]
pub fn reached_by_main(module: &Module) -> HashSet<usize> {
    reached(module, &|name| name == "main")
}

/// The names and members some items define, as reaching goes by them: what a cut-down program
/// left out, so that a program patched from it can tell whether it reaches any of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defined {
    pub names: BTreeSet<String>,
    pub members: BTreeSet<String>,
}

impl Defined {
    /// What `items` define. A test defines nothing, since nothing reaches one.
    pub fn of<'i>(items: impl IntoIterator<Item = &'i Item>) -> Defined {
        let mut defined = Defined::default();
        for node in items.into_iter().filter_map(|item| node(item, &|_| false)) {
            defined.names.extend(node.names);
            defined.members.extend(node.members);
        }
        defined
    }

    /// Adds what `other` defines.
    pub fn extend(&mut self, other: Defined) {
        self.names.extend(other.names);
        self.members.extend(other.members);
    }
}

/// Whether anything `main` reaches in `module` uses a name or member that `absent` defines. When
/// `absent` is what was left out of `module`, and it doesn't, `main` reaches the same in `module`
/// as in all of the program: everything reached uses only what's there.
#[must_use]
pub fn reaches_into(module: &Module, absent: &Defined) -> bool {
    reached_by_main(module).iter().filter_map(|at| module.items.get(*at)).any(|item| {
        let uses = uses(item);
        uses.names.iter().any(|name| absent.names.contains(name))
            || uses.members.iter().any(|member| absent.members.contains(member))
    })
}

/// Every name `items` use, as reaching goes by them.
pub fn names_used<'i>(items: impl IntoIterator<Item = &'i Item>) -> BTreeSet<String> {
    items.into_iter().flat_map(|item| uses(item).names).collect()
}

/// Which of `module`'s items the functions `is_start` names reach, by where they are in it.
fn reached(module: &Module, is_start: &dyn Fn(&str) -> bool) -> HashSet<usize> {
    let nodes: Vec<Option<Node>> = module.items.iter().map(|item| node(item, is_start)).collect();
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_member: HashMap<&str, Vec<usize>> = HashMap::new();
    for (at, node) in nodes.iter().enumerate() {
        let Some(node) = node else { continue };
        node.names.iter().for_each(|name| by_name.entry(name).or_default().push(at));
        node.members.iter().for_each(|name| by_member.entry(name).or_default().push(at));
    }
    let is_root = |(_, node): &(usize, &Option<Node>)| node.as_ref().is_some_and(|node| node.root);
    let mut pending: Vec<usize> = nodes.iter().enumerate().filter(is_root).map(|(at, _)| at).collect();
    let mut kept: HashSet<usize> = pending.iter().copied().collect();
    while let Some(at) = pending.pop() {
        let Some(item) = module.items.get(at) else { continue };
        let uses = uses(item);
        let named = uses.names.iter().filter_map(|name| by_name.get(name.as_str()));
        let members = uses.members.iter().filter_map(|name| by_member.get(name.as_str()));
        for next in named.chain(members).flatten() {
            if kept.insert(*next) {
                pending.push(*next);
            }
        }
    }
    kept
}

/// What an item adds to the graph. A test adds nothing, so it's never kept.
fn node(item: &Item, is_start: &dyn Fn(&str) -> bool) -> Option<Node> {
    let mut node = Node::default();
    match item {
        Item::Function(def) => {
            node.names.push(def.name.clone());
            // What saves and loads a tuple the program saves (see `fennec_check::tuple_function`)
            // is only called from inside std's generic code, which no name in the program reaches.
            node.root = is_start(&def.name) || def.name.starts_with("fennec_tuple_");
        }
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            node.names.push(def.name.clone());
            node.names.extend(def.cases.iter().map(|case| case.name.clone()));
            node.members.extend(def.methods.iter().map(|method| method.name.clone()));
        }
        Item::Interface(interface) => {
            node.names.push(interface.name.clone());
            node.members.extend(interface.methods.iter().map(|method| method.name.clone()));
        }
        // An extension's owner may never be named, as with an operator on a built-in type.
        Item::Extend(extension) => {
            node.names.push(extension.name.clone());
            node.members.extend(extension.methods.iter().map(|method| method.name.clone()));
            node.root = extension.methods.iter().any(|method| is_implicit(&method.name));
        }
        Item::TypeAlias { name, .. } => node.names.push(name.clone()),
        Item::Const { name, .. } => {
            node.names.push(name.clone());
            // A kit's `pack.lock` (see `fennec_kit::PACK_LOCK`) is read by std.pack through an
            // intrinsic, so no name in the program reaches it.
            node.root = name == "\0pack_lock";
        }
        Item::Import(_) | Item::Stmt(_) => node.root = true,
        Item::Test(_) => return None,
    }
    Some(node)
}

/// What a reached item uses, and so reaches too.
fn uses(item: &Item) -> Uses {
    let mut uses = Uses::default();
    match item {
        Item::Function(def) => uses.function(def),
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            uses.type_def(def);
            // Its extensions, which go by its name, as `def serialize` and `def deserialize` do
            // when they're generated: one without the other doesn't check.
            uses.names.insert(def.name.clone());
        }
        Item::Interface(interface) => interface.methods.iter().for_each(|method| uses.function(method)),
        Item::Extend(extension) => {
            extension.methods.iter().for_each(|method| uses.function(method));
            // The type it extends, and with it the type's other extensions.
            uses.names.insert(extension.name.clone());
        }
        Item::TypeAlias { ty, .. } => uses.type_expr(ty),
        Item::Const { ty, value, .. } => {
            if let Some(ty) = ty {
                uses.type_expr(ty);
            }
            uses.expr(value);
        }
        Item::Stmt(stmt) => uses.stmt(stmt),
        Item::Import(_) | Item::Test(_) => {}
    }
    uses
}

#[cfg(test)]
mod tests {
    use super::{Defined, keep_what_main_reaches, reached_by_main, reaches_into, what_main_reaches};

    /// A program cut down to what `main` reaches reaches nothing of what was cut, until something
    /// it keeps names what was cut, by name or as a member.
    #[test]
    fn a_cut_down_program_reaches_into_what_was_cut_only_when_it_names_it() {
        let whole = fennec_syntax::parse(
            "struct Box:\n    size: int\n    def grow(self) -> int:\n        self.size + 1\n\n\
             def unused() -> int:\n    1\n\ndef main():\n    print(2)\n",
        )
        .unwrap();
        let kept = reached_by_main(&whole);
        let cut = whole.items.iter().enumerate().filter(|(at, _)| !kept.contains(at)).map(|(_, item)| item);
        let absent = Defined::of(cut);
        assert!(absent.names.contains("unused") && absent.names.contains("Box"), "{absent:?}");
        assert!(absent.members.contains("grow"), "{absent:?}");
        let module = what_main_reaches(&whole);
        assert!(!reaches_into(&module, &absent));
        let calls = |body: &str| {
            fennec_syntax::parse(&format!("def main():\n    {body}\n"))
                .map(|module| reaches_into(&module, &absent))
        };
        assert_eq!(calls("print(unused())"), Ok(true), "by name");
        assert_eq!(calls("print(Box(size=1).size)"), Ok(true), "a type by name");
        assert_eq!(calls("x = 3\n    print(x.grow())"), Ok(true), "as a member");
        assert_eq!(calls("print(3)"), Ok(false));
    }

    /// The names of the items left after keeping what `main` reaches in `source`, which copying
    /// out what it reaches gives too.
    fn kept(source: &str) -> Vec<String> {
        let mut module = fennec_syntax::parse(source).unwrap();
        let copied = what_main_reaches(&module);
        keep_what_main_reaches(&mut module);
        assert_eq!(copied, module, "copying out what main reaches leaves what cutting the rest away does");
        module.items.iter().map(|item| format!("{:.40}", format!("{item:?}"))).collect()
    }

    fn keeps(source: &str, name: &str) -> bool {
        kept(source).iter().any(|item| item.contains(&format!("name: \"{name}\"")))
    }

    #[test]
    fn only_what_main_reaches_stays() {
        let source = "def used() -> int:\n    1\n\ndef unused() -> int:\n    2\n\n\
                      test \"t\":\n    assert unused() == 2\n\n\
                      def main(stdout: Stdout):\n    stdout.print(used())\n";
        assert!(keeps(source, "main") && keeps(source, "used"));
        assert!(!keeps(source, "unused"));
        assert_eq!(kept(source).len(), 2, "the test goes too");
    }

    #[test]
    fn a_type_stays_with_what_all_of_its_methods_reach() {
        let source = "def helper() -> int:\n    1\n\nstruct Point:\n    x: int\n\n    \
                      def other(self) -> int:\n        helper()\n\n\
                      def main(stdout: Stdout):\n    stdout.print(Point(x=1).x)\n";
        assert!(keeps(source, "Point") && keeps(source, "helper"));
    }

    /// A `shared Point` written in a kept function means the declaration lowering made for it,
    /// `Shared__Point`, which nothing names: it stays with the function, however the function
    /// was reached, here by a local in `main` with the function's name.
    #[test]
    fn the_declaration_a_shared_type_means_stays_with_what_writes_it() {
        let source = "struct Point:\n    x: int\n\nstruct Shared__Point:\n    value: Point\n\n\
                      def change(box: shared Point, by: int):\n    pass\n\n\
                      def main(stdout: Stdout):\n    for change in [1, 2]:\n        stdout.print(change)\n";
        assert!(keeps(source, "change"));
        assert!(keeps(source, "Shared__Point"));
    }

    #[test]
    fn a_type_stays_when_something_names_it() {
        let source = "struct Point:\n    x: int\n\n    def area(self) -> int:\n        self.x\n\n\
                      struct Lonely:\n    x: int\n\n\
                      def make(p: Point) -> int:\n    p.area()\n\n\
                      def main(stdout: Stdout):\n    stdout.print(make(Point(x=1)))\n";
        assert!(keeps(source, "Point") && keeps(source, "make"));
        assert!(!keeps(source, "Lonely"));
    }
}
