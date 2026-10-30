//! The second layer of hashing: each definition's syntax tree, with positions and comments
//! left out, so reformatting, editing comments and moving code around change no hash.

use std::fmt::Debug;

use fennec_blake3::{Hash, Hasher};
use fennec_syntax::{Item, Module};

/// The syntax tree of a definition, hashed without its positions.
pub fn tree_hash(tree: &impl Debug) -> Hash {
    let mut hasher = Hasher::new();
    hasher.update(without_positions(&format!("{tree:?}")).as_bytes());
    hasher.finalize()
}

/// A tree's debug text with every `Span { start: .., end: .. }` written as just `Span`.
/// Text inside string literals is left alone, so two different strings never look the same.
fn without_positions(debug: &str) -> String {
    const SPAN: &str = "Span { start: ";
    let mut out = String::with_capacity(debug.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut skipping = false;
    for (at, c) in debug.char_indices() {
        if skipping {
            skipping = c != '}';
            continue;
        }
        if in_string {
            in_string = escaped || c != '"';
            escaped = !escaped && c == '\\';
        } else if c == '"' {
            in_string = true;
        } else if debug.get(at..).is_some_and(|rest| rest.starts_with(SPAN)) {
            out.push_str("Span");
            skipping = true;
            continue;
        }
        out.push(c);
    }
    out
}

/// Every definition in a module and the hash of its tree, keyed by what it is and its name:
/// `def main`, `def Enemy.take_damage`, `struct Enemy`, `test "it works"`, `from std.math
/// import Vec3`. A type's methods
/// are keyed on their own, so changing a method doesn't change the type's hash.
pub fn definitions(module: &Module) -> Vec<(String, Hash)> {
    let mut found = Vec::new();
    for item in &module.items {
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
            Item::TypeAlias { name, .. } => found.push((format!("type {name}"), tree_hash(item))),
            Item::Import(import) => {
                let module = import.module.join(".");
                let key = if import.names.is_empty() {
                    format!("import {module}")
                } else {
                    format!("from {module} import {}", import.names.join(", "))
                };
                found.push((key, tree_hash(import)));
            }
            Item::Test(test) => found.push((format!("test \"{}\"", test.name), tree_hash(test))),
            Item::Stmt(_) => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::without_positions;

    #[test]
    fn positions_go_but_strings_stay() {
        assert_eq!(
            without_positions("Stmt { span: Span { start: 3, end: 9 }, s: \"Span { start: 1 }\" }"),
            "Stmt { span: Span, s: \"Span { start: 1 }\" }"
        );
        assert_eq!(
            without_positions("\"a\\\"Span { start: 1 }\" Span { start: 2, end: 3 }"),
            "\"a\\\"Span { start: 1 }\" Span"
        );
        assert_eq!(without_positions("\"\\\\\" Span { start: 2, end: 3 }"), "\"\\\\\" Span");
    }
}
