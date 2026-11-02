//! `full-path`: `std.math.sqrt(x)` in a file that imports `sqrt` from `std.math` already says
//! the same as `sqrt(x)`. The fix writes the name the import binds.
//!
//! A kit's loading leaves its files' imports out of the module, so each file is parsed again
//! for its own imports and what it writes.

use std::collections::HashMap;

use fennec_syntax::{Diagnostic, Expr, ExprKind, Import, Item, Sources, Span, parse_at};

use crate::Lint;
use crate::rewrites::replaceable;

/// The module path a name a file binds stands for, when it's a module: `std.math` for `std`
/// read as `std.math` after `import std.math`, `std.json` for `json` after `from std import
/// json`, and `game.physics` for `physics` after `import game.physics as physics`.
fn modules(imports: &[&Import]) -> Vec<(Vec<String>, Vec<String>)> {
    let mut found = Vec::new();
    for import in imports {
        if import.names.is_empty() {
            match &import.alias {
                // `physics.x` is `game.physics.x`.
                Some(alias) => found.push((vec![alias.clone()], import.module.clone())),
                // `game.physics.x` is written out whole.
                None => found.push((import.module.clone(), import.module.clone())),
            }
        } else {
            // `from game import physics` may bring in a module, `game.physics`.
            for (name, bound) in import.bound() {
                let mut module = import.module.clone();
                module.push(name.to_owned());
                found.push((vec![bound.to_owned()], module));
            }
        }
    }
    found
}

/// The names written in `expr` from left to right, when it's a plain dotted path like
/// `std.math.sqrt`.
fn path(expr: &Expr) -> Option<Vec<String>> {
    match &expr.kind {
        ExprKind::Name(name) => Some(vec![name.clone()]),
        ExprKind::Attr { object, name, optional: false } => {
            let mut names = path(object)?;
            names.push(name.clone());
            Some(names)
        }
        _ => None,
    }
}

/// Each place in one file that writes out the path of a name the file imports by itself.
fn in_file(text: &str, base: u32, sources: &Sources, found: &mut Vec<Lint>) {
    let Ok(mut module) = parse_at(text, base) else { return };
    let imports: Vec<&Import> = module
        .items
        .iter()
        .filter_map(|item| if let Item::Import(import) = item { Some(import) } else { None })
        .collect();
    // Each module path and name imported from it, with the name it's bound under.
    let mut imported: HashMap<(Vec<String>, String), String> = HashMap::new();
    for import in &imports {
        for (name, bound) in import.bound() {
            imported.entry((import.module.clone(), name.to_owned())).or_insert_with(|| bound.to_owned());
        }
    }
    if imported.is_empty() {
        return;
    }
    let modules = modules(&imports);
    let mut here = Vec::new();
    fennec_syntax::visit::exprs_mut(&mut module, &mut |expr| {
        let Some(written) = path(expr) else { return };
        let Some((last, prefix)) = written.split_last() else { return };
        for (bound, module) in &modules {
            let Some(rest) = prefix.strip_prefix(bound.as_slice()) else { continue };
            let mut full = module.clone();
            full.extend(rest.iter().cloned());
            if let Some(name) = imported.get(&(full.clone(), last.clone()))
                && replaceable(sources, expr)
            {
                let message = format!("`{}.{last}` is imported as `{name}` already", full.join("."));
                let diagnostic = Diagnostic::new(message, expr.span).with_fix(expr.span, name.clone());
                here.push(Lint::new("full-path", diagnostic));
                return;
            }
        }
    });
    // A path holds shorter paths, which the whole one's fix covers.
    let spans: Vec<Span> = here.iter().map(|lint| lint.diagnostic.span).collect();
    let inside = |span: Span| {
        spans.iter().any(|outer| *outer != span && outer.start <= span.start && span.end <= outer.end)
    };
    found.extend(here.into_iter().filter(|lint| !inside(lint.diagnostic.span)));
}

pub(crate) fn of(sources: &Sources) -> Vec<Lint> {
    let mut found = Vec::new();
    for (path, text) in sources.files() {
        if let Some((_, base)) = sources.by_path(path) {
            in_file(text, base, sources, &mut found);
        }
    }
    found
}
