//! Moving a test out of a loop. When a small inner loop tests a bool that can't change, the
//! loop is lowered twice, once for each value, with the test gone from both copies.

use std::collections::HashMap;

use cranelift_codegen::entity::EntityRef;
use cranelift_codegen::ir::InstBuilder;
use fennec_syntax::{Diagnostic, Expr, ExprKind, Stmt, StmtKind, UnaryOp};

use super::Lowerer;
use super::hoist::{each_stmt, each_stmt_expr};
use crate::types::Ty;

/// How many expressions a loop body may have and still be lowered twice.
const MOST_EXPRS: usize = 64;

/// The name a test reads, if all it does is read a name or that name's opposite.
fn tested(cond: &Expr) -> Option<&str> {
    match &cond.kind {
        ExprKind::Name(name) => Some(name),
        ExprKind::Unary { op: UnaryOp::Not, expr } => tested(expr),
        _ => None,
    }
}

impl Lowerer<'_, '_> {
    /// What a test comes to in the copy of a loop being lowered, if it's known.
    pub(super) fn known_test(&self, cond: &Expr) -> Option<bool> {
        match &cond.kind {
            ExprKind::Name(name) => {
                let var = self.local(name)?.var?;
                self.flags.get(&var.index()).copied()
            }
            ExprKind::Unary { op: UnaryOp::Not, expr } => self.known_test(expr).map(|known| !known),
            _ => None,
        }
    }

    /// A bool that can't change which an inner loop's body tests, if the body's small enough
    /// to lower twice.
    fn flag_to_unswitch(&self, body: &[Stmt]) -> Option<String> {
        if !self.flags.is_empty() {
            return None;
        }
        let (mut exprs, mut has_loop) = (0, false);
        let mut flag = None;
        let consider = |cond: &Expr| {
            let name = tested(cond)?;
            let local = self.local(name)?;
            (local.ty == Ty::Bool && !local.is_mut && local.var.is_some()).then(|| name.to_owned())
        };
        for stmt in body {
            each_stmt(stmt, &mut |stmt| match &stmt.kind {
                StmtKind::While { .. } | StmtKind::For { .. } => has_loop = true,
                StmtKind::If { branches, .. } => {
                    for (cond, _) in branches {
                        flag = flag.take().or_else(|| consider(cond));
                    }
                }
                _ => {}
            });
            each_stmt_expr(stmt, &mut |expr| {
                exprs += 1;
                if let ExprKind::Cond { cond, .. } = &expr.kind {
                    flag = flag.take().or_else(|| consider(cond));
                }
            });
        }
        flag.filter(|_| !has_loop && exprs <= MOST_EXPRS)
    }

    /// Lowers a loop by `lower`, in two copies if its body tests a bool that can't change.
    /// Gives back false, having done nothing, if it doesn't.
    pub(super) fn unswitched(
        &mut self,
        body: &[Stmt],
        lower: &mut dyn FnMut(&mut Self) -> Result<(), Diagnostic>,
    ) -> Result<bool, Diagnostic> {
        let Some(name) = self.flag_to_unswitch(body) else { return Ok(false) };
        let Some(var) = self.local(&name).and_then(|local| local.var) else { return Ok(false) };
        let test = self.read_var(var);
        let [yes, no, done] = [(); 3].map(|()| self.b.create_block());
        self.b.ins().brif(test, yes, &[], no, &[]);
        for (block, value) in [(yes, true), (no, false)] {
            self.b.switch_to_block(block);
            self.b.seal_block(block);
            self.flags.insert(var.index(), value);
            let lowered = lower(self);
            self.flags.clear();
            lowered?;
            self.drop_temps()?;
            if !self.is_dead() {
                self.b.ins().jump(done, &[]);
            }
        }
        self.b.switch_to_block(done);
        Ok(true)
    }
}

/// The bools known in the copy of a loop being lowered, by their variables.
pub(super) type Flags = HashMap<usize, bool>;
