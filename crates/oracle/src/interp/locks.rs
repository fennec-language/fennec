//! `shared` values and `lock`. A `shared` value is one cell, which every copy of it reaches. A
//! `lock` copies what each cell holds into the name it gives it, runs its block, and once the
//! block ends writes back what each `ref` name holds, unless a failure or a crash left it, which
//! undoes the block's changes, as the language says. Nothing else reaches a cell while a `lock`
//! holds it: a `lock` inside another is a compile error, and only one thing runs at a time here,
//! so copying in and writing back is exact. One that's reached anyway is refused.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use fennec_syntax::{Expr, ExprKind, Span, Stmt, WithName};

use super::{Binding, Flow, Interp, Run, unsupported};
use crate::value::{Cell, Value};

impl Interp {
    /// `shared name = value`: `name` is a new cell, holding the value.
    pub(crate) fn declare_shared(&mut self, name: &str, value: &'static Expr, span: Span) -> Run<()> {
        let held = self.eval(value)?;
        let made = Value::Shared(Arc::new(Cell::new(self.shared.new_cell(), held)));
        self.set_name(name, made, span)
    }

    /// `lock a, b as ref x, y:`: each name holds what its value holds, for the block, and a `ref`
    /// name's changes are kept when it ends. With `valued`, the block's last line is its value, as
    /// after `x =`.
    pub(crate) fn lock(
        &mut self,
        value: &'static Expr,
        names: &'static [WithName],
        body: &'static [Stmt],
        valued: bool,
    ) -> Run<Value> {
        let items: Vec<&'static Expr> = match &value.kind {
            ExprKind::Tuple(items) if names.len() > 1 => items.iter().collect(),
            _ => vec![value],
        };
        if items.len() != names.len() {
            return unsupported("a `lock` naming another number of values than it takes");
        }
        let mut cells: Vec<Arc<Cell>> = Vec::with_capacity(items.len());
        for item in items {
            match self.eval(item)? {
                Value::Shared(cell) => cells.push(cell),
                other => return unsupported(format!("a `lock` on a {}", other.kind())),
            }
        }
        for (at, cell) in cells.iter().enumerate() {
            if cells.iter().take(at).any(|earlier| earlier.id == cell.id) {
                return unsupported("one `shared` value locked twice in one `lock`");
            }
        }
        for (cell, name) in cells.iter().zip(names) {
            self.note_lock(cell.id, name.is_mut)?;
        }
        let mut taken = 0;
        for cell in &cells {
            if cell.locked.swap(true, Ordering::SeqCst) {
                break;
            }
            taken += 1;
        }
        let ran = if taken == cells.len() {
            self.holding += 1;
            let ran = self.locked(&cells, names, body, valued);
            self.holding -= 1;
            ran
        } else {
            unsupported("a `lock` on a `shared` value a `lock` holds")
        };
        for cell in cells.iter().take(taken) {
            cell.locked.store(false, Ordering::SeqCst);
        }
        ran
    }

    /// Runs a `lock`'s block with each name holding a copy of what its cell holds, and writes the
    /// `ref` names' back, unless the block failed or crashed.
    fn locked(
        &mut self,
        cells: &[Arc<Cell>],
        names: &'static [WithName],
        body: &'static [Stmt],
        valued: bool,
    ) -> Run<Value> {
        let mark = self.mark();
        let ran = self.bind_held(cells, names).map(|slots| {
            let ran = if valued { self.stmts_value(body) } else { self.stmts(body).map(|()| Value::Unit) };
            (slots, ran)
        });
        let (slots, ran) = match ran {
            Ok(both) => both,
            Err(flow) => {
                self.restore(mark);
                return Err(flow);
            }
        };
        // A failure or a crash leaving the block undoes its changes; any other way out keeps them.
        let keeps = matches!(ran, Ok(_) | Err(Flow::Return(_) | Flow::Break | Flow::Continue));
        let mut kept = Vec::new();
        if keeps {
            for ((cell, name), slot) in cells.iter().zip(names).zip(&slots) {
                if let (true, Some(slot)) = (name.is_mut, slot) {
                    let Some((_, Binding::Value(held))) =
                        self.frames.get(mark.0).and_then(|frame| frame.vars.get(*slot))
                    else {
                        self.restore(mark);
                        return unsupported("a `lock`'s name that's gone");
                    };
                    kept.push((cell, held.clone()));
                }
            }
        }
        self.restore(mark);
        for (cell, held) in kept {
            let Ok(mut inside) = cell.held.lock() else { return unsupported("a `shared` value was lost") };
            *inside = held;
        }
        ran
    }

    /// Gives each name a copy of what its cell holds, saying where in the frame each went.
    fn bind_held(&mut self, cells: &[Arc<Cell>], names: &'static [WithName]) -> Run<Vec<Option<usize>>> {
        let mut slots = Vec::with_capacity(names.len());
        for (cell, name) in cells.iter().zip(names) {
            let held = match cell.held.lock() {
                Ok(held) => held.clone(),
                Err(_) => return unsupported("a `shared` value was lost"),
            };
            self.charge(&held)?;
            let slot = self.frames.get(self.cur).map(|frame| frame.vars.len());
            slots.push(slot.filter(|_| name.name != "_"));
            self.bind(&name.name, held)?;
        }
        Ok(slots)
    }
}
