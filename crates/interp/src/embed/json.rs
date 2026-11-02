//! `std.json` for native code. The value to write and the result read cross as temporaries,
//! the text as string blocks, and both go through the interpreter's own `json.dumps` and
//! `json.loads`, so they read and write exactly what it does.

use fennec_syntax::{Diagnostic, Item, Span};

use super::heap::text_address;
use super::{State, attempt, span};
use crate::call::CallArg;
use crate::value::Value;

/// An argument for one of the interpreter's functions.
fn argument(value: Value, at: Span) -> Vec<CallArg> {
    vec![CallArg { name: None, value, mut_place: None, literal: false, span: at }]
}

/// The text of a string block, failing as the runtime does when it isn't one.
fn text_of(state: &State, address: i64, at: Span) -> Result<String, Diagnostic> {
    let block = u64::try_from(address).ok().and_then(|address| state.heap.block(address));
    block
        .filter(|block| super::heap::is_text(state, block))
        .map(super::heap::block_text)
        .ok_or_else(|| Diagnostic::new("the native code lost a string", at))
}

/// `json.dumps(value)`, of a temporary: its text, as a string block owned once.
pub extern "C" fn dumps(value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let value = state.temp(value);
        match state.interp.format_builtin("json.dumps", argument(value, at), at)? {
            Value::Str(text) => Ok(text_address(state, &text)),
            other => Err(Diagnostic::new(format!("`json.dumps` gave a {}", other.type_name()), at)),
        }
    })
}

/// `json.loads[T](text)`, with `T` written out as in the program in the string block `ty`, or
/// `json.loads(text)` when `ty` is 0: a temporary holding the result, `Ok` with what was read
/// or `Err` with a `ParseError`.
pub extern "C" fn loads(text: i64, ty: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let read = Value::str(&text_of(state, text, at)?);
        let result = if ty == 0 {
            state.interp.format_builtin("json.loads", argument(read, at), at)?
        } else {
            let ty = type_named(&text_of(state, ty, at)?, at)?;
            state.interp.typed_loads("json.loads", &ty, argument(read, at), at)?
        };
        Ok(state.push(result))
    })
}

/// A type written out as in a program, like `list[Save]`.
fn type_named(written: &str, at: Span) -> Result<fennec_syntax::TypeExpr, Diagnostic> {
    let module = fennec_syntax::parse(&format!("type Loaded = {written}\n"))?;
    module
        .items
        .into_iter()
        .find_map(|item| match item {
            Item::TypeAlias { ty, .. } => Some(ty),
            _ => None,
        })
        .ok_or_else(|| Diagnostic::new(format!("`{written}` isn't a type"), at))
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, strings, with};
    use crate::packed::Element;
    use crate::value::Value;

    fn text_of(address: i64) -> String {
        with(|state| {
            let block = u64::try_from(address).ok().and_then(|at| state.heap.block(at));
            block.map_or_else(String::new, heap::block_text)
        })
    }

    fn temp(at: i64) -> Value {
        with(|state| Some(state.temp(at))).unwrap()
    }

    #[test]
    fn json_is_written_and_read_as_the_interpreter_does() {
        let module = fennec_syntax::parse("struct Point:\n    x: int\n    y: int\n").unwrap();
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&module, tables, crate::Inputs::default()).unwrap();
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let numbers = with(|state| state.push(Value::list(vec![Value::Int(1), Value::Int(2)])));
        assert_eq!(text_of(super::dumps(numbers, 0, 0)), "[1, 2]");
        let read = super::loads(string("{\"x\": 3, \"y\": 4}"), string("Point"), 0, 0);
        let Value::Ok(point) = temp(read) else { panic!("it reads") };
        let point = with(|state| state.push(point.as_ref().clone()));
        assert_eq!(text_of(super::dumps(point, 0, 0)), "{\"x\": 3, \"y\": 4}");
        let read = super::loads(string("[5, 6]"), string("list[int]"), 0, 0);
        assert_eq!(temp(read), Value::Ok(Rc::new(Value::list(vec![Value::Int(5), Value::Int(6)]))));
        let untyped = super::loads(string("[true]"), 0, 0, 0);
        assert!(matches!(temp(untyped), Value::Ok(_)));
        finish();
    }

    #[test]
    fn strings_have_the_interpreter_s_methods() {
        let str = Rc::new(Element::Str);
        let tables = Tables { shapes: vec![Rc::clone(&str)], elements: vec![str], ..Tables::default() };
        start(&fennec_syntax::Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let fox = string("The quick Fox");
        assert_eq!(text_of(strings::upper(fox)), "THE QUICK FOX");
        assert_eq!(text_of(strings::lower(fox)), "the quick fox");
        assert_eq!(text_of(strings::replace(fox, string("quick"), string("slow"))), "The slow Fox");
        assert_eq!(strings::find(fox, string("Fox")), 10);
        assert_eq!(strings::find(fox, string("cat")), -1);
        let words = strings::split(fox, 0, 0, 0);
        let list = Element::List(Rc::new(Element::Str));
        let words = u64::try_from(words).unwrap();
        let parts = with(|state| Some(heap::read_word(state, &list, words))).unwrap();
        assert_eq!(parts, Value::list(["The", "quick", "Fox"].into_iter().map(Value::str).collect()));
        assert_eq!(strings::split(fox, string(""), 0, 0), 0);
        let (_, failure, _) = finish();
        assert!(failure.is_some());
    }
}
