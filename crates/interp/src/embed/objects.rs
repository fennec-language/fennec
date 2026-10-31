//! Direct operations on objects: reading fields, operators, indexing, telling types apart for
//! method calls, and changing a part of a name in place. Each is one call, with no syntax to
//! build or walk.

use fennec_syntax::{BinOp, Diagnostic, UnaryOp};

use super::{State, attempt, span, with};
use crate::place::{Place, Step};
use crate::value::Value;

/// The operators native code passes by number, in order.
const BINARY: [BinOp; 17] = [
    BinOp::Add,
    BinOp::Sub,
    BinOp::Mul,
    BinOp::Div,
    BinOp::FloorDiv,
    BinOp::Mod,
    BinOp::Pow,
    BinOp::Eq,
    BinOp::NotEq,
    BinOp::Lt,
    BinOp::LtEq,
    BinOp::Gt,
    BinOp::GtEq,
    BinOp::In,
    BinOp::NotIn,
    BinOp::Is,
    BinOp::IsNot,
];

/// The number native code passes for an operator, or `None` for `and` and `or`, which
/// native code always does itself.
pub fn binary_code(op: BinOp) -> Option<i64> {
    BINARY.iter().position(|known| *known == op).and_then(|at| i64::try_from(at).ok())
}

/// `a op b`, using the type's own definition (like `def +`) when it has one.
pub extern "C" fn binary(op: i64, a: i64, b: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        // A native value is never None, so `is` needn't look inside one.
        let tested = (Some(op) == binary_code(BinOp::Is), Some(op) == binary_code(BinOp::IsNot));
        if tested.0 || tested.1 {
            let (left, right) = (state.raw(a), state.raw(b));
            if let (Value::Lent(_), Value::None) | (Value::None, Value::Lent(_)) = (&left, &right) {
                return Ok(state.push(Value::Bool(tested.1)));
            }
        }
        let op = usize::try_from(op).ok().and_then(|at| BINARY.get(at)).copied();
        let op = op.ok_or_else(|| Diagnostic::new("the JIT passed an unknown operator", span(start, end)))?;
        let (a, b) = (state.take(a), state.take(b));
        let value = state.interp.binary(op, a, b, span(start, end))?;
        Ok(state.push(value))
    })
}

/// `-value` (`negate` 1) or `not value` (`negate` 0).
pub extern "C" fn unary(negate: i64, value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let op = if negate != 0 { UnaryOp::Neg } else { UnaryOp::Not };
        let value = crate::ops::unary(op, state.take(value), span(start, end))?;
        Ok(state.push(value))
    })
}

/// `object.name`, or `object?.name` when `optional`.
pub extern "C" fn field(object: i64, name: i64, optional: i8, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let raw = state.raw(object);
        if let Some(field) = super::heap::lent_field(state, &raw, name) {
            return Ok(state.push(field));
        }
        let object = state.take(object);
        let name = State::name_ref(&state.tables, name);
        let value = state.interp.attr(object, name, optional != 0, span(start, end))?;
        Ok(state.push(value))
    })
}

/// `object[at]`.
pub extern "C" fn index(object: i64, at: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let (object, at) = (state.take(object), state.take(at));
        let value = state.interp.index(object, at, span(start, end))?;
        Ok(state.push(value))
    })
}

/// The name of an object's type, by its number in the name table, or -1 when it isn't an
/// instance of a struct, enum or error, or its type's name isn't in the table.
pub extern "C" fn type_of(value: i64) -> i64 {
    with(|state| {
        let raw = state.raw(value);
        if let Some(name) = super::heap::lent_type(state, &raw) {
            return state.name_ids.get(name.as_ref()).copied().unwrap_or(-1);
        }
        match state.expanded(raw) {
            Value::Instance(instance) => {
                state.name_ids.get(instance.type_name.as_ref()).copied().unwrap_or(-1)
            }
            _ => -1,
        }
    })
}

/// Starts a path into a name, like `world.player`, for `put`.
pub extern "C" fn path(root: i64) {
    with(|state| {
        let root = state.name(root);
        state.path = Some(Place { root, steps: Vec::new(), span: fennec_syntax::Span::default() });
    });
}

/// Adds a field to the path.
pub extern "C" fn path_field(name: i64) {
    with(|state| {
        let name = state.name(name);
        if let Some(place) = &mut state.path {
            place.steps.push(Step::Field(name));
        }
    });
}

/// Adds an index to the path.
pub extern "C" fn path_index(at: i64) {
    with(|state| {
        let at = state.take(at);
        if let Some(place) = &mut state.path {
            place.steps.push(Step::Index(at));
        }
    });
}

/// The value at the end of a path of fields, like `self.position.x`, copying only that
/// value: copying the whole name would make the next change to it copy it again.
pub extern "C" fn path_get(start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let place =
            state.path.take().ok_or_else(|| Diagnostic::new("the JIT lost a path", span(start, end)))?;
        let binding = state.interp.frame.get(&place.root);
        let mut current = binding.map(|binding| &binding.value);
        let mut rest = place.steps.as_slice();
        while let (Some(Value::Instance(instance)), Some((Step::Field(name), after))) =
            (current, rest.split_first())
        {
            match instance.field(name) {
                Some(value) => {
                    current = Some(value);
                    rest = after;
                }
                None => break,
            }
        }
        let mut value = current.cloned().unwrap_or(Value::Unit);
        for step in rest {
            value = match step {
                Step::Field(name) => state.interp.attr(value, name, false, span(start, end))?,
                Step::Index(at) => state.interp.index(value, at.clone(), span(start, end))?,
            };
        }
        Ok(state.push(value))
    })
}

/// Puts a value at the end of the path, changing that part of the name in place.
pub extern "C" fn put(value: i64, start: i64, end: i64) {
    attempt(|state: &mut State| {
        let value = state.take(value);
        let mut place =
            state.path.take().ok_or_else(|| Diagnostic::new("the JIT lost a path", span(start, end)))?;
        place.span = span(start, end);
        state.interp.put(&place, value)
    });
}

/// After one turn of `for mut`, puts the item back where it came from: at `key` along the
/// path, its index or dict key. Items without a key, like a set's, aren't put back.
pub extern "C" fn put_item(key: i64, value: i64, start: i64, end: i64) {
    attempt(|state: &mut State| {
        let key = state.take(key);
        let mut place =
            state.path.take().ok_or_else(|| Diagnostic::new("the JIT lost a path", span(start, end)))?;
        if matches!(key, Value::None) {
            return Ok(());
        }
        let value = state.take(value);
        place.steps.push(Step::Index(key));
        place.span = span(start, end);
        state.interp.put(&place, value)
    });
}

/// The `(key, item)` pairs a `for mut` loop walks through, as a list. The key is None for
/// items that have none, like a set's.
pub extern "C" fn entries(value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let value = state.temp(value);
        let entries = state.interp.entries(value, span(start, end))?;
        let pairs = entries
            .into_iter()
            .map(|(key, item)| Value::tuple(vec![key.unwrap_or(Value::None), item]))
            .collect();
        Ok(state.push(Value::list(pairs)))
    })
}

/// A struct made from field values native code computed: the last `count` inputs handed
/// over, in the order the struct's fields are written. Native code only makes structs that
/// have no `where` rules this way, so there's nothing to check.
pub extern "C" fn make_instance(type_name: i64, count: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let type_name = State::name_ref(&state.tables, type_name).to_owned();
        let info = state.interp.type_info(&type_name).ok_or_else(|| {
            Diagnostic::new(format!("the JIT lost the type `{type_name}`"), span(start, end))
        })?;
        let count = usize::try_from(count).unwrap_or(0);
        let values = state.inputs.split_off(state.inputs.len().saturating_sub(count));
        let fields =
            info.def.fields.iter().map(|field| std::rc::Rc::from(field.name.as_str())).zip(values).collect();
        let instance = crate::value::Instance {
            type_name: std::rc::Rc::from(type_name.as_str()),
            case: None,
            fields,
            is_error: false,
            cause: None,
        };
        Ok(state.push(Value::Instance(std::rc::Rc::new(instance))))
    })
}

/// An enum case made from field values native code computed: the last `count` inputs
/// handed over, in the order the case's fields are written.
pub extern "C" fn make_variant(type_name: i64, case: i64, count: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let type_name = State::name_ref(&state.tables, type_name).to_owned();
        let case = State::name_ref(&state.tables, case).to_owned();
        let lost =
            || Diagnostic::new(format!("the JIT lost the case `{type_name}.{case}`"), span(start, end));
        let info = state.interp.type_info(&type_name).ok_or_else(lost)?;
        let written = info.def.cases.iter().find(|written| written.name == case).ok_or_else(lost)?;
        let count = usize::try_from(count).unwrap_or(0);
        let values = state.inputs.split_off(state.inputs.len().saturating_sub(count));
        let fields =
            written.fields.iter().map(|field| std::rc::Rc::from(field.name.as_str())).zip(values).collect();
        let instance = crate::value::Instance {
            type_name: std::rc::Rc::from(type_name.as_str()),
            case: Some(std::rc::Rc::from(case.as_str())),
            fields,
            is_error: false,
            cause: None,
        };
        Ok(state.push(Value::Instance(std::rc::Rc::new(instance))))
    })
}

/// Which case of an enum a value is, by its position in the enum's definition, failing
/// when it isn't a value of that enum.
pub extern "C" fn case_of(value: i64, type_name: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let type_name = State::name_ref(&state.tables, type_name);
        let found = match state.temp(value) {
            Value::Instance(instance) if instance.type_name.as_ref() == type_name => {
                instance.case.as_ref().and_then(|case| {
                    let info = state.interp.type_info(type_name)?;
                    info.def.cases.iter().position(|written| written.name == case.as_ref())
                })
            }
            _ => None,
        };
        let found = found.and_then(|at| i64::try_from(at).ok());
        found.ok_or_else(|| Diagnostic::new(format!("expected a `{type_name}` here"), span(start, end)))
    })
}
