//! What a program can reach from `main`, so that running one whose every definition has
//! already passed the checker needn't check, lower and load the rest of it again.
//!
//! This works on whole items, using the same names as `reach`: a type comes with all of its
//! methods and extensions. Reaching too much only costs time. A name it misses makes the
//! program fail to check, and the caller then goes back to the whole program.

use std::collections::{HashMap, HashSet};

use fennec_syntax::{Item, Module};

use crate::reach::{Uses, is_implicit};

/// What an item defines and uses, and whether it's kept whatever reaches it.
#[derive(Default)]
struct Node {
    names: Vec<String>,
    members: Vec<String>,
    uses: Uses,
    root: bool,
}

/// Removes everything `main` can't reach from `module`, tests included.
pub fn keep_what_main_reaches(module: &mut Module) {
    keep_what_functions_reach(module, &|name| name == "main");
}

/// Removes everything that the functions `is_start` names can't reach from `module`, tests
/// included.
pub fn keep_what_functions_reach(module: &mut Module, is_start: &dyn Fn(&str) -> bool) {
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
        let Some(Some(node)) = nodes.get(at) else { continue };
        let named = node.uses.names.iter().filter_map(|name| by_name.get(name.as_str()));
        let members = node.uses.members.iter().filter_map(|name| by_member.get(name.as_str()));
        for next in named.chain(members).flatten() {
            if kept.insert(*next) {
                pending.push(*next);
            }
        }
    }
    let mut at = 0;
    module.items.retain(|_| {
        at += 1;
        kept.contains(&(at - 1))
    });
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
            node.uses.function(def);
        }
        Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
            node.names.push(def.name.clone());
            node.names.extend(def.cases.iter().map(|case| case.name.clone()));
            node.members.extend(def.methods.iter().map(|method| method.name.clone()));
            node.uses.type_def(def);
            // Its extensions, which go by its name, as `def serialize` and `def deserialize` do
            // when they're generated: one without the other doesn't check.
            node.uses.names.insert(def.name.clone());
        }
        Item::Interface(interface) => {
            node.names.push(interface.name.clone());
            node.members.extend(interface.methods.iter().map(|method| method.name.clone()));
            interface.methods.iter().for_each(|method| node.uses.function(method));
        }
        // An extension's owner may never be named, as with an operator on a built-in type.
        Item::Extend(extension) => {
            node.names.push(extension.name.clone());
            node.members.extend(extension.methods.iter().map(|method| method.name.clone()));
            node.root = extension.methods.iter().any(|method| is_implicit(&method.name));
            extension.methods.iter().for_each(|method| node.uses.function(method));
            // The type it extends, and with it the type's other extensions.
            node.uses.names.insert(extension.name.clone());
        }
        Item::TypeAlias { name, ty, .. } => {
            node.names.push(name.clone());
            node.uses.type_expr(ty);
        }
        Item::Const { name, ty, value, .. } => {
            node.names.push(name.clone());
            // A kit's `pack.lock` (see `fennec_kit::PACK_LOCK`) is read by std.pack through an
            // intrinsic, so no name in the program reaches it.
            node.root = name == "\0pack_lock";
            if let Some(ty) = ty {
                node.uses.type_expr(ty);
            }
            node.uses.expr(value);
        }
        Item::Import(_) => node.root = true,
        Item::Stmt(stmt) => {
            node.root = true;
            node.uses.stmt(stmt);
        }
        Item::Test(_) => return None,
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::keep_what_main_reaches;

    /// The names of the items left after keeping what `main` reaches in `source`.
    fn kept(source: &str) -> Vec<String> {
        let mut module = fennec_syntax::parse(source).unwrap();
        keep_what_main_reaches(&mut module);
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
