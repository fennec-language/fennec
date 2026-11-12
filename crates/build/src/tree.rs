//! The second layer of hashing: each definition's syntax tree, with positions and comments
//! left out, so reformatting, editing comments and moving code around change no hash.

use fennec_blake3::Hash;
use fennec_rt::embed::codec::{Encode, Writer};
use fennec_syntax::{Item, Module};

/// The syntax tree of a definition, hashed without its positions: its encoding with every span
/// left out, so moving it or reformatting it changes no hash.
pub fn tree_hash(tree: &impl Encode) -> Hash {
    let mut writer = Writer::without_spans();
    tree.encode(&mut writer);
    fennec_blake3::hash(&writer.into_bytes())
}

/// Every definition in a module and the hash of its tree, keyed by what it is and its name:
/// `def main`, `def Enemy.take_damage`, `struct Enemy`, `test "it works"`, `from std.math
/// import Vec3`. A type's methods
/// are keyed on their own, so changing a method doesn't change the type's hash.
pub fn definitions(module: &Module) -> Vec<(String, Hash)> {
    let mut found = Vec::new();
    for item in &module.items {
        item_definitions(item, &mut found);
    }
    found
}

/// What `definitions` finds in one item.
pub(crate) fn item_definitions(item: &Item, found: &mut Vec<(String, Hash)>) {
    {
        match item {
            Item::Function(function) => found.push((format!("def {}", function.name), tree_hash(function))),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                let kind = match item {
                    Item::Struct(_) => "struct",
                    Item::Enum(_) => "enum",
                    _ => "error",
                };
                let fields = (&def.name, &def.fields, &def.cases);
                found.push((format!("{kind} {}", def.name), tree_hash(&fields)));
                for method in &def.methods {
                    found.push((format!("def {}.{}", def.name, method.name), tree_hash(method)));
                }
            }
            Item::Interface(interface) => {
                found.push((format!("interface {}", interface.name), tree_hash(interface)));
            }
            // An extension's methods are the type's, keyed like the ones in its declaration.
            Item::Extend(extension) => {
                for method in &extension.methods {
                    found.push((format!("def {}.{}", extension.name, method.name), tree_hash(method)));
                }
            }
            Item::TypeAlias { name, .. } => found.push((format!("type {name}"), tree_hash(item))),
            Item::Const { name, .. } => found.push((format!("const {name}"), tree_hash(item))),
            Item::Import(import) => {
                let module = import.module.join(".");
                let key = if import.names.is_empty() {
                    format!("import {module}")
                } else {
                    format!("from {module} import {}", import.names.join(", "))
                };
                found.push((key, tree_hash(import)));
            }
            Item::Test(test) => {
                let word = if test.bench { "bench" } else { "test" };
                found.push((format!("{word} \"{}\"", test.name), tree_hash(test)));
            }
            Item::Stmt(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tree_hash;

    fn first_item(source: &str, base: u32) -> fennec_syntax::Item {
        fennec_syntax::parse_at(source, base).unwrap().items.remove(0)
    }

    /// Where a definition is, and how it's spaced, change nothing; what it says changes the hash,
    /// even inside a string that looks like a position.
    #[test]
    fn a_tree_hashes_by_what_it_holds_not_where_it_is() {
        let source = "def greet(name: str) -> str:\n    return \"Span { start: 1 } {name}\"\n";
        let hash = tree_hash(&first_item(source, 0));
        assert_eq!(tree_hash(&first_item(source, 4096)), hash, "moved");
        let spaced = "\n\ndef greet(name: str) -> str:\n\n    return \"Span { start: 1 } {name}\"\n";
        assert_eq!(tree_hash(&first_item(spaced, 0)), hash, "spaced out");
        for changed in [
            "def greet(name: str) -> str:\n    return \"Span { start: 2 } {name}\"\n",
            "def greet(who: str) -> str:\n    return \"Span { start: 1 } {who}\"\n",
            "def hail(name: str) -> str:\n    return \"Span { start: 1 } {name}\"\n",
        ] {
            assert_ne!(tree_hash(&first_item(changed, 0)), hash, "{changed}");
        }
    }
}
