//! Packed lists for native code: packing a name's list, and the address and length of its
//! items, which native code then reads and changes directly. An address stays good until
//! the list grows, or something else gets hold of the list, so native code asks again
//! after anything that could do either.

use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::{State, attempt, span, with};
use crate::packed::Packed;
use crate::value::Value;

/// The packed list a name holds, packing it first if it holds an ordinary list of the
/// right items.
fn packed_mut(state: &mut State, name: i64, element: Option<i64>) -> Option<&mut Rc<Packed>> {
    let element =
        element.and_then(|at| usize::try_from(at).ok()).and_then(|at| state.tables.elements.get(at)).cloned();
    let name = State::name_ref(&state.tables, name);
    let binding = state.interp.frame.get_mut(name)?;
    if let (Value::List(items), Some(element)) = (&binding.value, element) {
        binding.value = Value::Packed(Rc::new(Packed::pack(&element, items)?));
    }
    match &mut binding.value {
        Value::Packed(packed) => Some(packed),
        _ => None,
    }
}

/// Packs the list a name holds, when every item is of the kind `element` describes. Gives
/// back whether it's packed now.
pub extern "C" fn pack(name: i64, element: i64) -> i8 {
    with(|state| i8::from(packed_mut(state, name, Some(element)).is_some()))
}

/// A new packed list of `count` items of a kind, all zero, as a temporary, for native code
/// to fill.
pub extern "C" fn zeroed(element: i64, count: i64) -> i64 {
    with(|state| {
        let element = usize::try_from(element).ok().and_then(|at| state.tables.elements.get(at)).cloned();
        let Some(element) = element else { return state.push(Value::list(Vec::new())) };
        let words = vec![0; usize::try_from(count).unwrap_or(0) * element.words()];
        state.push(Value::Packed(Rc::new(Packed { element, words })))
    })
}

/// Where a name's packed items start, for reading (`write` 0) or changing them (`write` 1,
/// which first gives the list memory of its own if it shares it).
pub extern "C" fn address(name: i64, element: i64, write: i8) -> i64 {
    attempt(|state: &mut State| {
        let packed = packed_mut(state, name, Some(element)).ok_or_else(not_packed)?;
        let words = if write != 0 {
            Rc::make_mut(packed).words.as_mut_ptr()
        } else {
            packed.words.as_ptr().cast_mut()
        };
        Ok(i64::try_from(words.addr()).unwrap_or(0))
    })
}

/// How many items a name's packed list has.
pub extern "C" fn length(name: i64, element: i64) -> i64 {
    attempt(|state: &mut State| {
        let packed = packed_mut(state, name, Some(element)).ok_or_else(not_packed)?;
        Ok(i64::try_from(packed.len()).unwrap_or(0))
    })
}

/// Adds an item's worth of zeroed memory to the end of a name's packed list, for native
/// code to fill, and gives back where it starts.
pub extern "C" fn push(name: i64, element: i64) -> i64 {
    attempt(|state: &mut State| {
        let packed = Rc::make_mut(packed_mut(state, name, Some(element)).ok_or_else(not_packed)?);
        let start = packed.words.len();
        packed.words.resize(start + packed.element.words(), 0);
        let item = packed.words.get_mut(start..).map_or(std::ptr::null_mut(), <[u64]>::as_mut_ptr);
        Ok(i64::try_from(item.addr()).unwrap_or(0))
    })
}

/// Fails with the interpreter's message for an index past either end of a list.
pub extern "C" fn out_of_range(at: i64, len: i64, start: i64, end: i64) {
    attempt(|_: &mut State| -> Result<(), Diagnostic> {
        Err(Diagnostic::new(format!("index {at} is out of range for length {len}"), span(start, end)))
    });
}

fn not_packed() -> Diagnostic {
    Diagnostic::new("the JIT expected a packed list here", fennec_syntax::Span::default())
        .with_help("this is a bug in Fennec; run without `--jit` and please report it")
}
