//! How far a program's generic functions can be copied. A generic function is copied for each set
//! of types it's used with, and everything that follows those copies, lowering and the checker's
//! walks alike, goes by these rules, so each stops where the others do:
//!
//! - **No type nests past [`DEEPEST_TYPE`].** A generic that calls itself with a bigger type each
//!   time, as `f[T]` calling `f(list[T])` does, would need a copy for every size.
//! - **A function one of whose copies was refused for that runs away**, and no new copy of it is
//!   made. Every copy already made would otherwise go on asking for more down to the same depth,
//!   each branch of a generic that calls itself in two ways doubling them.
//! - **A backstop that grows with the program**: [`COPIES_PER_FUNCTION`] for each function, and
//!   never fewer than [`LEAST_COPIES_ALLOWED`]. A chain of generics, each asking for copies of the
//!   next with two types, doubles them at every step well within the depth: a chain of 30 would ask
//!   for a billion. A program of ordinary generics asks for a few copies a function, so it never
//!   comes near.

use std::collections::HashSet;

use crate::types::Type;

/// How deeply a type a generic function is copied for can nest.
pub const DEEPEST_TYPE: usize = 32;

/// How many copies of generic functions the backstop allows for each function a program has.
pub const COPIES_PER_FUNCTION: usize = 16;

/// The least the backstop allows, for a program of only a few functions.
pub const LEAST_COPIES_ALLOWED: usize = 2000;

/// How deeply a type nests: 1 for one that holds nothing, and one more for each type that holds
/// another, whichever of its parts nests deepest.
#[must_use]
pub fn type_depth(ty: &Type) -> usize {
    let deepest = |types: &[Type]| types.iter().map(type_depth).max().unwrap_or(0);
    1 + match ty {
        Type::List(item)
        | Type::Buffer(item)
        | Type::Array(item, _)
        | Type::Iterator(item)
        | Type::Set(item)
        | Type::Optional(item)
        | Type::Table(item)
        | Type::Id(item) => type_depth(item),
        Type::Dict(first, second) | Type::Result(first, second) => type_depth(first).max(type_depth(second)),
        Type::Tuple(items) | Type::Generic(_, items) | Type::Union(items) => deepest(items),
        Type::Arrow(params, returns) => deepest(params).max(type_depth(returns)),
        _ => 0,
    }
}

/// The most copies of generic functions a program of `functions` functions can ask for.
#[must_use]
pub fn most_copies(functions: usize) -> usize {
    (COPIES_PER_FUNCTION * functions).max(LEAST_COPIES_ALLOWED)
}

/// Why a copy of a generic function can't be made.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// One of its types nests past [`DEEPEST_TYPE`]: `param` would stand for `ty`, `depth` deep.
    TooDeep { param: String, ty: Type, depth: usize },
    /// One of the function's copies was refused for its depth, so no new one is made.
    RunsAway,
    /// It would be copy `copy`, past the backstop's `most` for a program of `functions` functions.
    TooMany { copy: usize, most: usize, functions: usize },
}

impl Refusal {
    /// Why a call to `base` can't be built, as the build says it.
    #[must_use]
    pub fn said(&self, base: &str) -> String {
        match self {
            Refusal::TooDeep { param, ty, depth } => format!(
                "would need a copy of `{base}` with `{param}` as `{ty}`, a type nested {depth} deep, \
                 past the {DEEPEST_TYPE} a copy can be: a generic that calls itself with a bigger \
                 type each time needs a copy for every size, so it can't be built"
            ),
            Refusal::RunsAway => format!(
                "would need another copy of `{base}`, which calls itself with a bigger type each \
                 time: one of its copies needs a type nested past the {DEEPEST_TYPE} a copy can \
                 be, so no more are made"
            ),
            Refusal::TooMany { copy, most, functions } => format!(
                "would need copy {copy} of a generic function, past the {most} a program of \
                 {functions} functions can ask for, {COPIES_PER_FUNCTION} for each: generics that \
                 ask for copies of each other with more than one type each can double them at \
                 every step, so it can't be built"
            ),
        }
    }
}

/// The copies made so far, as the rules count them, and the functions that run away.
#[derive(Debug, Default)]
pub struct Copies {
    made: usize,
    runaway: HashSet<String>,
}

impl Copies {
    /// Whether a copy of `base` with its type parameters standing for `given` is too deep to make.
    /// Asked of every copy, made already or not: one that is marks `base` as running away.
    pub fn too_deep<'a>(
        &mut self,
        base: &str,
        given: impl IntoIterator<Item = (&'a String, &'a Type)>,
    ) -> Option<Refusal> {
        let (param, ty) = given.into_iter().find(|(_, ty)| type_depth(ty) > DEEPEST_TYPE)?;
        self.runaway.insert(base.to_owned());
        Some(Refusal::TooDeep { param: param.clone(), ty: ty.clone(), depth: type_depth(ty) })
    }

    /// Whether a new copy of `base` can be made, in a program of `functions` functions, counting
    /// it if it can: not one of a function that runs away, nor one past the backstop.
    pub fn make(&mut self, base: &str, functions: usize) -> Result<(), Refusal> {
        if self.runaway.contains(base) {
            return Err(Refusal::RunsAway);
        }
        let most = most_copies(functions);
        if self.made >= most {
            return Err(Refusal::TooMany { copy: self.made + 1, most, functions });
        }
        self.made += 1;
        Ok(())
    }
}

/// Each generic function of a program's or a kit's that comes back to itself, through its own
/// calls, with a bigger type each time, as `grow[T]` calling `grow([value])` does: lowering would
/// need a copy for every size, so the check refuses it where it's written, at the call that makes
/// the type bigger. One for each such set of functions. The standard library's own code isn't
/// looked at: it's not the program's to change.
pub(crate) fn growing(checker: &crate::checker::Checker) -> Vec<fennec_syntax::Diagnostic> {
    let mut graph = Graph::default();
    for (&(start, end), call) in &checker.generic_calls {
        let written = checker.program.functions.get(&call.caller).is_some_and(|entry| !entry.prelude);
        if !written {
            continue;
        }
        for (name, ty) in call.names.iter().zip(&call.types) {
            let ty = checker.settled(ty);
            let mut held = Vec::new();
            params_in(&ty, 0, &mut held);
            for (param, depth) in held {
                let from = graph.node(&call.caller, &param);
                let to = graph.node(&call.callee, name);
                let span = fennec_syntax::Span { start, end };
                graph.edges.push(Edge {
                    from,
                    to,
                    depth,
                    span,
                    call: (call.caller.clone(), call.callee.clone()),
                    name: name.clone(),
                    ty: ty.clone(),
                });
            }
        }
    }
    let component = graph.components();
    let mut said = HashSet::new();
    let mut found = Vec::new();
    for edge in &graph.edges {
        let looped = component.get(edge.from) == component.get(edge.to);
        if edge.depth == 0 || !looped || !said.insert(component.get(edge.from).copied()) {
            continue;
        }
        let (caller, callee) =
            (fennec_syntax::without_kits(&edge.call.0), fennec_syntax::without_kits(&edge.call.1));
        let message = if edge.call.0 == edge.call.1 {
            format!(
                "`{caller}` calls itself with a bigger type each time: here its `{}` is `{}`, so it would need a copy for every size, and can't be built",
                edge.name, edge.ty
            )
        } else {
            format!(
                "`{caller}` comes back to itself through `{callee}` with a bigger type each time: here `{callee}`'s `{}` is `{}`, so it would need a copy for every size, and can't be built",
                edge.name, edge.ty
            )
        };
        let help = "a type can't grow as a program runs: hold what nests in a type of its own, as `struct Nested:` with an `items: list[Nested]` field, or give it the same type each time";
        found.push(fennec_syntax::Diagnostic::new(message, edge.span).with_help(help));
    }
    found
}

/// Each type parameter in `ty`, with how many types hold it there: 0 for `T` itself, 1 for the `T`
/// in `list[T]`.
fn params_in(ty: &Type, depth: usize, held: &mut Vec<(String, usize)>) {
    match ty {
        Type::Param(name) => held.push((name.clone(), depth)),
        other => {
            for child in other.children() {
                params_in(child, depth + 1, held);
            }
        }
    }
}

/// A call giving a callee's type parameter a type holding one of its caller's.
struct Edge {
    from: usize,
    to: usize,
    /// How many types hold the caller's parameter in the one given: more than 0 makes it bigger.
    depth: usize,
    span: fennec_syntax::Span,
    /// The caller and the callee.
    call: (String, String),
    /// The callee's parameter, and the type it's given.
    name: String,
    ty: Type,
}

/// Each function's type parameters, and the calls between them.
#[derive(Default)]
struct Graph {
    nodes: std::collections::BTreeMap<(String, String), usize>,
    edges: Vec<Edge>,
}

impl Graph {
    fn node(&mut self, function: &str, param: &str) -> usize {
        let next = self.nodes.len();
        *self.nodes.entry((function.to_owned(), param.to_owned())).or_insert(next)
    }

    /// Which strongly connected component each node is in, by Tarjan's algorithm: two nodes share
    /// one when each reaches the other.
    fn components(&self) -> Vec<usize> {
        let count = self.nodes.len();
        let mut next: Vec<Vec<usize>> = vec![Vec::new(); count];
        for edge in &self.edges {
            if let Some(out) = next.get_mut(edge.from) {
                out.push(edge.to);
            }
        }
        let mut tarjan = Tarjan {
            next,
            index: vec![None; count],
            low: vec![0; count],
            on_stack: vec![false; count],
            stack: Vec::new(),
            counter: 0,
            component: vec![0; count],
            components: 0,
        };
        for node in 0..count {
            if tarjan.index.get(node).is_some_and(Option::is_none) {
                tarjan.visit(node);
            }
        }
        tarjan.component
    }
}

/// Tarjan's walk for strongly connected components.
struct Tarjan {
    next: Vec<Vec<usize>>,
    index: Vec<Option<usize>>,
    low: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    counter: usize,
    component: Vec<usize>,
    components: usize,
}

impl Tarjan {
    fn visit(&mut self, node: usize) {
        let at = self.counter;
        self.counter += 1;
        self.index[node] = Some(at);
        self.low[node] = at;
        self.stack.push(node);
        self.on_stack[node] = true;
        let next = self.next[node].clone();
        for to in next {
            match self.index[to] {
                None => {
                    self.visit(to);
                    self.low[node] = self.low[node].min(self.low[to]);
                }
                Some(index) if self.on_stack[to] => self.low[node] = self.low[node].min(index),
                Some(_) => {}
            }
        }
        if self.low[node] == at {
            while let Some(member) = self.stack.pop() {
                self.on_stack[member] = false;
                self.component[member] = self.components;
                if member == node {
                    break;
                }
            }
            self.components += 1;
        }
    }
}
