//! Patterns that name the fields they match, like `case InvalidValue(message=m)`. Outside the kit a
//! type comes from, a pattern names its fields rather than taking them by position, so a kit
//! can add a field with a default to a type it exports without breaking anyone's `match`.
//! Once checked, a named pattern is put in its fields' order here, with `_` for the fields it
//! doesn't read before the last one it does, so what runs a program can take its parts by
//! position.

use std::collections::BTreeMap;

use fennec_syntax::{Diagnostic, Edit, ExprKind, Module, Pattern, Sources, Span, TokenKind};

/// What a pattern matching another kit's type by position is told, after the type's name.
pub(crate) const POSITIONAL: &str = "comes from another kit, so this pattern names the fields it reads";

/// What to write instead: each field by name, or nothing at all to read none of them.
pub(crate) fn help(variant: &str, names: &[String]) -> String {
    let each: Vec<String> = names.iter().map(|name| format!("{name}=...")).collect();
    format!(
        "write `{variant}({})`, or `{variant}()` to read none, and a field added to it never breaks this; `fennec fix` adds the names",
        each.join(", ")
    )
}

/// The fields of a positional pattern that `help` was given, back from what it says.
fn names_in(help: &str) -> Option<Vec<String>> {
    let written = help.strip_prefix("write `")?.split('`').next()?;
    let inside = written.split_once('(')?.1.strip_suffix(')')?;
    Some(inside.split(", ").filter_map(|each| each.strip_suffix("=...")).map(str::to_owned).collect())
}

/// Puts each named pattern in its fields' order, as `fields` says it is for the variant a case's
/// span holds.
pub(crate) fn put_in_order(module: &mut Module, fields: &BTreeMap<(u32, u32, String), Vec<String>>) {
    if fields.is_empty() {
        return;
    }
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        if let ExprKind::Match { arms, .. } = &mut expr.kind {
            for arm in arms {
                in_order(&mut arm.pattern, arm.span, fields);
            }
        }
    });
}

fn in_order(pattern: &mut Pattern, span: Span, fields: &BTreeMap<(u32, u32, String), Vec<String>>) {
    match pattern {
        Pattern::Variant { name, args, fields: named, .. } => {
            for arg in args.iter_mut() {
                in_order(arg, span, fields);
            }
            if named.is_empty() {
                return;
            }
            let Some(order) = fields.get(&(span.start, span.end, name.clone())) else { return };
            let mut given: Vec<(String, Pattern)> = named.drain(..).zip(args.drain(..)).collect();
            let mut placed: Vec<Pattern> = order
                .iter()
                .map(|field| match given.iter().position(|(name, _)| name == field) {
                    Some(at) => given.swap_remove(at).1,
                    None => Pattern::Wildcard,
                })
                .collect();
            while placed.last() == Some(&Pattern::Wildcard) {
                placed.pop();
            }
            // Named still, in order now, so checking it again finds the same thing.
            *named = order.iter().take(placed.len()).cloned().collect();
            *args = placed;
        }
        Pattern::Tuple(items) | Pattern::Or(items) => {
            for item in items {
                in_order(item, span, fields);
            }
        }
        Pattern::Wildcard | Pattern::Bind(_) | Pattern::RefBind(_) | Pattern::Literal(_) => {}
    }
}

/// Gives each pattern that matches another kit's type by position the fix that names its
/// fields: `InvalidValue(m)` becomes `InvalidValue(message=m)`, and a part that's only `_` goes, so
/// `InvalidValue(_)` becomes `InvalidValue()`.
pub fn add_pattern_fixes(diagnostics: &mut [Diagnostic], sources: &Sources) {
    for diagnostic in diagnostics.iter_mut() {
        if !diagnostic.message.ends_with(POSITIONAL) || !diagnostic.fixes.is_empty() {
            continue;
        }
        let Some(variant) = diagnostic.message.split('`').nth(1) else { continue };
        let Some(names) = diagnostic.help.as_deref().and_then(names_in) else { continue };
        if let Some(edit) = named_fix(sources, diagnostic.span, variant, &names) {
            diagnostic.fixes.push(edit);
        }
    }
}

/// The edit that names each part of the first `variant(...)` in the case at `span`.
fn named_fix(sources: &Sources, span: Span, variant: &str, names: &[String]) -> Option<Edit> {
    let (_, text, base) = sources.file_at(span)?;
    let lexed = fennec_syntax::lex_source(text).ok()?;
    let own = variant.rsplit('.').next().unwrap_or(variant);
    let written = |at: usize| {
        let token = lexed.tokens.get(at)?;
        text.get(usize::try_from(token.span.start).ok()?..usize::try_from(token.span.end).ok()?)
    };
    let inside = |token: &fennec_syntax::SourceToken| {
        base + token.span.start >= span.start && base + token.span.end <= span.end
    };
    let open = (0..lexed.tokens.len()).find(|&at| {
        lexed.tokens.get(at).is_some_and(|token| inside(token) && token.kind == TokenKind::Name)
            && written(at) == Some(own)
            && written(at + 1) == Some("(")
    })? + 1;
    // Each part's text, split at the commas between the parentheses.
    let mut parts: Vec<(u32, u32)> = Vec::new();
    let mut depth = 0;
    let mut start = None;
    let mut close = None;
    for at in open + 1..lexed.tokens.len() {
        let token = lexed.tokens.get(at)?;
        match written(at)? {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" if depth > 0 => depth -= 1,
            ")" => {
                close = Some(token.span.start);
                break;
            }
            "," if depth == 0 => {
                parts.push((start?, lexed.tokens.get(at - 1)?.span.end));
                start = None;
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(token.span.start);
        }
        if depth == 0 && written(at + 1) == Some(")") {
            parts.push((start?, token.span.end));
            start = None;
        }
    }
    let close = close?;
    if parts.len() > names.len() {
        return None;
    }
    let mut rewritten = Vec::new();
    for ((from, to), field) in parts.iter().zip(names) {
        let part = text.get(usize::try_from(*from).ok()?..usize::try_from(*to).ok()?)?;
        if part != "_" {
            rewritten.push(format!("{field}={part}"));
        }
    }
    let from = lexed.tokens.get(open)?.span.end;
    Some(Edit { span: Span { start: base + from, end: base + close }, text: rewritten.join(", ") })
}
