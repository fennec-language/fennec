//! Positions: a type's `len` and the `mut` form of its `def []`, which `for mut` goes through.
//! The form's `return` names a place, so a position lowers to that place itself with no call:
//! its steps are added to those of what holds it, and the item is read and written back there.

use std::collections::HashMap;

use fennec_check::Type;
use fennec_syntax::{Expr, ExprKind, Span, Stmt, StmtKind};

use super::lists::Counting;
use super::places::{Leaving, Place, Step};
use super::{Loop, Lowered, Lowering, Tail};
use crate::{InstKind, LocalId, Operand, Terminator};

/// The name the loop's count goes by.
const POSITION: &str = "\0position";

impl Lowering<'_> {
    /// Whether a type has the `mut` form of `def []`.
    pub(super) fn has_mut_form(&self, owner: &str) -> bool {
        let base = self.context.base_key(&format!("{owner}.[]mut"));
        self.context.mut_forms.contains_key(&base)
    }

    /// The place that position `at` of what `holder` holds is. Lowering it runs the form's own
    /// asserts, then works out its indexes, with the types of the form's body.
    pub(super) fn position_place(
        &mut self,
        holder: Place,
        owner: &str,
        at: Operand,
        span: Span,
    ) -> Lowered<Place> {
        let context = self.context;
        let key = format!("{owner}.[]mut");
        let base = context.base_key(&key);
        let def =
            context.mut_forms.get(&base).ok_or_else(|| format!("`{owner}` has no `mut` form of `[]`"))?;
        let (checks, named) = names_a_place(&def.body)
            .ok_or_else(|| format!("the `mut` form of `{owner}.[]` does more than name a place"))?;
        let param = def.params.get(1).ok_or("the `mut` form of `[]` has no position")?;
        let body = context.signatures.types().body(&base);
        let body = body.ok_or_else(|| format!("the `mut` form of `{owner}.[]` has no types"))?;
        let subst = context.type_instance(owner).map(|copy| copy.subst).unwrap_or_default();
        let object = if holder.steps.is_empty() {
            holder.local
        } else {
            let read = self.read_steps(holder.local, &holder.steps, span)?;
            let Operand::Local(read) = read else { return Err("a value isn't in a local".to_owned()) };
            read
        };
        let position = self.temporary(Type::Int);
        self.emit(InstKind::Copy { dest: position, value: at }, span);
        let scope = HashMap::from([("self".to_owned(), object), (param.name.clone(), position)]);
        let outer_body = std::mem::replace(&mut self.body, body);
        let outer_scopes = std::mem::replace(&mut self.scopes, vec![scope]);
        let outer_subst = std::mem::replace(&mut self.subst, subst);
        let named = self.stmts(checks, Tail::Drop).and_then(|()| self.place_of(named));
        self.body = outer_body;
        self.scopes = outer_scopes;
        self.subst = outer_subst;
        let named = named?;
        if named.local != object {
            return Err("the `mut` form of `[]` names a place outside `self`".to_owned());
        }
        let mut steps = holder.steps;
        steps.extend(named.steps);
        Ok(Place { local: holder.local, steps })
    }

    /// Declares a loop's name holding what's at a place, read straight into it as a list's
    /// item is, so what it changes is the place's own once it's written back.
    fn read_into(&mut self, name: &str, place: &Place, held: Type, span: Span) -> Lowered<LocalId> {
        let (last, before) = place.steps.split_last().ok_or("the `mut` form of `[]` names `self`")?;
        let object = self.read_steps(place.local, before, span)?;
        let holder = Place { local: place.local, steps: before.to_vec() };
        let own_index = matches!(self.steps_type(&holder)?, Type::Named(_));
        let dest = self.declare(name, held);
        let kind = match last {
            Step::Field(field) => InstKind::GetField { dest, object, field: field.clone() },
            Step::Index(_) if own_index => {
                let value = self.read_steps(place.local, &place.steps, span)?;
                InstKind::Copy { dest, value }
            }
            Step::Index(index) => InstKind::GetIndex { dest, object, index: index.clone() },
        };
        self.emit(kind, span);
        Ok(dest)
    }

    /// `for mut name in iter`, on a type with positions: counts up to its `len`, skips what
    /// `is_live` says is empty, and has the item at each place, written back on leaving it.
    pub(super) fn through_positions(
        &mut self,
        name: &str,
        (iter, owner): (&Expr, &str),
        body: &[Stmt],
        span: Span,
    ) -> Lowered<()> {
        let holder = self.held_place(iter, span)?;
        let ty = self.steps_type(&holder)?;
        let (length, _) = self
            .resolve(&format!("{owner}.len"), &[Some(ty.clone())], None)
            .ok_or_else(|| format!("`for ref` over `{owner}` needs its `len`"))?;
        let held = self.read_steps(holder.local, &holder.steps, span)?;
        let limit = self.temporary(Type::Int);
        self.emit(InstKind::Call { dest: Some(limit), function: length, args: vec![held] }, span);
        let is_live = self.resolve(&format!("{owner}.is_live"), &[Some(ty), Some(Type::Int)], None);
        let names = [POSITION.to_owned()];
        let counting = Counting {
            names: &names,
            first: Operand::Int(0),
            limit: Operand::Local(limit),
            down: false,
            items: None,
            back: None,
        };
        self.counting(counting, span, |lowering| {
            let position = lowering.lookup(POSITION).ok_or("a loop lost its count")?;
            let outer = lowering.loops.last().ok_or("a loop lost its blocks")?;
            let (skip, leave) = (outer.next, outer.exit);
            if let Some((function, _)) = is_live {
                let held = lowering.read_steps(holder.local, &holder.steps, span)?;
                let live = lowering.temporary(Type::Bool);
                let args = vec![held, Operand::Local(position)];
                lowering.emit(InstKind::Call { dest: Some(live), function, args }, span);
                let then = lowering.new_block();
                let cond = Operand::Local(live);
                lowering.finish(Terminator::Branch { cond, then, otherwise: skip });
                lowering.current = then;
            }
            let place = lowering.position_place(holder.clone(), owner, Operand::Local(position), span)?;
            let held = lowering.steps_type(&place)?;
            let item = lowering.read_into(name, &place, held, span)?;
            let leaving = Leaving::Place { place, item };
            let (next, broke) = (lowering.new_block(), lowering.new_block());
            lowering.leaving.push(leaving.clone());
            lowering.loops.push(Loop { next, exit: broke, depth: lowering.leaving.len() });
            let lowered = lowering.stmts(body, Tail::Drop);
            lowering.loops.pop();
            lowering.leaving.pop();
            lowered?;
            lowering.finish(Terminator::Jump(next));
            lowering.current = next;
            lowering.undo(&leaving, span)?;
            lowering.finish(Terminator::Jump(skip));
            lowering.current = broke;
            lowering.undo(&leaving, span)?;
            lowering.finish(Terminator::Jump(leave));
            Ok(())
        })
    }
}

/// A `mut` form's asserts and the place it names, when that's all it does. Anything else, which
/// might change `self` on the way, gives `None`.
fn names_a_place(body: &[Stmt]) -> Option<(&[Stmt], &Expr)> {
    let (last, checks) = body.split_last()?;
    let StmtKind::Return(Some(named)) = &last.kind else { return None };
    let plain =
        checks.iter().all(|check| matches!(&check.kind, StmtKind::Assert(cond) if runs_nothing(cond)));
    (plain && runs_nothing(named)).then_some((checks, named))
}

/// Whether working an expression out only reads: names, fields, items, arithmetic and `len(x)`.
fn runs_nothing(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Name(_) | ExprKind::Int(_) => true,
        ExprKind::Attr { object, .. } => runs_nothing(object),
        ExprKind::Index { object, index } => runs_nothing(object) && runs_nothing(index),
        ExprKind::Binary { lhs, rhs, .. } => runs_nothing(lhs) && runs_nothing(rhs),
        ExprKind::Unary { expr, .. } => runs_nothing(expr),
        ExprKind::Call { callee, args } => {
            args.is_empty()
                && matches!(&callee.kind, ExprKind::Attr { object, name, .. } if name == "len" && runs_nothing(object))
        }
        _ => false,
    }
}
