//! Lowering a checked module to the IR, one function at a time. The types come from the
//! checker's typed tree, never from working them out again.

mod arrays;
pub(crate) mod calls;
mod closures;
mod collections;
mod constants;
mod consumers;
mod defaults;
mod failure;
mod generators;
mod generics;
mod imports;
mod layouts;
mod lists;
mod matching;
mod operators;
mod order;
mod parallel;
mod places;
mod positions;
mod structs;
mod tuples;

use std::collections::{HashMap, HashSet};

use fennec_check::{Body, Signatures, Type};
use fennec_syntax::{Arg, BinOp, Expr, ExprKind, FnDef, Item, Module, Span, Stmt, StmtKind, UnaryOp, fuse};

use crate::{
    Block, BlockId, EnumDef, FormatPart, Function, Inst, InstKind, Local, LocalId, NotLowered, Operand,
    Program, Terminator, type_in,
};

/// How a backend wants a program lowered. The default is what every engine can run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Whether the body of each `parallel` loop, comprehension and sum is lifted into a function
    /// of its own, for a backend that runs them on threads (see `lower::parallel`).
    pub outline_parallel: bool,
    /// Whether `main`, if it can fail, ends the program itself rather than give its failure back
    /// to what started it, for a backend with nothing there to take one.
    pub stop_on_failed_main: bool,
    /// Whether `for conn in parallel(server)` goes through the connections in order, one at
    /// a time, rather than giving each a task: a test's servers are a fake network the
    /// runtime holds, which only answers in order.
    pub serve_in_order: bool,
}

/// Lowers a module's functions and methods, and the standard library's that they reach. The
/// module is one `check_and_resolve` has been over, which writes in what calls leave out.
pub fn lower(module: &Module, signatures: &Signatures) -> Program {
    lower_some(module, signatures, &|_| true)
}

/// Like `lower`, the way a backend asks.
pub fn lower_with(module: &Module, signatures: &Signatures, options: Options) -> Program {
    lower_from(module, signatures, &|_| true, options)
}

/// Like `lower`, starting only from the functions and constants `wanted` picks. What they call
/// in the module and `wanted` leaves out is listed in [`Program::external`]. Something that
/// runs a program every time it changes wants this, since lowering a big one takes a while.
pub fn lower_some(module: &Module, signatures: &Signatures, wanted: &dyn Fn(&str) -> bool) -> Program {
    lower_from(module, signatures, wanted, Options::default())
}

fn lower_from(
    module: &Module,
    signatures: &Signatures,
    wanted: &dyn Fn(&str) -> bool,
    options: Options,
) -> Program {
    let library = fennec_syntax::prelude_to_run().unwrap_or_default();
    let modules: Vec<&Module> = std::iter::once(module).chain(library).collect();
    let functions = definitions(module);
    // The library's functions the checker rewrote for this program stand in for its own.
    let rewritten = module.std_rewritten.iter().map(|(key, def)| (key.clone(), def));
    let mut every: HashMap<String, &FnDef> =
        library.iter().flat_map(definitions).chain(rewritten.clone()).collect();
    every.extend(functions.iter().map(|(key, def)| (key.clone(), *def)));
    let reachable_later: HashMap<String, &FnDef> = library
        .iter()
        .flat_map(definitions)
        .chain(rewritten)
        .filter(|(key, _)| !functions.iter().any(|(own, _)| own == key))
        .collect();
    let defs = structs::definitions(&modules, signatures);
    let enums = matching::definitions(&modules, signatures);
    let errors = failure::definitions(signatures);
    let context = Context {
        shared_types: shared_types(module),
        signatures,
        errors: errors.iter().map(|def| def.name.as_str()).collect(),
        enums: enums.iter().map(|def| (def.name.as_str(), def)).collect(),
        functions: functions
            .iter()
            .map(|(key, _)| key.as_str())
            .chain(reachable_later.keys().map(String::as_str))
            .collect(),
        structs: defs.iter().map(|def| def.name.as_str()).collect(),
        constants: constants::written(module),
        defaults: defaults::written(module),
        rules: defaults::rules(module),
        imports: imports::written(module),
        library_imports: imports::of_library(library),
        library: reachable_later.keys().map(String::as_str).collect(),
        generic_types: generic_types(&modules),
        instances: generics::Instances::default(),
        generator_defs: std::cell::RefCell::default(),
        own_texts: std::cell::RefCell::default(),
        layouts: std::cell::RefCell::default(),
        layout_problems: std::cell::RefCell::default(),
        // A library's yield methods too, which a loop over one can copy in.
        generators: those_where(&every, |def| fuse::has_yield(&def.body)),
        mut_forms: those_where(&every, |def| def.name == "[]mut"),
        intrinsics: every.iter().filter(|(_, def)| is_intrinsic(def)).map(|(key, _)| key.clone()).collect(),
        options,
        saves_itself: saves_itself(&functions),
    };
    let mut program = Program::default();
    for (key, def) in &functions {
        // Something generic is only lowered as the copies a program uses.
        let generic = signatures.get(key).is_some_and(generics::is_generic);
        if !generic && def.generics.is_empty() && wanted(key) {
            lower_into(&mut program, &generics::FunctionInstance::plain(key), def, &context);
        }
    }
    constants::lower_all(&context, &mut program, wanted);
    // Copies of generics, and the standard library's functions, come in as they're reached.
    loop {
        let waiting = context.instances.waiting.borrow_mut().pop();
        let next = waiting.or_else(|| {
            calls::external(&program)
                .into_iter()
                .find(|key| reachable_later.contains_key(key))
                .map(|key| generics::FunctionInstance::plain(&key))
        });
        let Some(next) = next else { break };
        if let Some(def) = every.get(&next.base) {
            lower_into(&mut program, &next, def, &context);
            constants::lower_all(&context, &mut program, wanted);
        } else {
            let why = format!("it has no definition: `{}`", next.base);
            program.not_lowered.push(NotLowered { function: next.name, why });
        }
    }
    failure::stop_on_failed_main(&mut program, &functions, options);
    program.external = calls::external(&program);
    let (mut defs, mut enums) = (defs.clone(), enums.iter().map(|def| (*def).clone()).collect::<Vec<_>>());
    structs::add_copies(&context, &mut defs, &mut enums);
    take_what_was_found(&mut program, &context);
    program.structs = defs;
    program.enums = enums;
    program.errors = errors;
    crate::arrays::spill_big_arrays(&mut program);
    program
}

/// The functions in `every` that `keep` picks.
/// The program's types whose `def serialize` it writes itself: one the checker generated is in
/// the prelude's spans.
fn saves_itself(functions: &[(String, &FnDef)]) -> HashSet<String> {
    functions
        .iter()
        .filter(|(key, def)| key.ends_with(".serialize") && !def.span.in_prelude())
        .filter_map(|(key, _)| key.strip_suffix(".serialize").map(str::to_owned))
        .collect()
}

/// What lowering the functions found along the way: the generators passed as values, the types
/// with their own text, and the layouts `std.pack` saves.
fn take_what_was_found(program: &mut Program, context: &Context<'_>) {
    program.generators = context.generator_defs.take();
    program.own_texts = context.own_texts.take();
    program.layouts = context.layouts.take();
    program.layout_problems = context.layout_problems.take();
}

fn those_where<'a>(
    every: &HashMap<String, &'a FnDef>,
    keep: impl Fn(&FnDef) -> bool,
) -> HashMap<String, &'a FnDef> {
    every.iter().filter(|(_, def)| keep(def)).map(|(key, def)| (key.clone(), *def)).collect()
}

/// Whether a function is one of the standard library's `@intrinsic` ones, which have no body.
fn is_intrinsic(def: &FnDef) -> bool {
    def.body.is_empty() && def.annotations.iter().any(|annotation| annotation.name == "intrinsic")
}

fn lower_into(
    program: &mut Program,
    instance: &generics::FunctionInstance,
    def: &FnDef,
    context: &Context<'_>,
) {
    match Lowering::function(instance, def, context, (&[], None)) {
        Ok(functions) => program.functions.extend(functions),
        Err(why) => program.not_lowered.push(NotLowered { function: instance.name.clone(), why }),
    }
}

/// The generic structs and enums of a module and the standard library.
fn generic_types<'a>(modules: &[&'a Module]) -> HashMap<&'a str, &'a fennec_syntax::TypeDef> {
    let mut found = HashMap::new();
    for item in modules.iter().rev().flat_map(|module| &module.items) {
        if let Item::Struct(def) | Item::Enum(def) = item
            && !def.generics.is_empty()
        {
            found.insert(def.name.as_str(), def);
        }
    }
    found
}

/// Every function and method in a module, by its key in the typed tree.
fn definitions<'m>(module: &'m Module) -> Vec<(String, &'m FnDef)> {
    let mut found = Vec::new();
    // The first of each name a type has, as the checker keys a method defined again.
    let mut first: HashMap<String, &'m FnDef> = HashMap::new();
    let mut methods = |owner: &str, defs: &'m [FnDef]| -> Vec<String> {
        defs.iter()
            .map(|def| {
                let plain = format!("{owner}.{}", def.name);
                let key = fennec_check::method_key(owner, def, first.get(&plain).copied());
                first.entry(plain).or_insert(def);
                key
            })
            .collect()
    };
    for item in &module.items {
        match item {
            Item::Function(def) => found.push((def.name.clone(), def)),
            Item::Struct(def) | Item::Enum(def) | Item::Error(def) => {
                found.extend(methods(&def.name, &def.methods).into_iter().zip(&def.methods));
            }
            Item::Extend(extension) => {
                found
                    .extend(methods(&extension.name, &extension.methods).into_iter().zip(&extension.methods));
            }
            _ => {}
        }
    }
    found
}

/// What stopped a function being lowered.
type Lowered<T> = Result<T, String>;

/// A block being built. It has no terminator until it's finished.
#[derive(Default)]
struct Open {
    insts: Vec<Inst>,
    end: Option<Terminator>,
}

/// What becomes of the value of the last statement of a block.
#[derive(Clone, Copy)]
enum Tail {
    /// Nothing keeps it.
    Drop,
    /// The function gives it back.
    Return,
    /// It's the value of a `match`, kept in this local.
    Into(LocalId),
}

/// Where `break` and `continue` go.
struct Loop {
    next: BlockId,
    exit: BlockId,
    /// How many things to undo on leaving the function there were outside the loop.
    depth: usize,
}

/// The capabilities, whose methods the engines provide.
pub(crate) const CAPABILITIES: [&str; 11] =
    ["Stdout", "Fs", "Clock", "Args", "Stdin", "Process", "Entropy", "Net", "Env", "Signals", "Stderr"];

/// What's known about the whole program.
struct Context<'a> {
    /// The program's `shared` types, whose values are handles.
    shared_types: Vec<String>,
    signatures: &'a Signatures,
    /// The program's enums that the IR can hold.
    enums: HashMap<&'a str, &'a EnumDef>,
    /// The error types that the IR can hold.
    errors: HashSet<&'a str>,
    /// The keys of the program's functions and methods.
    functions: HashSet<&'a str>,
    /// The names of the program's structs that the IR can hold.
    structs: HashSet<&'a str>,
    /// The values of the constants the program can read, the standard library's included.
    constants: HashMap<&'a str, constants::Written<'a>>,
    /// The fields with defaults, the standard library's included.
    defaults: HashMap<defaults::FieldKey<'a>, &'a Expr>,
    /// The `where` rules of each struct, the standard library's included, in field order.
    rules: HashMap<&'a str, Vec<(&'a str, &'a Expr)>>,
    /// What the module imports from the standard library.
    imports: imports::Imports,
    /// What the standard library's own code imports.
    library_imports: imports::Imports,
    /// The keys of the standard library's functions and methods.
    library: HashSet<&'a str>,
    /// The generic structs and enums, the standard library's included.
    generic_types: HashMap<&'a str, &'a fennec_syntax::TypeDef>,
    /// The copies of generics a program uses.
    instances: generics::Instances,
    /// The yield functions of the module, which a loop over one can take a copy of.
    generators: HashMap<String, &'a FnDef>,
    /// The `mut` forms of `def []`, whose places a position lowers to.
    mut_forms: HashMap<String, &'a FnDef>,
    /// The generators lowered so far.
    generator_defs: std::cell::RefCell<Vec<crate::GeneratorDef>>,
    /// The types whose own text is asked for by printing something that holds them.
    own_texts: std::cell::RefCell<Vec<crate::OwnText>>,
    /// The layout of each type `std.pack` saves, as `layout[T]()` gives them.
    layouts: std::cell::RefCell<Vec<crate::SavedLayout>>,
    /// Why a type can't load a layout its kit's `pack.lock` holds for it.
    layout_problems: std::cell::RefCell<Vec<String>>,
    /// The standard library's `@intrinsic` functions, which have no body: a call to one is an
    /// [`InstKind::Builtin`] of its key, which each engine provides.
    intrinsics: HashSet<String>,
    /// How the backend wants the program lowered.
    options: Options,
    /// The program's types that write their own `def serialize`, which a layout can't see into.
    saves_itself: HashSet<String>,
}

impl Context<'_> {
    /// Whether a value of this type is a `shared` handle.
    fn is_handle(&self, ty: &Type) -> bool {
        matches!(ty, Type::Named(name) if self.shared_types.contains(name))
    }

    /// The type of a function as a value, when the IR holds it.
    fn arrow_of(&self, key: &str) -> Option<Type> {
        let signature = self.signatures.get(key)?;
        let params = signature.params.iter().map(|param| param.ty.clone()).collect();
        let ty = Type::Arrow(params, Box::new(signature.returns.clone()));
        self.holds(&ty).then_some(ty)
    }

    /// Whether the IR holds values of this type yet.
    fn holds(&self, ty: &Type) -> bool {
        match ty {
            // A sized number is held in a word, as an `int` or a `float` is.
            Type::Int | Type::Float | Type::Sized(_) | Type::Bool | Type::Str | Type::Bytes | Type::None => {
                true
            }
            Type::List(item)
            | Type::Buffer(item)
            | Type::Iterator(item)
            | Type::Set(item)
            | Type::Optional(item)
            | Type::Table(item)
            | Type::Id(item)
            | Type::Array(item, _) => self.holds(item),
            Type::Dict(key, value) => self.holds(key) && self.holds(value),
            Type::Tuple(items) | Type::Union(items) => items.iter().all(|item| self.holds(item)),
            Type::Arrow(params, returns) => {
                params.iter().all(|param| self.holds(param))
                    && (**returns == Type::Unit || self.holds(returns))
            }
            Type::Function(key) => self.arrow_of(key).is_some(),
            Type::Result(ok, error) => (**ok == Type::Unit || self.holds(ok)) && self.holds(error),
            // An error with cases is held as an enum.
            Type::Errors(names) => names
                .iter()
                .all(|name| self.errors.contains(name.as_str()) || self.enums.contains_key(name.as_str())),
            Type::Named(name) => {
                self.structs.contains(name.as_str())
                    || self.instances.types.borrow().contains_key(name.as_str())
                    || self.enums.contains_key(name.as_str())
                    || self.errors.contains(name.as_str())
                    || CAPABILITIES.contains(&name.as_str())
            }
            _ => false,
        }
    }
}

struct Lowering<'a> {
    /// The function's name in the IR.
    name: String,
    /// The function's key in the typed tree.
    base: String,
    /// What its type parameters stand for.
    subst: generics::Subst,
    context: &'a Context<'a>,
    body: &'a Body,
    returns: Type,
    locals: Vec<Local>,
    blocks: Vec<Open>,
    current: BlockId,
    /// The names in each block being lowered, innermost last.
    scopes: Vec<HashMap<String, LocalId>>,
    loops: Vec<Loop>,
    /// The `for mut` loops being lowered, innermost last.
    leaving: Vec<places::Leaving>,
    /// Whether what's being left is left by a failure, which a `lock ... as ref` block doesn't keep
    /// the changes of.
    failing_out: bool,
    /// The failure leaving, while a loop over a generator lets go of it on the way out: a
    /// failure of its cleanup goes on this as a note.
    failing_with: Option<LocalId>,
    /// The `shared` handles the `with` blocks being lowered hold the locks of.
    locked: Vec<LocalId>,
    /// The locals changed so far, for operands read before one of those changes (see `order`).
    changed: Vec<LocalId>,
    /// The closures' functions lifted out of this one.
    lifted: Vec<Function>,
    /// What a generator being lowered knows.
    generating: Option<generators::Generating>,
    /// The `for` loops that count.
    counts: Vec<crate::Count>,
    /// The places calls change.
    places: Vec<crate::ChangedPlace>,
    /// What's making the collections of comprehensions over stages, innermost last.
    fills: Vec<collections::Fill>,
    /// The types of the yield functions whose copies are being lowered, innermost last.
    copied: Vec<(String, &'a Body, Option<generics::Subst>)>,
    /// The types of the hidden names a copied yield function's parameters are bound to.
    bound: HashMap<String, Type>,
    /// The types written in brackets at the call being lowered.
    written_types: Vec<Type>,
    /// Set while lowering a `list.pop()` whose list is known not to be empty.
    popping: bool,
    /// What the `with` blocks being lowered run when a failure leaves from inside what they
    /// call, innermost last.
    on_failure: Vec<failure::OnFailure>,
    /// How many `parallel` loops being lowered as ordinary loops hold this statement. A failure
    /// in one hasn't had its block's cleanup written in.
    plain_rounds: usize,
    /// Types that stand in for what the typed tree says, innermost last: a union's receiver,
    /// read as one member while that member's method is called.
    retyped: Vec<(Span, Type)>,
}

/// The name a place is reached from: `text` for `text`, `self` for `self._text` or `self.parts[0]`.
fn root_name(target: &Expr) -> Option<&str> {
    match &target.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Attr { object, optional: false, .. } | ExprKind::Index { object, .. } => root_name(object),
        _ => None,
    }
}

/// The pieces of `a + b + c`, left to right.
fn added_pieces(value: &Expr) -> Vec<Expr> {
    match &value.kind {
        ExprKind::Binary { op: BinOp::Add, lhs, rhs } => {
            let mut pieces = added_pieces(lhs);
            pieces.push((**rhs).clone());
            pieces
        }
        _ => vec![value.clone()],
    }
}

/// Whether an expression reads a name anywhere in it.
fn mentions(piece: &Expr, name: &str) -> bool {
    let mut found = matches!(&piece.kind, ExprKind::Name(held) if held == name);
    let mut looked = piece.clone();
    fennec_syntax::visit::in_expr(&mut looked, &mut |inner| {
        found |= matches!(&inner.kind, ExprKind::Name(held) if held == name);
    });
    found
}

/// What `name = value` adds to `name`'s text, in order, when `value` is `name + a + b ...` or an
/// f-string that starts with `name` as it is: the pieces, or the rest of the f-string. Each is
/// worked out after the ones before it, as the whole would be.
fn grown_by(name: &str, value: &Expr) -> Option<Vec<Expr>> {
    let pieces = pieces_grown_by(name, value)?;
    // A piece after the first that reads the text itself would see it grown by the ones before.
    (!pieces.iter().skip(1).any(|piece| mentions(piece, name))).then_some(pieces)
}

fn pieces_grown_by(name: &str, value: &Expr) -> Option<Vec<Expr>> {
    match &value.kind {
        ExprKind::Binary { op: BinOp::Add, lhs, rhs } => {
            if matches!(&lhs.kind, ExprKind::Name(held) if held == name) {
                return Some(vec![(**rhs).clone()]);
            }
            let mut pieces = grown_by_adding(name, lhs)?;
            pieces.push((**rhs).clone());
            Some(pieces)
        }
        ExprKind::FStr(parts) => match parts.split_first() {
            Some((fennec_syntax::FStrPart::Expr { expr, spec: None }, rest))
                if !rest.is_empty() && matches!(&expr.kind, ExprKind::Name(held) if held == name) =>
            {
                Some(vec![Expr { kind: ExprKind::FStr(rest.to_vec()), span: value.span }])
            }
            _ => None,
        },
        _ => None,
    }
}

/// The pieces of `name + a + b ...` after `name`, when that's what it is.
fn grown_by_adding(name: &str, value: &Expr) -> Option<Vec<Expr>> {
    let ExprKind::Binary { op: BinOp::Add, lhs, rhs } = &value.kind else { return None };
    let mut pieces = if matches!(&lhs.kind, ExprKind::Name(held) if held == name) {
        Vec::new()
    } else {
        grown_by_adding(name, lhs)?
    };
    pieces.push((**rhs).clone());
    Some(pieces)
}

impl<'a> Lowering<'a> {
    fn new(
        instance: &generics::FunctionInstance,
        context: &'a Context<'a>,
        body: &'a Body,
        returns: Type,
    ) -> Self {
        Lowering {
            name: instance.name.clone(),
            base: instance.base.clone(),
            subst: instance.subst.clone(),
            context,
            body,
            returns,
            locals: Vec::new(),
            blocks: vec![Open::default()],
            current: BlockId(0),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            leaving: Vec::new(),
            failing_out: false,
            failing_with: None,
            locked: Vec::new(),
            changed: Vec::new(),
            lifted: Vec::new(),
            generating: None,
            counts: Vec::new(),
            places: Vec::new(),
            fills: Vec::new(),
            copied: Vec::new(),
            bound: HashMap::new(),
            written_types: Vec::new(),
            popping: false,
            on_failure: Vec::new(),
            plain_rounds: 0,
            retyped: Vec::new(),
        }
    }

    /// A function, and those lifted out of it. A nested function's first parameters are what
    /// it captures, and it's given its own type if it names itself.
    fn function(
        instance: &generics::FunctionInstance,
        def: &FnDef,
        context: &'a Context<'a>,
        (captures, itself): (&[(String, Type)], Option<&Type>),
    ) -> Lowered<Vec<Function>> {
        let key = instance.name.as_str();
        let raw = context.signatures.get(&instance.base).ok_or("it has no signature")?;
        let signature = context.instantiate(raw, &instance.subst);
        let body = context.signatures.types().body(&instance.base).ok_or("it has no types")?;
        if signature.returns != Type::Unit && !context.holds(&signature.returns) {
            return Err(format!("it gives back {}", signature.returns));
        }
        if signature.generator {
            if itself.is_some() {
                return Err("a nested generator names itself".to_owned());
            }
            return Lowering::generator(instance, def, context, captures, &signature);
        }
        let mut lowering = Lowering::new(instance, context, body, signature.returns.clone());
        for (name, ty) in captures {
            lowering.declare(name, ty.clone());
        }
        for param in &signature.params {
            if !context.holds(&param.ty) {
                return Err(format!("`{}` is {}", param.name, param.ty));
            }
            lowering.declare(&param.name, param.ty.clone());
        }
        if let Some(ty) = itself {
            lowering.bind_itself(key, &def.name, ty, captures.len(), def.span);
        }
        // A failure that ends the program is shown, so a `Secret` in it has to stay hidden.
        if key == "main" {
            for name in context.signatures.error_types() {
                let error = Type::Named(name.to_owned());
                if lowering.holds_secret(&error, &mut Vec::new()) {
                    lowering.own_texts_within(&error, &mut Vec::new(), def.span)?;
                }
            }
        }
        let plain = lowering.returns == Type::Unit;
        lowering.stmts(&def.body, if plain { Tail::Drop } else { Tail::Return })?;
        lowering.end_function(def.span)?;
        // A `shared` handle is never copied back: it's the same handle after the call.
        let mut_params = signature
            .params
            .iter()
            .enumerate()
            .filter(|(_, param)| param.is_mut && !context.is_handle(&param.ty))
            .map(|(at, _)| at + captures.len())
            .collect();
        Ok(lowering.finished(captures.len() + signature.params.len(), mut_params))
    }

    fn declare(&mut self, name: &str, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len());
        self.locals.push(Local { name: Some(name.to_owned()), ty, copied: false });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), id);
        }
        id
    }

    fn local_type(&self, local: LocalId) -> Type {
        self.locals.get(local.0).map_or(Type::Unknown, |held| held.ty.clone())
    }

    fn temporary(&mut self, ty: Type) -> LocalId {
        self.locals.push(Local { name: None, ty, copied: false });
        LocalId(self.locals.len() - 1)
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).copied())
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Open::default());
        BlockId(self.blocks.len() - 1)
    }

    /// Whether the current block can still take instructions.
    fn is_open(&self) -> bool {
        self.blocks.get(self.current.0).is_some_and(|block| block.end.is_none())
    }

    /// Ends the current block, unless it's ended already.
    fn finish(&mut self, end: Terminator) {
        if let Some(block) = self.blocks.get_mut(self.current.0)
            && block.end.is_none()
        {
            block.end = Some(end);
        }
    }

    /// Adds an instruction to the block being built. One that reads what a `shared` handle
    /// holds takes the handle's lock to read, around just that instruction.
    fn emit(&mut self, kind: InstKind, span: Span) {
        self.note_change(&kind);
        let handles = self.read_through(&kind);
        let Some(block) = self.blocks.get_mut(self.current.0) else { return };
        for handle in &handles {
            let lock = InstKind::Lock { handle: handle.clone(), write: false };
            block.insts.push(Inst { kind: lock, span });
        }
        block.insts.push(Inst { kind, span });
        for handle in handles.into_iter().rev() {
            block.insts.push(Inst { kind: InstKind::Unlock { handle }, span });
        }
    }

    /// The `shared` handles an instruction reads the cell of, and that aren't locked already.
    fn read_through(&self, kind: &InstKind) -> Vec<Operand> {
        let read: Vec<&Operand> = match kind {
            InstKind::GetField { object, .. } => vec![object],
            InstKind::Builtin { args, .. } => args.iter().collect(),
            InstKind::Format { parts, .. } => parts
                .iter()
                .filter_map(|part| match part {
                    FormatPart::Value { value, .. } => Some(value),
                    FormatPart::Text(_) => None,
                })
                .collect(),
            _ => return Vec::new(),
        };
        let mut handles: Vec<Operand> = Vec::new();
        for operand in read {
            let is_handle = type_in(&self.locals, operand).is_some_and(|ty| self.context.is_handle(&ty));
            if is_handle && !self.locked_already(operand) && !handles.contains(operand) {
                handles.push(operand.clone());
            }
        }
        handles
    }

    /// Whether a handle's lock is held already: it's `self`, in one of its own methods, which
    /// are only called with the lock taken.
    fn locked_already(&self, handle: &Operand) -> bool {
        if matches!(handle, Operand::Local(local) if self.locked.contains(local)) {
            return true;
        }
        let itself = self.lookup("self").map(Operand::Local);
        itself.as_ref() == Some(handle)
            && type_in(&self.locals, handle).is_some_and(|ty| self.context.is_handle(&ty))
    }

    /// The type the typed tree has for an expression, when the IR can hold it.
    fn ty(&self, expr: &Expr) -> Lowered<Type> {
        let ty = self.type_at(expr.span).ok_or("an expression has no type")?;
        self.held(&ty).ok_or_else(|| format!("it uses a value of type {ty}"))
    }

    /// A type as the IR holds it, when it does: a function as a value is a closure.
    fn held(&self, ty: &Type) -> Option<Type> {
        let ty = self.closures_for_functions(ty)?;
        self.context.holds(&ty).then_some(ty)
    }

    /// A type with each function named in it, as in `(add, 4)`'s, the closure it's held as.
    pub(super) fn closures_for_functions(&self, ty: &Type) -> Option<Type> {
        let each = |items: &[Type]| {
            items.iter().map(|item| self.closures_for_functions(item)).collect::<Option<Vec<_>>>()
        };
        Some(match ty {
            Type::Function(key) => self.context.arrow_of(key)?,
            Type::Tuple(items) => Type::Tuple(each(items)?),
            Type::List(item) => Type::List(Box::new(self.closures_for_functions(item)?)),
            Type::Optional(item) => Type::Optional(Box::new(self.closures_for_functions(item)?)),
            other => other.clone(),
        })
    }

    /// Lowers statements in order. Code after leaving early never runs, so it's skipped.
    /// `tail` says what becomes of the value of the last one.
    fn stmts(&mut self, stmts: &[Stmt], tail: Tail) -> Lowered<()> {
        for (at, stmt) in stmts.iter().enumerate() {
            if !self.is_open() {
                break;
            }
            self.stmt(stmt, if at + 1 == stmts.len() { tail } else { Tail::Drop })?;
        }
        Ok(())
    }

    fn block(&mut self, stmts: &[Stmt], tail: Tail) -> Lowered<()> {
        self.scopes.push(HashMap::new());
        let lowered = self.stmts(stmts, tail);
        self.scopes.pop();
        lowered
    }

    fn stmt(&mut self, stmt: &Stmt, tail: Tail) -> Lowered<()> {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.tail_expr(expr, tail)?,
            StmtKind::Assign { target, op, value, ty: _, declare_mut } => {
                self.assign(target, (*op, *declare_mut), value, stmt.span)?;
            }
            StmtKind::If { branches, otherwise } => self.if_stmt(branches, otherwise.as_deref(), tail)?,
            StmtKind::While { cond, body } => self.while_stmt(cond, body)?,
            StmtKind::Return(Some(value)) => self.return_value(value)?,
            StmtKind::Return(None) => self.return_nothing(stmt.span)?,
            // The checker writes each `lend` in where its source is used.
            StmtKind::Lend { .. } => return Err("a `lend` outside a `with` source".into()),
            StmtKind::Break | StmtKind::Continue => {
                let target = self.loops.last().ok_or("`break` outside a loop")?;
                let depth = target.depth;
                let target = if matches!(stmt.kind, StmtKind::Break) { target.exit } else { target.next };
                self.undo_since(depth, stmt.span)?;
                self.finish(Terminator::Jump(target));
            }
            StmtKind::Pass => {}
            StmtKind::For { names, is_mut: false, iter, body }
                if fennec_check::fields_of(iter).is_some() && self.lookup("fields").is_none() =>
            {
                self.each_field(names, iter, body)?;
            }
            StmtKind::For { names, is_mut, iter, body } => {
                let lifted = self
                    .parallel_for((names, *is_mut), iter, body, stmt.span)
                    .or_else(|| self.parallel_serve(names, iter, body, stmt.span));
                if let Some(outlined) = lifted {
                    outlined?;
                } else {
                    let rounds = usize::from(parallel::parallel_items(iter).is_some());
                    self.plain_rounds += rounds;
                    let lowered = self.for_stmt((names, *is_mut), iter, body, stmt.span);
                    self.plain_rounds -= rounds;
                    lowered?;
                }
            }
            StmtKind::OnFailure { name, cleanup, body } => {
                self.on_failure.push(failure::OnFailure { name: name.clone(), cleanup: cleanup.clone() });
                let lowered = self.block(body, tail);
                self.on_failure.pop();
                lowered?;
            }
            StmtKind::Fail(error) => self.fail(error, stmt.span)?,
            StmtKind::Yield(value) => self.yield_value(value, stmt.span)?,
            StmtKind::Assert(cond) => self.assert(cond)?,
            StmtKind::With { value, names, body, .. } => {
                self.with_block(value, names, body, tail, stmt.span)?;
            }
            StmtKind::Def(def) => self.nested(def, stmt.span)?,
        }
        Ok(())
    }

    /// An expression statement, with what becomes of its value. Each case of a `match`
    /// passes its own value on the same way.
    fn tail_expr(&mut self, expr: &Expr, tail: Tail) -> Lowered<()> {
        if collections::is_fill_marker(expr) {
            return self.fill_marker(expr.span);
        }
        if let ExprKind::Match { subject, arms } = &expr.kind {
            return self.match_expr(subject, arms, tail);
        }
        match tail {
            Tail::Drop => self.effect(expr)?,
            Tail::Return => self.return_value(expr)?,
            Tail::Into(dest) => {
                let value = self.expr(expr)?;
                let ty = self.local_type(dest);
                let value = self.coerce(value, &ty, expr.span)?;
                self.emit(InstKind::Copy { dest, value }, expr.span);
            }
        }
        Ok(())
    }

    fn assign(
        &mut self,
        target: &Expr,
        (op, declares): (Option<BinOp>, bool),
        value: &Expr,
        span: Span,
    ) -> Lowered<()> {
        if let ExprKind::Tuple(targets) = &target.kind {
            return self.assign_unpacking(targets, value, span);
        }
        if op.is_none() && !declares && self.replaces_its_source(target, value, span)? {
            return Ok(());
        }
        // `text = text + piece` and `text = f"{text}..."` make the same text `text += ...` does,
        // so they're that: the text grows where it is, rather than being copied into a new one.
        if op.is_none()
            && !declares
            && let ExprKind::Name(name) = &target.kind
            && self.lookup(name).is_some_and(|held| self.local_type(held) == Type::Str)
            && let Some(pieces) = grown_by(name, value)
        {
            for piece in &pieces {
                self.assign(target, (Some(BinOp::Add), false), piece, span)?;
            }
            return Ok(());
        }
        // `text += a + b + c` adds each piece in turn, as `text += a`, `text += b`, `text += c`,
        // with no texts made to join first. A piece that reads what the target is reached from
        // isn't split off, since it would see the text grown already.
        if op == Some(BinOp::Add)
            && let Some(root) = root_name(target)
            && self.type_at(target.span) == Some(Type::Str)
            && let ExprKind::Binary { op: BinOp::Add, .. } = &value.kind
        {
            let pieces = added_pieces(value);
            if pieces.iter().all(|piece| self.type_at(piece.span) == Some(Type::Str))
                && !pieces.iter().any(|piece| mentions(piece, root))
            {
                for piece in &pieces {
                    self.assign(target, (Some(BinOp::Add), false), piece, span)?;
                }
                return Ok(());
            }
        }
        let ExprKind::Name(name) = &target.kind else {
            return self.assign_place(target, op, value, span);
        };
        // `_ = save(path) else as why:` keeps nothing, and what gives nothing is only run.
        let discarded =
            name.strip_prefix('_').is_some_and(|rest| rest.starts_with(fennec_syntax::visit::HIDDEN));
        if discarded && op.is_none() && self.type_at(value.span) == Some(Type::Unit) {
            return self.effect(value);
        }
        // A name made while fusing a stage is new, and holds what it's first given.
        let binds_item = declares && target.span == value.span && op.is_none();
        if binds_item || (name.starts_with('\0') && self.lookup(name).is_none()) {
            let ty = match self.bound.get(name) {
                Some(ty) => ty.clone(),
                None => self.fused_type(value)?,
            };
            let value = self.expr(value)?;
            let value = self.coerce(value, &ty, span)?;
            let dest = self.declare(name, ty);
            self.emit(InstKind::Copy { dest, value }, span);
            return Ok(());
        }
        // What's added to is read first, left to right, if the value can change it by `ref`.
        let before = match (op, self.lookup(name)) {
            (Some(_), Some(held)) if places::calls_in(value).1 => {
                let copy = self.temporary(self.local_type(held));
                self.emit(InstKind::Copy { dest: copy, value: Operand::Local(held) }, span);
                Some(Operand::Local(copy))
            }
            _ => None,
        };
        let value = self.expr(value)?;
        let dest = self.named_target(target, name)?;
        if let Some(op) = op {
            let ty = self.local_type(dest);
            let left = before.unwrap_or(Operand::Local(dest));
            if self.check_operands(op, &left, &value).is_ok() {
                let kind = InstKind::Binary { dest, op, lhs: left, rhs: value };
                self.emit(kind, span);
            } else {
                let value = self.operate(op, (left, value), &ty, span)?;
                self.emit(InstKind::Copy { dest, value }, span);
            }
        } else {
            let ty = self.local_type(dest);
            let value = self.coerce(value, &ty, span)?;
            self.emit(InstKind::Copy { dest, value }, span);
        }
        Ok(())
    }

    /// The local a name being assigned to holds its value in, declared if it's new.
    fn named_target(&mut self, target: &Expr, name: &str) -> Lowered<LocalId> {
        if let Some(dest) = self.lookup(name) {
            return Ok(dest);
        }
        let ty = self.body.name(target.span, name).ok_or_else(|| format!("`{name}` has no type"))?;
        let ty = self.concrete(ty);
        let held = self.held(&ty).ok_or_else(|| format!("`{name}` is {ty}"))?;
        Ok(self.declare(name, held))
    }

    fn if_stmt(
        &mut self,
        branches: &[(Expr, Vec<Stmt>)],
        otherwise: Option<&[Stmt]>,
        tail: Tail,
    ) -> Lowered<()> {
        let join = self.new_block();
        for (cond, body) in branches {
            let cond = self.expr(cond)?;
            let then = self.new_block();
            let next = self.new_block();
            self.finish(Terminator::Branch { cond, then, otherwise: next });
            self.current = then;
            self.block(body, tail)?;
            self.finish(Terminator::Jump(join));
            self.current = next;
        }
        self.block(otherwise.unwrap_or_default(), tail)?;
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(())
    }

    fn while_stmt(&mut self, cond: &Expr, body: &[Stmt]) -> Lowered<()> {
        let head = self.new_block();
        let then = self.new_block();
        let exit = self.new_block();
        self.finish(Terminator::Jump(head));
        self.current = head;
        // `while True:` leaves only by `break`.
        if matches!(cond.kind, ExprKind::Bool(true)) {
            self.finish(Terminator::Jump(then));
        } else {
            let cond = self.expr(cond)?;
            self.finish(Terminator::Branch { cond, then, otherwise: exit });
        }
        self.current = then;
        self.loops.push(Loop { next: head, exit, depth: self.leaving.len() });
        let lowered = self.block(body, Tail::Drop);
        self.loops.pop();
        lowered?;
        self.finish(Terminator::Jump(head));
        self.current = exit;
        Ok(())
    }

    /// An expression whose value nothing keeps.
    fn effect(&mut self, expr: &Expr) -> Lowered<()> {
        if let ExprKind::Call { callee, args } = &expr.kind
            && self.constructs(callee, expr.span).is_none()
            && self.built_case(expr).is_none()
        {
            return self.call(callee, args, None, expr.span).map(|_| ());
        }
        if let ExprKind::Try { value, context } = &expr.kind {
            return self.try_value(value, context.as_deref(), expr.span).map(|_| ());
        }
        // `conn.write(line) else: continue`: what fails gives nothing back when it doesn't.
        if let ExprKind::Else { value, default, .. } = &expr.kind
            && self.type_at(expr.span) == Some(Type::Unit)
        {
            return self.else_effect(value, default, expr.span);
        }
        self.expr(expr).map(|_| ())
    }

    /// A duration literal: `30s` is `Duration(nanos=30_000_000_000)`.
    fn duration(&mut self, nanos: i64, expr: &Expr) -> Lowered<Operand> {
        let dest = self.temporary(self.ty(expr)?);
        let fields = vec![Operand::Int(nanos)];
        self.emit(InstKind::Make { dest, type_name: "Duration".to_owned(), fields }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// A call that makes its value itself rather than calling a function: `copy(x, field=...)`, a
    /// consumer like `sum(...)`, or `Ok(...)` or `Err(...)` written out.
    fn made_by_name(&mut self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Lowered<Option<Operand>> {
        // `list[int]()`, `dict[str, int]()` or `set[str]()`: an empty one, its type the call's.
        if let Some(empty) = typed_empty(expr) {
            return self.expr(&empty).map(Some);
        }
        // `copy(value, field=new)` copies with fields changed. `replace` was its old name.
        if let ExprKind::Name(name) = &callee.kind
            && (name == "replace" || name == "copy" && args.len() > 1)
            && self.lookup(name).is_none()
            && !self.context.functions.contains(name.as_str())
        {
            return self.replace(args, expr.span).map(Some);
        }
        if let Some(value) = self.consumer(expr, callee, args)? {
            return Ok(Some(value));
        }
        self.written_result(expr, callee, args)
    }

    /// A name's value: its local as what's known of it here, or else a constant.
    fn read_name(&mut self, name: &str, expr: &Expr) -> Lowered<Operand> {
        let Some(local) = self.lookup(name) else { return self.read_constant(name, expr) };
        self.narrowed(Operand::Local(local), expr)
    }

    fn expr(&mut self, expr: &Expr) -> Lowered<Operand> {
        match builtin_as_method(expr) {
            Some(method) => self.expr_as_written(&method),
            None => self.expr_as_written(expr),
        }
    }

    /// Lowers an expression whose `len(x)` or `abs(x)`, if it is one, is already the method it calls.
    fn expr_as_written(&mut self, expr: &Expr) -> Lowered<Operand> {
        let span = expr.span;
        if let Some((owner, case, args)) = self.built_case(expr) {
            return self.make_case(&owner, case, args, span);
        }
        match &expr.kind {
            ExprKind::Int(value) => Ok(Operand::Int(*value)),
            ExprKind::Float(value) => Ok(Operand::Float(*value)),
            ExprKind::Duration(nanos) => self.duration(*nanos, expr),
            ExprKind::Bytes(data) => Ok(Operand::Bytes(data.clone())),
            ExprKind::Folder(files) => Ok(self.folder(files, expr.span)),
            ExprKind::Bool(value) => Ok(Operand::Bool(*value)),
            ExprKind::Str(value) => Ok(Operand::Str(value.clone())),
            ExprKind::None => Ok(Operand::None),
            ExprKind::Name(name) => self.read_name(name, expr),
            ExprKind::Binary { op: op @ (BinOp::Is | BinOp::IsNot), lhs, rhs } => {
                self.is_none(*op, lhs, rhs, span)
            }
            ExprKind::Else { value, default, .. } => self.else_value(expr, value, default),
            ExprKind::Try { value, context } => self
                .try_value(value, context.as_deref(), span)?
                .ok_or_else(|| "it uses the nothing `?` gives back".to_owned()),
            ExprKind::Unary { op, expr: inner } => self.unary(*op, inner, expr),
            ExprKind::Binary { op: op @ (BinOp::And | BinOp::Or), lhs, rhs } => {
                self.short_circuit(*op, lhs, rhs, span)
            }
            ExprKind::Binary { op: op @ (BinOp::In | BinOp::NotIn), lhs, rhs } => {
                self.contains(*op, lhs, rhs, span)
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let ty = self.ty(expr)?;
                let lhs = self.expr(lhs)?;
                let sides = self.both(lhs, |lowering| lowering.expr(rhs), span)?;
                self.operate(*op, sides, &ty, span)
            }
            ExprKind::Cond { then, cond, otherwise } => self.conditional(expr, (then, cond, otherwise)),
            ExprKind::Attr { object, name, optional: false } => self.get_field(expr, object, name),
            ExprKind::Attr { object, name, optional: true } => self.optional_field(expr, object, name),
            ExprKind::Match { subject, arms } => {
                let dest = self.temporary(self.ty(expr)?);
                self.match_expr(subject, arms, Tail::Into(dest))?;
                Ok(Operand::Local(dest))
            }
            ExprKind::Call { callee, args } => {
                if let Some(type_name) = self.constructs(callee, span) {
                    return self.make(&type_name, args, span);
                }
                if let Some(copied) = self.copy_call(expr, callee, args)? {
                    return Ok(copied);
                }
                if let Some(made) = self.made_by_name(expr, callee, args)? {
                    return Ok(made);
                }
                let collected = self.collected_args(callee, args);
                let args = collected.as_deref().unwrap_or(args);
                let dest = self.call(callee, args, Some(self.ty(expr)?), span)?;
                dest.map(Operand::Local).ok_or_else(|| "a call kept nothing".to_owned())
            }
            ExprKind::List(items) => self.list(expr, items),
            ExprKind::Set(items) => self.set(expr, items),
            ExprKind::Dict(pairs) => self.dict(expr, pairs),
            ExprKind::Tuple(items) => self.tuple(expr, items),
            ExprKind::Comprehension { kind, elem, value, names, iter, cond } => {
                if let (fennec_syntax::CompKind::List, None) = (kind, value)
                    && let Some(outlined) =
                        self.parallel_value(expr, (elem, names, iter, cond.as_deref()), parallel::Gives::List)
                {
                    return outlined;
                }
                let parts = collections::Parts { elem, value: value.as_deref(), cond: cond.as_deref() };
                self.comprehension(expr, *kind, &parts, (names, iter))
            }
            ExprKind::Lambda { params, body } => self.arrow(expr, params, body, false),
            ExprKind::Index { object, index } => self.get_index(expr, object, index),
            ExprKind::Slice { object, start, end } => {
                self.slice(expr, object, (start.as_deref(), end.as_deref()))
            }
            ExprKind::FStr(parts) => self.format(parts, span),
            other => Err(format!("it uses {}", describe(other))),
        }
    }

    /// `copy(value)`: values are copies here, so it's the value in a local of its own.
    fn copy_call(&mut self, expr: &Expr, callee: &Expr, args: &[Arg]) -> Lowered<Option<Operand>> {
        let (ExprKind::Name(name), [arg]) = (&callee.kind, args) else { return Ok(None) };
        if name != "copy" || self.lookup(name).is_some() || self.context.functions.contains("copy") {
            return Ok(None);
        }
        let value = self.expr(&arg.value)?;
        let dest = self.temporary(self.ty(expr)?);
        if let Some(local) = self.locals.get_mut(dest.0) {
            local.copied = true;
        }
        self.emit(InstKind::Copy { dest, value }, expr.span);
        Ok(Some(Operand::Local(dest)))
    }

    fn unary(&mut self, op: UnaryOp, inner: &Expr, expr: &Expr) -> Lowered<Operand> {
        let ty = self.ty(expr)?;
        let value = self.expr(inner)?;
        let dest = self.temporary(ty);
        self.emit(InstKind::Unary { dest, op, value }, expr.span);
        Ok(Operand::Local(dest))
    }

    /// `and` and `or`: the right side only runs when the left doesn't settle it.
    fn short_circuit(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span) -> Lowered<Operand> {
        let dest = self.temporary(Type::Bool);
        let value = self.expr(lhs)?;
        self.emit(InstKind::Copy { dest, value }, span);
        let right = self.new_block();
        let join = self.new_block();
        let cond = Operand::Local(dest);
        self.finish(if op == BinOp::And {
            Terminator::Branch { cond, then: right, otherwise: join }
        } else {
            Terminator::Branch { cond, then: join, otherwise: right }
        });
        self.current = right;
        let value = self.expr(rhs)?;
        self.emit(InstKind::Copy { dest, value }, span);
        self.finish(Terminator::Jump(join));
        self.current = join;
        Ok(Operand::Local(dest))
    }

    /// `then if cond else otherwise`.
    fn conditional(
        &mut self,
        expr: &Expr,
        (then, cond, otherwise): (&Expr, &Expr, &Expr),
    ) -> Lowered<Operand> {
        let dest = self.temporary(self.ty(expr)?);
        let cond = self.expr(cond)?;
        let (yes, no, join) = (self.new_block(), self.new_block(), self.new_block());
        self.finish(Terminator::Branch { cond, then: yes, otherwise: no });
        for (block, side) in [(yes, then), (no, otherwise)] {
            self.current = block;
            let value = self.expr(side)?;
            let ty = self.local_type(dest);
            let value = self.coerce(value, &ty, side.span)?;
            self.emit(InstKind::Copy { dest, value }, side.span);
            self.finish(Terminator::Jump(join));
        }
        self.current = join;
        Ok(Operand::Local(dest))
    }
}

/// What an expression that isn't lowered yet is, for saying why.
fn describe(kind: &ExprKind) -> &'static str {
    match kind {
        ExprKind::FStr(_) => "an f-string",
        ExprKind::None => "`None`",
        ExprKind::Variant(_) => "an enum case",
        ExprKind::Else { .. } => "`else`",
        ExprKind::Try { .. } => "`?`",
        ExprKind::Attr { .. } => "a field or method",
        _ => "something that isn't lowered yet",
    }
}

/// The blocks control can reach, in order, with their targets renumbered. A join that every
/// branch left early never gets used, so it goes.
fn reachable(blocks: Vec<Open>) -> Vec<Block> {
    let ends = |block: &Open| block.end.as_ref().map_or_else(Vec::new, Terminator::targets);
    let mut seen = vec![false; blocks.len()];
    let mut waiting = vec![BlockId(0)];
    while let Some(block) = waiting.pop() {
        if let Some(slot) = seen.get_mut(block.0)
            && !*slot
        {
            *slot = true;
            waiting.extend(blocks.get(block.0).map(ends).unwrap_or_default());
        }
    }
    let mut renumbered = HashMap::new();
    for (at, _) in seen.iter().enumerate().filter(|(_, seen)| **seen) {
        renumbered.insert(at, BlockId(renumbered.len()));
    }
    let moved = |block: BlockId| renumbered.get(&block.0).copied().unwrap_or(block);
    blocks
        .into_iter()
        .zip(seen)
        .filter(|(_, seen)| *seen)
        .map(|(block, _)| Block {
            insts: block.insts,
            end: match block.end {
                Some(Terminator::Jump(target)) => Terminator::Jump(moved(target)),
                Some(Terminator::Branch { cond, then, otherwise }) => {
                    Terminator::Branch { cond, then: moved(then), otherwise: moved(otherwise) }
                }
                Some(other) => other,
                None => Terminator::Unreachable,
            },
        })
        .collect()
}

/// The `shared` types a module declares that take a lock. One the standard library ships, like a
/// connection, is a resource: lowered like a struct around what it holds, with no lock, and a
/// `with` on it runs its `del` when the block ends.
fn shared_types(module: &Module) -> Vec<String> {
    module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Struct(def) if def.shared && !def.span.in_prelude() => Some(def.name.clone()),
            _ => None,
        })
        .collect()
}

/// `list[int]()`, `dict[str, int]()` or `set[str]()` as the empty one it makes, with the call's
/// span, whose type the checker gave: the checker leaves the call as written, so a check of
/// the program again still knows what it holds.
fn typed_empty(expr: &Expr) -> Option<Expr> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Index { object, .. } = &callee.kind else { return None };
    let ExprKind::Name(name) = &object.kind else { return None };
    if !args.is_empty() || !matches!(name.as_str(), "list" | "dict" | "set") {
        return None;
    }
    let mut empty = expr.clone();
    fennec_syntax::visit::empty_collection(&mut empty);
    Some(empty)
}

/// `len(x)` and `abs(x)` call `x`'s own method, which the checker writes in when it resolves a
/// module: one lowered as it was checked reads them so too. The call as that method, if it's one.
fn builtin_as_method(expr: &Expr) -> Option<Expr> {
    let ExprKind::Call { callee, args } = &expr.kind else { return None };
    let ExprKind::Name(name) = &callee.kind else { return None };
    let [arg] = args.as_slice() else { return None };
    if !matches!(name.as_str(), "len" | "abs") || arg.name.is_some() || arg.is_mut {
        return None;
    }
    let mut method = expr.clone();
    fennec_syntax::visit::method_of_call(&mut method);
    Some(method)
}
