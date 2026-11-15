//! A `std.net` `Router`'s routes, checked where they're written. `router.get("/notes/{id}",
//! show_note)` names each of a route's parameters in its path, and its handler takes the `Request`,
//! then each of those by name, in any order, each a `str`, an `int` or a sized int, and gives back
//! a `Response`. An arrow's parameters can't be typed, so an arrow handler takes each as a `str`.
//!
//! Once the program checks, each route is written in as `router.handle("GET /notes/{id}", ...)`,
//! with a function made for it that reads each parameter as its handler takes it and calls the
//! handler. A parameter that doesn't read as what it's taken as, like `abc` for an `int`, answers
//! 404, as a path no route has does.

use std::collections::BTreeMap;

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, FnDef, Import, Item, Module, Span, TypeExpr};

use crate::checker::Checker;
use crate::function::FnCx;
use crate::types::{Type, a};

/// The methods a route is written with, each with the HTTP method it answers.
const METHODS: [(&str, &str); 5] =
    [("get", "GET"), ("post", "POST"), ("put", "PUT"), ("patch", "PATCH"), ("delete", "DELETE")];

/// The most parameters a route has: as many as `Captured` holds.
const MOST_PARAMETERS: usize = 8;

/// The name `std.net` is imported under for what's written in, which no program can write.
const NET: &str = "net__route";

/// A route as written, once it checks, for what's written in for it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Route {
    /// The HTTP method it answers, as `GET`.
    pub(crate) method: &'static str,
    pub(crate) path: String,
    /// The route's parameters, in the path's order.
    pub(crate) params: Vec<String>,
    /// The handler's parameters after its `Request`, in its own order, each with its type.
    pub(crate) takes: Vec<(String, Type)>,
}

/// A route checked so far in one function, for two on one router that clash.
pub(crate) struct Routed {
    router: String,
    method: &'static str,
    path: String,
    /// Each segment of its path: the text written out, or `None` for a parameter, with its name.
    segments: Vec<(Option<String>, String)>,
}

/// Whether `method` called on a value of the type named `owner` is a route written with a method,
/// as `router.get(...)` on a `std.net` `Router`.
pub(crate) fn is_route(checker: &Checker, owner: &str, method: &str) -> bool {
    owner == "Router"
        && checker.program.type_info(owner).is_some_and(|info| info.prelude)
        && METHODS.iter().any(|(name, _)| *name == method)
}

/// Whether a name can be a route's parameter: a name a program can write for a local.
fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase() || first == '_')
        && chars.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '_')
}

/// Each segment of a route's path: the text written out, or `None` for a parameter, with its name.
fn segments(path: &str) -> Vec<(Option<String>, String)> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| match segment.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')) {
            Some(name) => (None, name.to_owned()),
            None => (Some(segment.to_owned()), segment.to_owned()),
        })
        .collect()
}

impl FnCx<'_> {
    /// `router.get(path, handler)`: a route, checked against its handler and the routes before it
    /// on the same router here, and noted to be written in as a call to `handle`.
    pub(crate) fn route_call(&mut self, object: &Expr, method: &str, args: &[Arg], span: Span) -> Type {
        let Some((_, verb)) = METHODS.iter().find(|(name, _)| *name == method) else { return Type::Unknown };
        self.mut_place(object, "adding a route changes the router, so it's held in a mut name");
        let [path, handler] = args else {
            let message = format!("`router.{method}` takes a route's path and its handler");
            let help = format!("write it as `router.{method}(\"/notes/{{id}}\", show_note)`");
            self.error(Diagnostic::new(message, span).with_help(help));
            self.ignore_args(args);
            return Type::Unit;
        };
        if [path, handler].iter().any(|arg| arg.name.is_some() || arg.is_mut) {
            let message = format!("`router.{method}` takes a route's path and its handler, as they are");
            self.error(Diagnostic::new(message, span));
            self.ignore_args(args);
            return Type::Unit;
        }
        let Some((written, params)) = self.route_path(&path.value) else {
            self.ignore_args(std::slice::from_ref(handler));
            return Type::Unit;
        };
        let Some(takes) = self.route_handler(handler, (&written, &params)) else { return Type::Unit };
        self.route_clash(object, verb, &written, path.value.span);
        let route = Route { method: verb, path: written, params, takes };
        self.checker.routes.insert((span.start, span.end), route);
        Type::Unit
    }

    /// A route's path, written out, and its parameters in order, or `None` once what's wrong
    /// with it is said.
    fn route_path(&mut self, value: &Expr) -> Option<(String, Vec<String>)> {
        let ExprKind::Str(path) = &value.kind else {
            let message = "a route's path is written out, so it's checked against its handler here";
            let help = "write it as text in quotes, as `\"/notes/{id}\"`";
            self.error(Diagnostic::new(message, value.span).with_help(help));
            return None;
        };
        let fail = |cx: &mut Self, message: String, help: &str| {
            cx.error(Diagnostic::new(message, value.span).with_help(help));
            None
        };
        if !path.starts_with('/') {
            return fail(
                self,
                format!("`{path}` doesn't start with `/`"),
                "a route's path starts at `/`, as `/notes/{id}`",
            );
        }
        let mut params: Vec<String> = Vec::new();
        for segment in path.split('/').filter(|segment| !segment.is_empty() && segment.contains(['{', '}'])) {
            let name = segment
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
                .filter(|name| is_name(name));
            let Some(name) = name else {
                let message = format!("`{segment}` in `{path}` isn't a parameter");
                return fail(self, message, "a parameter is a whole segment, a name in braces, as `{id}`");
            };
            if params.iter().any(|had| had == name) {
                return fail(
                    self,
                    format!("`{path}` has `{{{name}}}` twice"),
                    "give each parameter a name of its own",
                );
            }
            params.push(name.to_owned());
        }
        if params.len() > MOST_PARAMETERS {
            let message = format!(
                "`{path}` has {} parameters, but a route has at most {MOST_PARAMETERS}",
                params.len()
            );
            return fail(
                self,
                message,
                "a handler reads its route's parameters from `Captured`, which holds 8",
            );
        }
        Some((path.clone(), params))
    }

    /// What a route's handler takes after its `Request`, in its own order, or `None` once what's
    /// wrong with it is said: a `def` or an arrow taking the route's parameters by name and giving
    /// back a `Response`.
    fn route_handler(&mut self, arg: &Arg, (path, params): (&str, &[String])) -> Option<Vec<(String, Type)>> {
        let (request, response) = (Type::named("Request"), Type::named("Response"));
        let handler = &arg.value;
        if let ExprKind::Lambda { params: names, .. } = &handler.kind {
            let Some((_, rest)) = names.split_first() else {
                let message = "this arrow takes nothing, but a route's handler takes the `Request` first";
                self.error(
                    Diagnostic::new(message, handler.span).with_help("write it as `(request) => ...`"),
                );
                return None;
            };
            let ok = self.route_names(rest, params, (path, "this arrow", handler.span));
            let mut types = vec![request];
            types.extend(rest.iter().map(|_| Type::Str));
            let ty = self.arg_value(arg, &types);
            if let Type::Arrow(_, returns) = self.checker.settled(&ty)
                && !self.checker.fits(&response, &returns)
            {
                self.not_a_response("this arrow", &returns, handler.span);
                return None;
            }
            return ok.then(|| rest.iter().map(|name| (name.clone(), Type::Str)).collect());
        }
        let ty = self.expr(handler);
        let Type::Function(key) = self.checker.settled(&ty) else {
            let message = format!("a route's handler is a function, but this is {}", a(&ty));
            let help = "pass a `def` by its name, as `show_note`, or an arrow, as `(request) => ...`";
            self.error(Diagnostic::new(message, handler.span).with_help(help));
            return None;
        };
        let signature = self.checker.signature(&key)?;
        let shown = fennec_syntax::without_kits(&key);
        let Some((first, rest)) = signature.params.split_first() else {
            let message = format!("`{shown}` takes nothing, but a route's handler takes the `Request` first");
            self.error(Diagnostic::new(message, handler.span));
            return None;
        };
        // What it takes after isn't read as the route's parameters when what it takes first isn't
        // the `Request`.
        if self.checker.settled(&first.ty) != request {
            let message = format!(
                "`{shown}` takes `{}` first, but a route's handler takes the `Request` first",
                first.name
            );
            self.error(Diagnostic::new(message, handler.span));
            return None;
        }
        let mut ok = true;
        let names: Vec<String> = rest.iter().map(|param| param.name.clone()).collect();
        ok &= self.route_names(&names, params, (path, &format!("`{shown}`"), handler.span));
        for param in rest {
            let ty = self.checker.settled(&param.ty);
            if !matches!(ty, Type::Str | Type::Int) && !matches!(ty, Type::Sized(num) if !num.is_float()) {
                let message = format!(
                    "`{shown}` takes `{}` as {}, but a route's parameter is read as text or a number",
                    param.name,
                    a(&ty)
                );
                let help = "take it as a `str`, an `int` or a sized int, as `id: int`";
                self.error(Diagnostic::new(message, handler.span).with_help(help));
                ok = false;
            }
        }
        let returns = self.checker.settled(&signature.returns);
        if !self.checker.fits(&response, &returns) {
            self.not_a_response(&format!("`{shown}`"), &returns, handler.span);
            ok = false;
        }
        ok.then(|| rest.iter().map(|param| (param.name.clone(), self.checker.settled(&param.ty))).collect())
    }

    /// Refuses a handler that gives back something other than a `Response`. One that can fail is
    /// told to handle its failures, as a route answers every request it's given.
    fn not_a_response(&mut self, who: &str, returns: &Type, at: Span) {
        let diagnostic = if let Type::Result(..) = returns {
            let message = format!("{who} can fail, but a route's handler answers every request it's given");
            let help = "handle its failures with `except`, and give back a `Response` for each";
            Diagnostic::new(message, at).with_help(help)
        } else {
            let message =
                format!("{who} gives back {}, but a route's handler gives back a `Response`", a(returns));
            Diagnostic::new(message, at)
        };
        self.error(diagnostic);
    }

    /// Whether a handler's parameters after its `Request` are just its route's, by name: each
    /// route parameter taken, and nothing else. Says what isn't.
    fn route_names(
        &mut self,
        names: &[String],
        params: &[String],
        (path, who, at): (&str, &str, Span),
    ) -> bool {
        let mut ok = true;
        for param in params.iter().filter(|param| !names.contains(param)) {
            let message = format!("`{path}` has `{{{param}}}`, but {who} has no parameter called `{param}`");
            self.error(
                Diagnostic::new(message, at).with_help(format!("take it as `{param}`, after the `Request`")),
            );
            ok = false;
        }
        for name in names.iter().filter(|name| !params.contains(name)) {
            let message = format!("{who} takes `{name}`, but `{path}` has no `{{{name}}}`");
            self.error(
                Diagnostic::new(message, at)
                    .with_help("a handler takes the `Request`, then its route's parameters by name"),
            );
            ok = false;
        }
        ok
    }

    /// Refuses a route that clashes with one before it on the same router in this function: the
    /// same method on a path of the same shape, which routing would refuse when it runs, or a
    /// parameter at the same place under another name.
    fn route_clash(&mut self, object: &Expr, method: &'static str, path: &str, at: Span) {
        let Some(router) = crate::stmt::path(object) else { return };
        let route = Routed { router, method, path: path.to_owned(), segments: segments(path) };
        let mut before = self.routes_here.iter().filter(|before| before.router == route.router);
        if let Some((message, help)) = before.find_map(|before| clash(before, &route)) {
            self.error(Diagnostic::new(message, at).with_help(help));
        }
        self.routes_here.push(route);
    }
}

/// What's wrong with `route` beside `before` on the same router, with what to do about it: the same
/// method on a path of the same shape, or a parameter in a place the two share under another name.
fn clash(before: &Routed, route: &Routed) -> Option<(String, String)> {
    let shape = |segments: &[(Option<String>, String)]| -> Vec<Option<String>> {
        segments.iter().map(|(written, _)| written.clone()).collect()
    };
    let (method, path) = (route.method, &route.path);
    if before.method == method && shape(&before.segments) == shape(&route.segments) {
        let message =
            format!("`{method} {path}` is routed twice: `{method} {}` is the same route", before.path);
        return Some((message, "give each route a path of its own".to_owned()));
    }
    // Down the places the two paths share, a parameter has the same name in each.
    for ((written, name), (had_written, had_name)) in route.segments.iter().zip(&before.segments) {
        match (written, had_written) {
            (None, None) if name != had_name => {
                let message = format!(
                    "`{path}` calls the parameter in this place `{{{name}}}`, but `{}` calls it `{{{had_name}}}`",
                    before.path
                );
                return Some((
                    message,
                    format!("call it `{{{had_name}}}` here too: routes share a parameter in the same place"),
                ));
            }
            (None, None) => {}
            (written, had_written) if written == had_written => {}
            _ => return None,
        }
    }
    None
}

/// Writes each route in as a call to `handle`, with a function made for it that reads its
/// parameters and calls its handler, saying whether there were any.
pub(crate) fn lower(module: &mut Module, routes: &BTreeMap<(u32, u32), Route>) -> bool {
    if routes.is_empty() {
        return false;
    }
    let mut made: Vec<(String, Route)> = Vec::new();
    fennec_syntax::visit::exprs_mut(module, &mut |expr| {
        let Some(route) = routes.get(&(expr.span.start, expr.span.end)) else { return };
        let span = expr.span;
        let ExprKind::Call { callee, args } = &mut expr.kind else { return };
        let (ExprKind::Attr { name, .. }, [_, handler]) = (&mut callee.kind, args.as_mut_slice()) else {
            return;
        };
        let adapter = format!("route__{}", span.start);
        let here = |span: Span| crate::lend_lowering::here(span);
        let named = |name: &str| Expr { kind: ExprKind::Name(name.to_owned()), span: here(span) };
        let call = ExprKind::Call {
            callee: Box::new(named(&adapter)),
            args: [named("request__route"), named("found__route"), handler.value.clone()]
                .into_iter()
                .map(|value| Arg { name: None, is_mut: false, value })
                .collect(),
        };
        let arrow = ExprKind::Lambda {
            params: vec!["request__route".to_owned(), "found__route".to_owned()],
            body: Box::new(Expr { kind: call, span: here(span) }),
        };
        let written = format!("{} {}", route.method, route.path);
        "handle".clone_into(name);
        let path =
            Arg { name: None, is_mut: false, value: Expr { kind: ExprKind::Str(written), span: here(span) } };
        let answering = Arg { name: None, is_mut: false, value: Expr { kind: arrow, span: here(span) } };
        *args = vec![path, answering];
        made.push((adapter, route.clone()));
    });
    let mut wrote = false;
    for (name, route) in made {
        if let Some(def) = adapter(&name, &route) {
            module.items.push(Item::Function(def));
            wrote = true;
        }
    }
    let imported = module
        .items
        .iter()
        .any(|item| matches!(item, Item::Import(import) if import.alias.as_deref() == Some(NET)));
    if wrote && !imported {
        let import = Import {
            module: vec!["std".to_owned(), "net".to_owned()],
            names: Vec::new(),
            aliases: Vec::new(),
            alias: Some(NET.to_owned()),
            span: Span::default(),
        };
        module.items.insert(0, Item::Import(import));
    }
    wrote
}

/// The function a route's handler is called through: it reads each of the route's parameters as
/// the handler takes it, answering 404 for one that doesn't read as that, and calls the handler.
fn adapter(name: &str, route: &Route) -> Option<FnDef> {
    let types: Vec<String> = route.takes.iter().map(|(_, ty)| ty.to_string()).collect();
    let mut lines = vec![format!(
        "def {name}(request__route: {NET}.Request, found__route: {NET}.Captured, handler__route: ({NET}.Request{}) -> {NET}.Response) -> {NET}.Response:",
        types.iter().flat_map(|ty| [", ", ty.as_str()]).collect::<String>()
    )];
    for (at, param) in route.params.iter().enumerate() {
        let ty = route.takes.iter().find(|(taken, _)| taken == param).map(|(_, ty)| ty)?;
        let text = format!("found__route.text_at({at})");
        lines.push(match ty {
            Type::Str => format!("    {param}__given = {text}"),
            ty => format!("    {param}__given = {ty}.parse({text}) except InvalidValue(): return {NET}.Response.not_found()"),
        });
    }
    let given: String = route.takes.iter().flat_map(|(param, _)| [", ", param.as_str(), "__given"]).collect();
    lines.push(format!("    return handler__route(request__route{given})"));
    let text = lines.join("\n") + "\n";
    let module = fennec_syntax::parse(&text).ok()?;
    let Some(Item::Function(mut def)) = module.items.into_iter().next() else { return None };
    // Spans of its own, in what lowering writes, so the types it's checked again with, found by
    // span, are never another's.
    let length = u32::try_from(text.len()).ok()? + 1;
    let base = crate::lend_lowering::reserved(length);
    let moved = |span: Span| Span { start: span.start + base, end: span.end + base };
    def.span = moved(def.span);
    for param in &mut def.params {
        param.span = moved(param.span);
        if let Some(ty) = &mut param.ty {
            respan_type(ty, &moved);
        }
    }
    if let Some(ty) = &mut def.ret {
        respan_type(ty, &moved);
    }
    fennec_syntax::visit::written_types_in(&mut def.body, &mut |ty| respan_type(ty, &moved));
    fennec_syntax::visit::respan_stmts(&mut def.body, &mut |span| moved(span));
    Some(def)
}

/// A written type, and every type in it, at spans moved through `moved`.
fn respan_type(ty: &mut TypeExpr, moved: &dyn Fn(Span) -> Span) {
    ty.span = moved(ty.span);
    for arg in &mut ty.args {
        respan_type(arg, moved);
    }
}
