//! `with` over a source the program writes, and its `lend`. The source runs until it lends; the
//! caller's block runs there, in the caller's frame, with what was lent; then the source carries
//! on, after its `lend` if the block ended well, or in its `lend ... except:` block if it failed.

use fennec_syntax::{Expr, ExprKind, Span, Stmt, StmtKind, WithName};

use super::calls::Target;
use super::{Flow, Interp, Run, unsupported};
use crate::value::Value;

/// A `with` whose source is running: what its `lend` hands the caller's block, and how the
/// block ended.
pub(crate) struct Lending {
    /// The frame of the source's call, where its `lend` is.
    source_frame: usize,
    /// The frame of the `with`, where its block runs.
    caller_frame: usize,
    /// The handles the `with` opens, and the names each lends to.
    handles: Vec<(&'static Expr, &'static [WithName])>,
    /// Which handle this is.
    level: usize,
    body: &'static [Stmt],
    lent: bool,
    /// A `return`, `break` or `continue` the block ended with, which leaves the `with` once the
    /// source has finished.
    pending: Option<Flow>,
}

impl Interp {
    pub(crate) fn with(
        &mut self,
        value: &'static Expr,
        names: &'static [WithName],
        body: &'static [Stmt],
    ) -> Run<()> {
        let handles: Vec<(&'static Expr, &'static [WithName])> = match &value.kind {
            ExprKind::Tuple(items) if items.len() == names.len() && items.len() > 1 => {
                items.iter().zip(names.chunks(1)).collect()
            }
            _ => vec![(value, names)],
        };
        self.open(handles, 0, body)
    }

    fn open(
        &mut self,
        handles: Vec<(&'static Expr, &'static [WithName])>,
        level: usize,
        body: &'static [Stmt],
    ) -> Run<()> {
        let Some((handle, _)) = handles.get(level).copied() else {
            return unsupported("a `with` with no handle");
        };
        let call = match &handle.kind {
            ExprKind::Try { value, context: None } => value,
            _ => handle,
        };
        let ExprKind::Call { callee, args } = &call.kind else {
            return unsupported("a `with` over something that isn't a call");
        };
        let (target, given) = self.prepare(call, callee, args)?;
        match &target {
            Target::Def { def, .. } if lends_in(&def.body) => {}
            Target::Def { def, .. } => {
                return unsupported(format!("a `with` over `{}`, which doesn't lend", def.name));
            }
            _ => return unsupported("a `with` over the standard library's sources"),
        }
        self.lends.push(Lending {
            source_frame: self.frames.len(),
            caller_frame: self.cur,
            handles,
            level,
            body,
            lent: false,
            pending: None,
        });
        let ran = self.invoke(target, given, call);
        let Some(lending) = self.lends.pop() else { return unsupported("a `with` lost its source") };
        match ran {
            Err(Flow::Abandoned) => {
                self.shared.refuse("a generator let go of inside a `with` block".to_owned());
                Err(Flow::Abandoned)
            }
            Err(other) => Err(other),
            Ok(_) if !lending.lent => unsupported("a `with` source that ended without lending"),
            Ok(_) => lending.pending.map_or(Ok(()), Err),
        }
    }

    /// `lend value`: runs the caller's block with `value`, then carries on, or runs the cleanup
    /// in `otherwise` and passes the block's failure on.
    pub(crate) fn lend(&mut self, value: &'static Expr, otherwise: &'static [Stmt], span: Span) -> Run<()> {
        let here = self.cur;
        let Some(at) = self.lends.iter().rposition(|lending| lending.source_frame == here && !lending.lent)
        else {
            return unsupported("`lend` outside a `with`");
        };
        let lent = self.eval(value)?;
        let Some(lending) = self.lends.get_mut(at) else { return unsupported("a `with` lost its source") };
        lending.lent = true;
        let (caller, handles, level, body) =
            (lending.caller_frame, lending.handles.clone(), lending.level, lending.body);
        let names = handles.get(level).map_or(&[][..], |(_, names)| *names);
        self.cur = caller;
        let ran = self.lend_names(names, lent, span).and_then(|()| {
            if level + 1 < handles.len() { self.open(handles, level + 1, body) } else { self.block(body) }
        });
        self.cur = here;
        match ran {
            Ok(()) => Ok(()),
            Err(flow @ (Flow::Return(_) | Flow::Break | Flow::Continue)) => {
                if let Some(lending) = self.lends.get_mut(at) {
                    lending.pending = Some(flow);
                }
                Ok(())
            }
            Err(Flow::Fail(error, at_span)) => {
                match self.block(otherwise) {
                    Err(Flow::Crash(message, crashed)) => return Err(Flow::Crash(message, crashed)),
                    Err(Flow::Unsupported(why)) => return Err(Flow::Unsupported(why)),
                    _ => {}
                }
                Err(Flow::Fail(error, at_span))
            }
            Err(Flow::Crash(message, crashed)) => {
                if let Err(Flow::Unsupported(why)) = self.block(otherwise) {
                    return Err(Flow::Unsupported(why));
                }
                Err(Flow::Crash(message, crashed))
            }
            Err(Flow::Abandoned) => {
                self.shared.refuse("a generator let go of inside a `with` block".to_owned());
                Err(Flow::Abandoned)
            }
            Err(other) => Err(other),
        }
    }

    fn lend_names(&mut self, names: &'static [WithName], lent: Value, span: Span) -> Run<()> {
        match names {
            [] => Ok(()),
            [name] => self.set_name(&name.name, lent, span),
            several => match lent {
                Value::Tuple(parts) if parts.len() == several.len() => {
                    for (name, part) in several.iter().zip(parts) {
                        self.set_name(&name.name, part, span)?;
                    }
                    Ok(())
                }
                _ => unsupported("a `with` taking apart what isn't a tuple"),
            },
        }
    }
}

/// Whether a function's body lends, which makes it a `with` source.
fn lends_in(body: &[Stmt]) -> bool {
    body.iter().any(|stmt| match &stmt.kind {
        StmtKind::Lend { .. } => true,
        StmtKind::If { branches, otherwise } => {
            branches.iter().any(|(_, body)| lends_in(body)) || otherwise.as_deref().is_some_and(lends_in)
        }
        StmtKind::With { body, .. } | StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
            lends_in(body)
        }
        _ => false,
    })
}
