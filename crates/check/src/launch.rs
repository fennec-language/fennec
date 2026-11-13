//! `with launch(...)`: a task run beside its caller until the `with` block ends. The arrow is
//! the call you'd write anyway, and a parameter of it typed `Inbox[T]` makes the channel: the
//! task gets the receiving end, and the `as` name the sending end, a `Sender[T]`. `stop_after=`
//! and `inbox_size=` are `launch`'s own. The block fails with the task's errors and `Crashed`.
//!
//! Once checked, it's written in as a `with` source made for it (see `source`), so it runs as
//! any source does, around `std.intrinsics`' `launch` and `launch_end`.

use std::sync::Arc;

use fennec_syntax::{Arg, Diagnostic, Expr, ExprKind, Item, Span};

use crate::function::FnCx;
use crate::infer::Hole;
use crate::types::{Type, a};

/// What a `with launch(...)` was checked to be, for lowering to write in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Launch {
    /// What its inbox takes, when the task has one.
    pub(crate) message: Option<Type>,
    /// What the task's call gives back, which nothing reads.
    pub(crate) value: Type,
    /// What the task can fail with, when it can.
    pub(crate) error: Option<Type>,
    /// The arrow, as written: the task's call, and the name of its inbox, when it has one.
    pub(crate) task: Expr,
}

impl Launch {
    /// Each type it holds, settled once the check is done.
    pub(crate) fn settled(&self, settle: &dyn Fn(&Type) -> Type) -> Launch {
        Launch {
            message: self.message.as_ref().map(settle),
            value: settle(&self.value),
            error: self.error.as_ref().map(settle),
            task: self.task.clone(),
        }
    }
}

impl FnCx<'_> {
    /// `launch(task, stop_after=..., inbox_size=...)`, as what a `with` takes: what it lends,
    /// the task's `Sender[T]` or nothing, failing with the task's errors and `Crashed`.
    pub(crate) fn launch(&mut self, args: &[Arg], span: Span) -> Type {
        if !self.with_values.contains(&(span.start, span.end)) {
            self.error(
                Diagnostic::new("`launch` is only ever written as `with launch(...)`", span)
                    .with_help("a task runs until its `with` block ends: `with launch(() => work(...))?:`"),
            );
        }
        if let Some(held) = self.lock.clone() {
            self.error(
                Diagnostic::new(
                    format!("a `with launch` waits for its task where it ends, so not {held}"),
                    span,
                )
                .with_help("launch it before taking the lock, or after"),
            );
        }
        let Some((task, sized)) = self.launch_options(args, span) else { return Type::Unknown };
        let ExprKind::Lambda { params, body } = &task.kind else {
            self.expr(task);
            self.error(
                Diagnostic::new("`launch` takes an arrow: the call you'd write anyway", task.span).with_help(
                    "write `() => work(...)`, or `(inbox) => work(inbox, ...)` for a task with an inbox",
                ),
            );
            return Type::Unknown;
        };
        if params.len() > 1 {
            self.error(
                Diagnostic::new("a task takes at most one parameter, its inbox", task.span).with_help(
                    "pass anything else it needs inside the arrow: `(inbox) => work(inbox, speakers)`",
                ),
            );
        }
        let inbox: Vec<Type> = params
            .first()
            .map(|name| self.checker.fresh(task.span, Hole::TypeParam(name.clone())))
            .into_iter()
            .collect();
        let arrow = self.lambda(params, body, &inbox, task.span);
        let returns = match arrow {
            Type::Arrow(_, returns) => self.checker.settled(&returns),
            _ => Type::Unknown,
        };
        let message = self.inbox_message(inbox.first(), task.span);
        if let (Some(at), None) = (sized, &message) {
            self.error(
                Diagnostic::new("this task takes no inbox, so it has no `inbox_size=`", at).with_help(
                    "give the task a parameter typed `Inbox[Message]`, or leave `inbox_size=` out",
                ),
            );
        }
        let (value, error) = match returns {
            Type::Result(value, error) => (*value, Some(*error)),
            value => (value, None),
        };
        if self.checker.program.type_info("Crashed").is_none() {
            self.error(
                Diagnostic::new("`with launch` fails with `Crashed`, from `std.concurrent`", span)
                    .with_help("write `from std.concurrent import Crashed`"),
            );
        }
        let fails = with_crashed(error.as_ref());
        let launch = Launch { message: message.clone(), value, error, task: task.clone() };
        self.checker.launches.insert((span.start, span.end), launch);
        let lent = message.map_or(Type::Unit, |message| Type::Generic("Sender".to_owned(), vec![message]));
        Type::result(Type::Generic(crate::lend::WITH.to_owned(), vec![lent]), fails)
    }

    /// The task and where `inbox_size=` is given, checking `launch`'s options.
    fn launch_options<'a>(&mut self, args: &'a [Arg], span: Span) -> Option<(&'a Expr, Option<Span>)> {
        let mut task = None;
        let mut sized = None;
        for arg in args {
            match arg.name.as_deref() {
                None if task.is_none() => task = Some(&arg.value),
                Some("stop_after") => {
                    self.expr_as(&arg.value, &Type::named("Duration"));
                }
                Some("inbox_size") => {
                    self.expr_as(&arg.value, &Type::Int);
                    sized = Some(arg.value.span);
                }
                _ => {
                    self.expr(&arg.value);
                    let message = "`launch` takes the task, then `stop_after=` and `inbox_size=`";
                    let help = "pass anything else the task needs inside its arrow: `(inbox) => work(inbox, speakers)`";
                    self.error(Diagnostic::new(message, arg.value.span).with_help(help));
                }
            }
        }
        let Some(task) = task else {
            self.error(
                Diagnostic::new("`launch` takes the task to run, as an arrow", span).with_help(
                    "write the call you'd make anyway: `with launch(() => autosave(rooms, fs))?:`",
                ),
            );
            return None;
        };
        Some((task, sized))
    }

    /// What the task's inbox takes, from the type its parameter settled to.
    fn inbox_message(&mut self, inbox: Option<&Type>, span: Span) -> Option<Type> {
        match inbox.map(|ty| self.checker.settled(ty)) {
            Some(Type::Generic(name, held)) if name == "Inbox" && held.len() == 1 => held.into_iter().next(),
            Some(other) if other.is_known() => {
                let message =
                    format!("a task's parameter is its inbox, an `Inbox[T]`, but this is {}", a(&other));
                self.error(Diagnostic::new(message, span).with_help(
                    "type the task's parameter `Inbox[Message]`, or pass the value inside the arrow instead",
                ));
                None
            }
            _ => None,
        }
    }
}

/// A task's errors and `Crashed`, which every task can end with.
fn with_crashed(error: Option<&Type>) -> Type {
    let mut names = match error {
        Some(Type::Named(name)) => vec![name.clone()],
        Some(Type::Errors(names)) => names.clone(),
        _ => Vec::new(),
    };
    names.push("Crashed".to_owned());
    names.sort();
    names.dedup();
    match names.as_slice() {
        [one] => Type::named(one),
        _ => Type::Errors(names),
    }
}

/// The `with` source a checked `with launch(...)` is written in as, around the intrinsics: it
/// makes the channel, starts the task with the receiving end, lends the sending end, and where
/// the block ends closes the channel, waits for the task and gives back how it ended. A block
/// that fails cuts the task short at once.
///
/// Its code is the same text for every launch, read as the standard library's, with a slot where
/// each type goes; the checked types are put in as written types built straight from them, never
/// as text, so a program's type named like the library's, a dependency's, or any other the
/// checker holds under a name a program can't write comes through as it is.
pub(crate) fn source(launch: &Launch) -> Option<crate::lend_lowering::Source> {
    let mut module = fennec_syntax::parse_prelude(&source_text(launch.message.is_some())).ok()?;
    // `intrinsics.launch` is the intrinsic's own name, as the standard library's calls to it are.
    fennec_syntax::visit::exprs_mut(&mut module, &mut |expr| {
        if let ExprKind::Attr { object, name, optional: false } = &expr.kind
            && let ExprKind::Name(module) = &object.kind
            && module == "intrinsics"
        {
            expr.kind = ExprKind::Name(format!("intrinsics.{name}"));
        }
        // A `_` is a hidden name of its own, as a program's are once loaded, which lowering
        // numbers apart at each place the `else:` is written in.
        if matches!(&expr.kind, ExprKind::Name(name) if name == "_") {
            expr.kind = ExprKind::Name(format!("_{}", fennec_syntax::visit::HIDDEN));
        }
    });
    // What `launch_end` gives back on a failure: the task's error, or `Crashed` for a task that
    // can't fail, whose code 2 never comes.
    let ended = launch.error.clone().unwrap_or_else(|| Type::named("Crashed"));
    let fails = with_crashed(launch.error.as_ref());
    let mut slots = vec![(ENDED, ended), (FAILS, fails)];
    slots.extend(launch.message.clone().map(|message| (MESSAGE, message)));
    let mut items = module.items.into_iter();
    let Some(Item::Function(mut def)) = items.next() else { return None };
    fill(&mut def, &slots)?;
    let inbox = match items.next() {
        Some(Item::Function(mut maker)) => {
            fill(&mut maker, &slots)?;
            let Some(fennec_syntax::StmtKind::Return(Some(made))) =
                maker.body.into_iter().next().map(|stmt| stmt.kind)
            else {
                return None;
            };
            Some(made)
        }
        _ => None,
    };
    with_task(&mut def, launch, inbox.as_ref())?;
    let lent =
        launch.message.clone().map_or(Type::Int, |message| Type::Generic("Sender".to_owned(), vec![message]));
    Some(crate::lend_lowering::Source {
        def: Arc::new(def),
        method: false,
        gives: Type::Generic(crate::lend::WITH.to_owned(), vec![lent]),
        generics: Vec::new(),
    })
}

/// The slots in the source's text, each where one checked type goes: what the inbox takes, what
/// `launch_end` gives back on a failure, and what the source fails with.
const MESSAGE: &str = "Message__slot";
const ENDED: &str = "Ended__slot";
const FAILS: &str = "Fails__slot";

/// Puts each slot's type in, wherever a slot is in `def`: in its signature as a written type, and
/// in its code, as in `Sender[Message__slot](...)`, as the expression that names that type.
fn fill(def: &mut fennec_syntax::FnDef, slots: &[(&str, Type)]) -> Option<()> {
    let mut whole = true;
    for ty in def
        .ret
        .iter_mut()
        .chain(def.fails.iter_mut())
        .chain(def.params.iter_mut().filter_map(|param| param.ty.as_mut()))
    {
        whole &= fill_type(ty, slots).is_some();
    }
    fennec_syntax::visit::in_function(def, &mut |expr| {
        if let ExprKind::Name(name) = &expr.kind
            && let Some((_, ty)) = slots.iter().find(|(slot, _)| slot == name)
        {
            match ty.written(expr.span).and_then(|written| written.as_expr(expr.span)) {
                Some(named) => *expr = named,
                None => whole = false,
            }
        }
    });
    whole.then_some(())
}

/// Puts each slot's type in where a slot is in a written type, keeping a `?` written after it.
fn fill_type(ty: &mut fennec_syntax::TypeExpr, slots: &[(&str, Type)]) -> Option<()> {
    if let Some((_, filled)) = slots.iter().find(|(slot, _)| *slot == ty.name) {
        let optional = ty.optional;
        *ty = filled.written(ty.span)?;
        ty.optional |= optional;
        return Some(());
    }
    ty.args.iter_mut().try_for_each(|arg| fill_type(arg, slots))
}

/// A `launch(...)` call with its task taken out, leaving `launch`'s own options to give the
/// source written in for it.
pub(crate) fn options_only(call: &Expr) -> Expr {
    let mut options = call.clone();
    if let ExprKind::Call { args, .. } = &mut options.kind {
        args.retain(|arg| arg.name.is_some());
    }
    options
}

/// The text of the source written in for a `with launch(...)`, the same for every launch with an
/// inbox, and for every one without: a slot stands where each type goes (see `fill`). With an
/// inbox, a second function after it holds the inbox the task is given, made from the channel.
fn source_text(inbox: bool) -> String {
    // With an inbox, the channel is made first and closed and let go of where the block ends.
    let channel = |line: &str| inbox.then(|| line.to_owned());
    // The task stays as written, in the caller's code: its arrow is the closure started, with
    // its inbox written in, where the placeholder is. It's taken out of the call's arguments.
    let (params, lends, lent) = if inbox {
        (
            "stop_after: Duration? = None, inbox_size = 16",
            format!("Sender[{MESSAGE}]"),
            format!("Sender[{MESSAGE}](_channel=channel__made)"),
        )
    } else {
        ("stop_after: Duration? = None", "int".to_owned(), "0".to_owned())
    };
    let lines = [
        Some(format!("def launched__({params}) -> with {lends} fails {FAILS}:")),
        channel("    channel__made = intrinsics.channel_new(inbox_size)"),
        Some("    started__made = intrinsics.launch(task__placeholder)".to_owned()),
        // Above the `lend`, since its `except:` is written in at each place the block can fail.
        Some("    mut cut__made = 0".to_owned()),
        Some(format!("    lend {lent} except:")),
        channel("        intrinsics.channel_close(channel__made)"),
        Some(format!("        _ = intrinsics.launch_end[{ENDED}](started__made, -1, True, ref cut__made)")),
        channel("        intrinsics.channel_free(channel__made)"),
        channel("    intrinsics.channel_close(channel__made)"),
        Some("    mut nanos__made = -1".to_owned()),
        // A negative one has already run out: -1 would be no limit at all.
        Some("    if stop_after is not None: nanos__made = max(stop_after.nanos, 0)".to_owned()),
        Some("    mut why__made = 0".to_owned()),
        Some(format!(
            "    failed__made = intrinsics.launch_end[{ENDED}](started__made, nanos__made, False, ref why__made)"
        )),
        channel("    intrinsics.channel_free(channel__made)"),
        Some("    if why__made == 1: fail Crashed(why=\"the task crashed\")".to_owned()),
        Some("    if why__made == 2:".to_owned()),
        Some("        failure__made = failed__made else fail Crashed(why=\"the task crashed\")".to_owned()),
        Some("        fail failure__made".to_owned()),
        channel(""),
        channel(&format!("def inbox__made(channel__made: int) -> Inbox[{MESSAGE}]:")),
        channel(&format!("    return Inbox[{MESSAGE}](_channel=channel__made)")),
    ];
    lines.into_iter().flatten().map(|line| line + "\n").collect()
}

/// Writes the task's arrow in as the closure the source starts, where its placeholder is, with its
/// inbox, when it has one, as the receiving end of the channel the source made: `inbox`, which
/// reads `channel__made`.
fn with_task(def: &mut fennec_syntax::FnDef, launch: &Launch, inbox: Option<&Expr>) -> Option<()> {
    // The arrow, with its inbox written in, where the placeholder is.
    let ExprKind::Lambda { params, body } = &launch.task.kind else { return None };
    let mut called = body.as_ref().clone();
    if let (Some(name), Some(inbox)) = (params.first(), inbox) {
        fennec_syntax::visit::in_expr(&mut called, &mut |expr| {
            if matches!(&expr.kind, ExprKind::Name(named) if named == name) {
                *expr = inbox.clone();
            }
        });
    }
    let closure = Expr {
        kind: ExprKind::Lambda { params: Vec::new(), body: Box::new(called) },
        span: launch.task.span,
    };
    fennec_syntax::visit::in_function(def, &mut |expr| {
        if matches!(&expr.kind, ExprKind::Name(name) if name == "task__placeholder") {
            *expr = closure.clone();
        }
    });
    Some(())
}
