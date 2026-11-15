//! Places through a type's own `def []`, as `docs/decisions.md` sets them out. The `ref` form,
//! `def [](ref self, at: int)`, which the parser names `[]mut`, returns a place inside the value,
//! which indexing, `for ref` and a change go straight into. Through the plain form, `def [](self, key)` beside a
//! `def []=(ref self, key, value)`, a change reads the part with `[]`, changes that copy, and
//! writes it back with `[]=` once the change is done: once for `shelf[0].x += 1.0`, and once for
//! a call that changes it, like `shelf[0].shift(0.5)`.

use fennec_syntax::{FnDef, Span, StmtKind};

use super::calls::{ArgVal, Receiver};
use super::{Binding, Flow, Interp, Place, Run, Step, unsupported};
use crate::value::Value;

/// A part read through a type's own `def []` to be changed, held in a hidden local until it's
/// written back through the type's `def []=`.
pub(crate) struct WriteBack {
    /// Where the value whose part it is lies.
    owner: Place,
    /// The type of that value.
    ty: String,
    /// The key the part was read at.
    key: Value,
    /// The hidden local the part is held in while it's changed.
    part: Place,
}

/// Whether a method takes `self` as a `ref`.
pub(crate) fn takes_ref_self(def: &FnDef) -> bool {
    def.params.first().is_some_and(|param| param.is_self && param.is_mut)
}

impl Interp {
    /// The place `place[at]` names. Into a list, a tuple, a dict or a `Table` it's one step
    /// more. Through a type's `ref` `def []` it's the place that returns. Through the plain
    /// form it's no place to read, so `None`, and the value is read by calling `[]`; to change,
    /// with `changing`, it's a hidden local holding the part, written back when the change is
    /// done (`settle_write_backs`).
    pub(crate) fn indexed_place(
        &mut self,
        place: Place,
        at: Value,
        changing: bool,
        span: Span,
    ) -> Run<Option<Place>> {
        let own = self.peek_place(&place, span, |held| match held {
            Value::Instance(instance) => Some(instance.ty.clone()),
            _ => None,
        })?;
        let Some(ty) = own else { return Ok(Some(place.then(Step::At(at)))) };
        // A position is an int: a type may have the `ref` form for those beside a plain `def []`
        // for another kind of key, as `Grid` has for `(x, y)`.
        if matches!(at, Value::Int(_))
            && let Some(position) = self.program.method(&ty, "[]mut")
        {
            return self.own_position(place, position, at, span).map(Some);
        }
        let Some(getter) = self.program.method(&ty, "[]") else {
            return unsupported(format!("indexing a {ty}, which has no `def []`"));
        };
        if !changing {
            return Ok(None);
        }
        if self.program.method(&ty, "[]=").is_none() {
            return unsupported(format!("changing part of a {ty}, which has no `def []=`"));
        }
        let given = vec![ArgVal::Value(None, at.clone())];
        let part = self.call_def(getter, None, Some(Receiver::Place(place.clone())), given, span)?;
        self.parts_read += 1;
        let name = format!("\0part{}", self.parts_read);
        self.bind(&name, part)?;
        let held = Place { frame: self.cur, name, path: Vec::new() };
        self.write_backs.push(WriteBack { owner: place, ty, key: at, part: held.clone() });
        Ok(Some(held))
    }

    /// The place a type's `ref` `def []` returns for `at`: its body runs with `self` standing for
    /// `owner`, and its last line, which has to return a place, is read as one.
    fn own_position(&mut self, owner: Place, getter: &'static FnDef, at: Value, span: Span) -> Run<Place> {
        let Some((last, before)) = getter.body.split_last() else {
            return unsupported("a `ref` `def []` with nothing in it");
        };
        let StmtKind::Return(Some(returned)) = &last.kind else {
            return unsupported("a `ref` `def []` that doesn't end by returning a place");
        };
        let [_, param] = getter.params.as_slice() else {
            return unsupported("a `ref` `def []` that doesn't take one key");
        };
        let vars = vec![("self".to_owned(), Binding::Ref(owner)), (param.name.clone(), Binding::Value(at))];
        let previous = self.push_frame(vars, None)?;
        let ran = (|| {
            self.stmts(before)?;
            self.place_of(returned)
        })();
        self.pop_frame(previous);
        match ran {
            Ok(Some(place)) => Ok(place),
            Ok(None) => unsupported("a `ref` `def []` whose last line isn't a place"),
            Err(Flow::Return(_)) => unsupported("a `ref` `def []` that returns before its last line"),
            Err(other) => {
                let _ = span;
                Err(other)
            }
        }
    }

    /// The places of a value a `for ref` goes over through its type's own positions: from 0 up
    /// to its `len()`, each place its `ref` `def []` gives, but those its `is_live` says are
    /// empty.
    pub(crate) fn own_positions(&mut self, place: &Place, ty: &str, span: Span) -> Run<Vec<Value>> {
        if self.program.method(ty, "[]mut").is_none() {
            return unsupported(format!("`for ref` over a {ty}, which has no `ref` form of `def []`"));
        }
        let value = self.read_place(place, span)?;
        let count = self.length(value, span)?;
        Ok((0..count).map(Value::Int).collect())
    }

    /// Whether position `at` of the value at `place` holds an item: its type's `is_live`, where
    /// it has one, says so.
    pub(crate) fn live_at(&mut self, place: &Place, ty: &str, at: &Value, span: Span) -> Run<bool> {
        let Some(live) = self.program.method(ty, "is_live") else { return Ok(true) };
        let given = vec![ArgVal::Value(None, at.clone())];
        match self.call_def(live, None, Some(Receiver::Place(place.clone())), given, span)? {
            Value::Bool(live) => Ok(live),
            other => unsupported(format!("an `is_live` that gives a {}", other.kind())),
        }
    }

    /// The place an expression names, to be changed: as `place_of`, but through a type's plain
    /// `def []` it's a part read to be changed and written back.
    pub(crate) fn place_for_change(&mut self, expr: &'static fennec_syntax::Expr) -> Run<Option<Place>> {
        use fennec_syntax::ExprKind;
        match &expr.kind {
            ExprKind::Attr { object, name, optional: false } => {
                Ok(self.place_for_change(object)?.map(|place| place.then(Step::Field(name.clone()))))
            }
            ExprKind::Index { object, index } => {
                let Some(place) = self.place_for_change(object)? else { return Ok(None) };
                let at = self.eval(index)?;
                self.indexed_place(place, at, true, expr.span)
            }
            _ => self.place_of(expr),
        }
    }

    /// Ends the parts read since `mark`, the newest first, so one inside another is written into
    /// it before it's written back itself. With `write`, each goes back through its type's
    /// `def []=`; without, the change didn't happen, and nothing's written.
    pub(crate) fn settle_write_backs(&mut self, mark: usize, write: bool, span: Span) -> Run<()> {
        while self.write_backs.len() > mark {
            let Some(back) = self.write_backs.pop() else { break };
            let part = self.read_place(&back.part, span)?;
            if let Some(frame) = self.frames.get_mut(back.part.frame) {
                frame.vars.retain(|(name, _)| *name != back.part.name);
            }
            if !write {
                continue;
            }
            let Some(setter) = self.program.method(&back.ty, "[]=") else {
                return unsupported(format!("changing part of a {}, which has no `def []=`", back.ty));
            };
            let given = vec![ArgVal::Value(None, back.key), ArgVal::Value(None, part)];
            self.call_def(setter, None, Some(Receiver::Place(back.owner)), given, span)?;
        }
        Ok(())
    }
}

impl Interp {
    /// What a change through places read since `mark` came to: each part is written back when
    /// it went through. A failure partway, which a program can catch, is refused, since whether
    /// the runtime writes back what changed before it isn't promised; anything else that stopped
    /// it is passed on, with nothing written.
    pub(crate) fn settled<R>(&mut self, mark: usize, changed: Run<R>, span: Span) -> Run<R> {
        match changed {
            Ok(done) => {
                self.settle_write_backs(mark, true, span)?;
                Ok(done)
            }
            Err(Flow::Fail(..)) if self.write_backs.len() > mark => {
                self.settle_write_backs(mark, false, span)?;
                unsupported("a failure partway through a change through a type's own `def []=`")
            }
            Err(other) => {
                self.settle_write_backs(mark, false, span)?;
                Err(other)
            }
        }
    }
}
